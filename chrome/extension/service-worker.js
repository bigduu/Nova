import {
  CONTENT_TIMEOUT_MS,
  PAIR_TTL_MS,
  PROTOCOL_VERSION,
  ProtocolError,
  assertWireSize,
  isOpaqueId,
  randomOpaqueId,
  redactForDiagnostic,
  sameRoute,
  validateReceipt,
  validateRequest,
} from "./lib/protocol.js";
import { RouterState } from "./lib/router-state.js";

const CHANNEL = "nova-extension-v1";
const NATIVE_HOST = "com.zenith.nova.chrome";
const PACKAGED_SCRIPTS = ["lib/semantic-runtime.js", "content-script.js"];
const state = new RouterState();
let nativePort = null;
let reconnectTimer = null;
let reconnectDelay = 1000;
let pairTimer = null;
let pairingCandidate = null;
let pairingCandidateDraftRoute = null;
let pairingCandidateRevision = 0;

function invalidatePairingCandidate() {
  pairingCandidate = null;
  pairingCandidateDraftRoute = null;
  pairingCandidateRevision += 1;
}

function pairingCandidateRoute() {
  return pairingCandidate?.route ?? pairingCandidateDraftRoute;
}

function clearCandidateDraft(route, revision) {
  if (
    revision === pairingCandidateRevision &&
    sameRoute(pairingCandidateDraftRoute, route, false)
  ) {
    pairingCandidateDraftRoute = null;
  }
}

function expirePendingPair() {
  invalidatePairingCandidate();
  clearTimeout(pairTimer);
  const response = state.expirePendingPair();
  if (response) {
    postNative(response);
    postEvent("pair_expired");
  }
  return response;
}

function postNative(message) {
  if (!nativePort) return false;
  try {
    nativePort.postMessage(assertWireSize(message));
    return true;
  } catch (error) {
    console.warn("Nova bridge send failed", error?.message ?? "unknown error");
    return false;
  }
}

function postEvent(name, details = {}) {
  postNative({
    protocolVersion: PROTOCOL_VERSION,
    kind: "event",
    name,
    epoch: state.epoch,
    details,
  });
  if (["pair_pending", "pair_expired", "pair_confirmed", "route_revoked"].includes(name)) {
    notifyPopup(details.reason ?? name);
  }
}

function notifyPopup(reason) {
  // This carries no authorization or page data. A visible popup reads the
  // current worker state; no receiver is normal when the popup is closed.
  void chrome.runtime.sendMessage({ channel: CHANNEL, type: "popup_state_changed", reason }).catch(() => {});
}

function scheduleReconnect() {
  clearTimeout(reconnectTimer);
  reconnectTimer = setTimeout(connectNative, reconnectDelay);
  reconnectDelay = Math.min(reconnectDelay * 2, 30_000);
}

function connectNative() {
  if (nativePort) return;
  try {
    const port = chrome.runtime.connectNative(NATIVE_HOST);
    nativePort = port;
    state.connectNative();
    reconnectDelay = 1000;
    port.onMessage.addListener(onNativeMessage);
    port.onDisconnect.addListener(() => {
      if (nativePort !== port) return;
      nativePort = null;
      clearTimeout(pairTimer);
      invalidatePairingCandidate();
      state.disconnectNative();
      notifyPopup("native_disconnected");
      scheduleReconnect();
    });
    postNative({
      protocolVersion: PROTOCOL_VERSION,
      kind: "hello",
      role: "chrome_extension",
      extensionId: chrome.runtime.id,
      extensionVersion: chrome.runtime.getManifest().version,
      epoch: state.epoch,
    });
    notifyPopup("native_connected");
  } catch (error) {
    nativePort = null;
    state.disconnectNative();
    scheduleReconnect();
  }
}

async function sendContent(route, action, args) {
  // Chrome checks the actual document's current host/activeTab access. The
  // packaged bootstrap is idempotent and never reads semantic page content.
  await bootstrapScripts(route.tabId, route.documentId);
  if (action !== "ping" && state.checkPairedRoute(route)) {
    throw new ProtocolError("page_access_revoked", "The paired page was revoked; enable and pair it again");
  }
  const message = {
    channel: CHANNEL,
    type: "semantic_command",
    route,
    action,
    args: args ?? {},
  };
  let timer;
  try {
    return await Promise.race([
      chrome.tabs.sendMessage(route.tabId, message, { documentId: route.documentId }),
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(Object.assign(new Error("content script timed out"), { code: "content_timeout" })),
          CONTENT_TIMEOUT_MS,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function dispatchRequest(request) {
  // Diagnostics are always redacted before logging. In particular set_value
  // never reaches a console sink as plaintext.
  void redactForDiagnostic(request).then((diagnostic) =>
    console.debug("Nova semantic request", diagnostic),
  );

  let decision;
  try {
    decision = state.begin(request);
  } catch (error) {
    if (isOpaqueId(request?.requestId)) {
      postNative(
        state.reject(
          request,
          error?.code ?? "invalid_request",
          String(error?.message ?? "invalid request").slice(0, 512),
        ),
      );
    }
    return;
  }

  if (decision.response) {
    postNative(decision.response);
    if (request.action === "release") postEvent("route_revoked", { reason: "released" });
    return;
  }
  if (decision.pendingPair) {
    invalidatePairingCandidate();
    postEvent("pair_pending", { expiresAt: decision.pendingPair.expiresAt });
    clearTimeout(pairTimer);
    pairTimer = setTimeout(expirePendingPair, PAIR_TTL_MS + 10);
    return;
  }
  if (!decision.execute) return;

  try {
    const content = await sendContent(decision.route, request.action, request.args);
    if (!content || content.action !== request.action) {
      postNative(state.complete(request.requestId, content?.action ?? "status", decision.route, undefined));
      return;
    }
    if (!content.ok) {
      postNative(
        state.failExecution(
          request.requestId,
          request.action,
          decision.route,
          content.code ?? "dom_action_failed",
          String(content.message ?? "DOM action failed").slice(0, 512),
        ),
      );
      return;
    }
    postNative(state.complete(request.requestId, request.action, content.route, content.result));
  } catch (error) {
    if (["page_access_denied", "page_access_revoked", "stale_document", "content_unavailable"].includes(error?.code)) {
      // A late permission probe belongs only to the pairing that started it.
      if (sameRoute(decision.route, state.paired?.route, true)) {
        revokeTabAccess(decision.route.tabId, error.code);
      }
      postNative(state.reject(request, error.code, error.message));
      return;
    }
    const isTimeout = error?.code === "content_timeout";
    const errorCode = isTimeout
      ? "ambiguous_content_timeout"
      : "ambiguous_content_transport";
    const beforeEpoch = state.epoch;
    const response = state.failTransportAmbiguity(
      request.requestId,
      request.action,
      decision.route,
      errorCode,
      isTimeout
        ? "content response timed out; the action may have completed"
        : "content transport failed; the action may have completed",
    );
    postNative(response);
    if (state.epoch !== beforeEpoch) {
      postEvent("route_revoked", {
        reason: "content_transport_ambiguous",
        errorCode,
        previousRoute: decision.route,
      });
    }
  }
}

function onNativeMessage(message) {
  try {
    assertWireSize(message);
    if (message?.kind === "request") {
      validateRequest(message);
      void dispatchRequest(message);
      return;
    }
    if (message?.kind === "receipt") {
      validateReceipt(message);
      const acknowledged = state.acknowledge(message);
      if (!acknowledged.ok) postEvent("receipt_rejected", acknowledged);
      return;
    }
    throw new ProtocolError("invalid_kind", "only request and receipt are accepted from Nova.app");
  } catch (error) {
    postEvent("protocol_error", {
      code: error?.code ?? "invalid_message",
      message: String(error?.message ?? "invalid message").slice(0, 512),
    });
  }
}

function trustedSenderRoute(sender, nonce) {
  if (
    sender.id !== chrome.runtime.id ||
    sender.frameId !== 0 ||
    !Number.isSafeInteger(sender.tab?.id) ||
    !isOpaqueId(sender.documentId, 256) ||
    !isOpaqueId(nonce, 128)
  ) {
    throw new ProtocolError("untrusted_sender", "sender is not a top-level extension document");
  }
  return { tabId: sender.tab.id, documentId: sender.documentId, nonce };
}

function isTrustedPopupSender(sender) {
  return (
    sender.id === chrome.runtime.id &&
    sender.url === `chrome-extension://${chrome.runtime.id}/popup.html` &&
    sender.tab === undefined
  );
}

async function activeTabId() {
  return (await activeTab())?.id ?? null;
}

async function activeTab() {
  const tabs = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  return Number.isSafeInteger(tabs[0]?.id) ? tabs[0] : null;
}

function siteAccess(rawUrl) {
  let url;
  try { url = new URL(rawUrl); } catch {
    return { status: "unknown", code: "page_url_unavailable", message: "The current page URL is unavailable. Open Nova from a web page." };
  }
  if (!["http:", "https:"].includes(url.protocol) ||
      url.hostname === "chromewebstore.google.com" ||
      (url.hostname === "chrome.google.com" && url.pathname.startsWith("/webstore"))) {
    return { status: "unsupported", code: "restricted_page", message: "Chrome does not allow Nova on this page. Use an HTTP(S) page outside the Chrome Web Store." };
  }
  return { origin: url.origin, sitePattern: `${url.protocol}//${url.hostname}/*` };
}

async function activePageAccess() {
  const tab = await activeTab();
  const access = { ...siteAccess(tab?.url), tabId: tab?.id ?? null, url: tab?.url ?? null };
  if (!access.sitePattern) return access;
  const siteAllowed = await chrome.permissions.contains({ origins: [access.sitePattern] });
  return {
    ...access,
    siteAllowed,
    status: siteAllowed ? "site_allowed" : state.routes.has(tab.id) ? "tab_enabled" : "needs_tab_access",
    message: siteAllowed
      ? "This site is allowed. Enable the page, then pair its exact document."
      : state.routes.has(tab.id)
        ? "This tab is enabled temporarily. Pairing is still required."
        : "Enable this tab temporarily, or allow only the site shown below.",
  };
}

async function bootstrapScripts(tabId, documentId = null) {
  let results;
  try {
    results = await chrome.scripting.executeScript({
      target: documentId ? { tabId, documentIds: [documentId] } : { tabId, frameIds: [0] },
      files: documentId ? ["content-script.js"] : PACKAGED_SCRIPTS,
      world: "ISOLATED",
    });
  } catch {
    throw new ProtocolError("page_access_denied", "Chrome denied access to this document. Open Nova on the page or allow its site, then enable and pair again.");
  }
  const main = results.find((result) => result.frameId === 0);
  if (!main || !isOpaqueId(main.documentId, 256) || (documentId && main.documentId !== documentId)) {
    throw new ProtocolError("stale_document", "The document changed; review the page and pair again");
  }
  if (!main.result?.ok) {
    throw new ProtocolError("content_unavailable", "The packaged page bridge could not register; enable the page again");
  }
  return main.documentId;
}

async function bootstrapReviewedTab(message) {
  const tab = await activeTab();
  if (!tab || tab.id !== message.tabId || tab.url !== message.url) {
    throw new ProtocolError("stale_reviewed_tab", "The reviewed tab changed; reopen the popup to review it again");
  }
  const access = siteAccess(tab.url);
  if (!access.sitePattern) throw new ProtocolError(access.code, access.message);
  const documentId = await bootstrapScripts(tab.id);
  const current = await activeTab();
  const entry = state.routes.get(tab.id);
  if (!current || current.id !== tab.id || current.url !== tab.url || entry?.route.documentId !== documentId) {
    throw new ProtocolError("stale_document", "The document changed while enabling it; review and enable it again");
  }
  return { ok: true };
}

function removedHostMatches(pattern, rawUrl) {
  if (pattern === "<all_urls>") return true;
  const parts = /^(\*|https?):\/\/([^/]+)\//u.exec(pattern);
  if (!parts) return false;
  let url;
  try { url = new URL(rawUrl); } catch { return false; }
  const [, scheme, host] = parts;
  return (scheme === "*" || `${scheme}:` === url.protocol) &&
    (host === "*" || host === url.hostname ||
      (host.startsWith("*.") && (url.hostname === host.slice(2) || url.hostname.endsWith(host.slice(1)))));
}

function revokeTabAccess(tabId, reason) {
  const entry = state.routes.get(tabId);
  if (pairingCandidateRoute()?.tabId === tabId) invalidatePairingCandidate();
  const beforeEpoch = state.epoch;
  state.unregisterTab(tabId, reason);
  if (entry) {
    void chrome.tabs.sendMessage(tabId, { channel: CHANNEL, type: "revoke_access" },
      { documentId: entry.route.documentId }).catch(() => {});
  }
  if (state.epoch !== beforeEpoch) postEvent("route_revoked", { reason });
  else notifyPopup(reason);
}

async function pingExactRoute(route) {
  const pingRoute = Object.freeze({ ...route, epoch: state.epoch + 1 });
  const pong = await sendContent(pingRoute, "ping", {});
  if (
    !pong?.ok ||
    pong.action !== "ping" ||
    !sameRoute(pong.route, pingRoute, true)
  ) {
    throw new ProtocolError("content_unavailable", "the exact top-level document did not answer");
  }
}

function candidateStillMatches(candidate) {
  const pending = state.pendingPair;
  const entry = state.routes.get(candidate.route.tabId);
  return Boolean(
    pending &&
      pending.requestId === candidate.pendingRequestId &&
      pending.action === "pair" &&
      pending.expiresAt === candidate.expiresAt &&
      candidate.expiresAt > Date.now() &&
      entry &&
      sameRoute(entry.route, candidate.route, false)
  );
}

async function createPairingCandidate() {
  let revision = pairingCandidateRevision;
  const pending = state.pendingPair;
  if (!pending) return null;
  if (pending.expiresAt <= Date.now()) {
    expirePendingPair();
    return null;
  }

  const tabId = await activeTabId();
  if (revision !== pairingCandidateRevision || !Number.isSafeInteger(tabId)) return null;
  const entry = state.routes.get(tabId);
  if (!entry) {
    if (pairingCandidate) invalidatePairingCandidate();
    return null;
  }
  if (pairingCandidate && candidateStillMatches(pairingCandidate) &&
      sameRoute(pairingCandidate.route, entry.route, false) &&
      pairingCandidate.page.title === entry.title && pairingCandidate.page.url === entry.url) {
    return pairingCandidate;
  }
  invalidatePairingCandidate();
  revision = pairingCandidateRevision;

  const snapshot = Object.freeze({
    pendingRequestId: pending.requestId,
    expiresAt: pending.expiresAt,
    route: Object.freeze({ ...entry.route }),
    page: Object.freeze({ title: entry.title, url: entry.url }),
  });
  pairingCandidateDraftRoute = snapshot.route;
  try {
    await pingExactRoute(snapshot.route);
  } catch (error) {
    // A late read/probe cannot remove a route that was subsequently paired.
    if (revision === pairingCandidateRevision && candidateStillMatches(snapshot)) {
      invalidatePairingCandidate();
      state.unregisterTopFrame(snapshot.route, "content_unavailable");
    }
    throw error;
  }

  if (
    revision !== pairingCandidateRevision ||
    !candidateStillMatches(snapshot) ||
    (await activeTabId()) !== snapshot.route.tabId ||
    revision !== pairingCandidateRevision ||
    !candidateStillMatches(snapshot)
  ) {
    clearCandidateDraft(snapshot.route, revision);
    return null;
  }

  const candidate = Object.freeze({
    ...snapshot,
    candidateId: randomOpaqueId("pair-candidate"),
    revision,
  });
  pairingCandidateDraftRoute = null;
  pairingCandidate = candidate;
  return candidate;
}

async function confirmPairingCandidate(candidateId) {
  if (!isOpaqueId(candidateId)) {
    throw new ProtocolError("invalid_pair_candidate", "pairing candidate is invalid or stale");
  }
  const candidate = pairingCandidate;
  if (!candidate || candidate.candidateId !== candidateId) {
    throw new ProtocolError("invalid_pair_candidate", "pairing candidate is invalid or stale");
  }

  // Consume before the first await so one popup gesture can authorize at most
  // one confirmation attempt. Lifecycle invalidations advance the revision.
  pairingCandidate = null;
  pairingCandidateDraftRoute = candidate.route;
  try {
    if (!candidateStillMatches(candidate)) {
      throw new ProtocolError("stale_pair_candidate", "the pairing candidate is no longer current");
    }
    if ((await activeTabId()) !== candidate.route.tabId) {
      throw new ProtocolError("inactive_pair_candidate", "the reviewed page is no longer active");
    }
    if (candidate.revision !== pairingCandidateRevision || !candidateStillMatches(candidate)) {
      throw new ProtocolError("stale_pair_candidate", "the pairing candidate is no longer current");
    }

    await pingExactRoute(candidate.route);
    if (
      candidate.revision !== pairingCandidateRevision ||
      !candidateStillMatches(candidate) ||
      (await activeTabId()) !== candidate.route.tabId ||
      candidate.revision !== pairingCandidateRevision ||
      !candidateStillMatches(candidate)
    ) {
      throw new ProtocolError("stale_pair_candidate", "the pairing candidate is no longer current");
    }
    return state.confirmPair(candidate.route);
  } finally {
    clearCandidateDraft(candidate.route, candidate.revision);
  }
}

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (message?.channel !== CHANNEL) return false;

  if (message.type === "register_top_frame") {
    try {
      const beforeEpoch = state.epoch;
      const route = trustedSenderRoute(sender, message.nonce);
      const previous = state.routes.get(route.tabId);
      state.registerTopFrame(route, { url: sender.url ?? message.url, title: message.title });
      const current = state.routes.get(route.tabId);
      if (previous && !sameRoute(previous.route, route, false)) invalidatePairingCandidate();
      if (state.epoch !== beforeEpoch) postEvent("route_revoked", { reason: "document_replaced" });
      else if (!previous || !sameRoute(previous.route, route, false) ||
               previous.url !== current.url || previous.title !== current.title) {
        notifyPopup("page_enabled");
      }
      sendResponse({ ok: true, route });
    } catch (error) {
      sendResponse({ ok: false, code: error?.code ?? "registration_failed" });
    }
    return false;
  }

  if (message.type === "unregister_top_frame") {
    try {
      const beforeEpoch = state.epoch;
      const route = trustedSenderRoute(sender, message.nonce);
      if (message.documentId !== route.documentId) throw new ProtocolError("route_mismatch", "document mismatch");
      const removed = state.unregisterTopFrame(route);
      if (removed && sameRoute(pairingCandidateRoute(), route, false)) {
        invalidatePairingCandidate();
      }
      if (state.epoch !== beforeEpoch) postEvent("route_revoked", { reason: "document_unloaded" });
      else if (removed) notifyPopup("document_unloaded");
      sendResponse({ ok: true });
    } catch (error) {
      sendResponse({ ok: false, code: error?.code ?? "unregister_failed" });
    }
    return false;
  }

  // Pair confirmation/release is an explicit user action in the extension
  // popup. Do not accept the same message shape from an injected content
  // script merely because it belongs to this extension ID.
  if (!isTrustedPopupSender(sender)) return false;
  if (message.type === "bootstrap_tab") {
    void bootstrapReviewedTab(message).then(sendResponse).catch((error) =>
      sendResponse({ ok: false, code: error?.code ?? "page_access_failed", message: error?.message }));
    return true;
  }
  if (message.type === "popup_state") {
    void (async () => {
      let access = await activePageAccess();
      let candidate = null;
      try {
        candidate = state.pendingPair ? await createPairingCandidate() : null;
      } catch (error) {
        access = { ...access, status: "denied", code: error?.code ?? "content_unavailable", message: error?.message };
      }
      if (!state.pendingPair) invalidatePairingCandidate();
      const activeId = candidate ? candidate.route.tabId : await activeTabId();
      const activeEntry = Number.isSafeInteger(activeId) ? state.routes.get(activeId) : null;
      const status = state.status();
      const pairedEntry = status.route ? state.routes.get(status.route.tabId) : null;
      const pairedPage =
        pairedEntry && sameRoute(pairedEntry.route, status.route, false)
          ? { title: pairedEntry.title, url: pairedEntry.url }
          : null;
      sendResponse({
        ok: true,
        status,
        activePage: activeEntry
          ? candidate
            ? candidate.page
            : { title: activeEntry.title, url: activeEntry.url }
          : null,
        pairedPage,
        candidateId: candidate?.candidateId ?? null,
        access,
      });
    })().catch(() => sendResponse({ ok: false, code: "popup_state_failed" }));
    return true;
  }
  if (message.type === "confirm_pair") {
    void (async () => {
      const response = await confirmPairingCandidate(message.candidateId);
      clearTimeout(pairTimer);
      postNative(response);
      postEvent("pair_confirmed", { route: response.result.route });
      sendResponse({ ok: true, route: response.result.route });
    })().catch((error) =>
      sendResponse({ ok: false, code: error?.code ?? "pair_failed", message: error?.message }),
    );
    return true;
  }
  if (message.type === "deny_pair") {
    try {
      invalidatePairingCandidate();
      clearTimeout(pairTimer);
      const response = state.denyPair();
      postNative(response);
      notifyPopup("pair_denied");
      sendResponse({ ok: true });
    } catch (error) {
      sendResponse({ ok: false, code: error?.code ?? "pair_failed" });
    }
    return false;
  }
  if (message.type === "release_pair") {
    invalidatePairingCandidate();
    const previous = state.status().route;
    if (previous) {
      state.revoke("popup_release");
      postEvent("route_revoked", { reason: "popup_release", previousRoute: previous });
    }
    sendResponse({ ok: true });
    return false;
  }
  return false;
});

chrome.tabs.onRemoved.addListener((tabId) => {
  if (pairingCandidateRoute()?.tabId === tabId) invalidatePairingCandidate();
  const beforeEpoch = state.epoch;
  state.unregisterTab(tabId, "tab_closed");
  if (state.epoch !== beforeEpoch) postEvent("route_revoked", { reason: "tab_closed" });
  else notifyPopup("tab_closed");
});

chrome.tabs.onReplaced.addListener((addedTabId, removedTabId) => {
  if (
    pairingCandidateRoute()?.tabId === removedTabId ||
    pairingCandidateRoute()?.tabId === addedTabId
  ) {
    invalidatePairingCandidate();
  }
  const beforeEpoch = state.epoch;
  state.unregisterTab(removedTabId, "tab_replaced");
  state.unregisterTab(addedTabId, "tab_replaced");
  if (state.epoch !== beforeEpoch) postEvent("route_revoked", { reason: "tab_replaced" });
  else notifyPopup("tab_replaced");
});

chrome.tabs.onUpdated.addListener((tabId, changeInfo) => {
  if (changeInfo.status !== "loading" && changeInfo.url === undefined) return;
  revokeTabAccess(tabId, "navigation");
});

chrome.permissions.onRemoved.addListener((permissions) => {
  for (const [tabId, entry] of [...state.routes]) {
    if (permissions.origins?.some((pattern) => removedHostMatches(pattern, entry.url))) {
      revokeTabAccess(tabId, "permission_removed");
    }
  }
  notifyPopup("site_permission_removed");
});

chrome.permissions.onAdded.addListener(() => notifyPopup("site_permission_added"));

connectNative();
