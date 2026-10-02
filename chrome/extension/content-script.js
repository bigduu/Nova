(() => {
  "use strict";

  // Bootstrap targets only the main frame; retain the runtime boundary too.
  if (window.top !== window || !globalThis.NovaSemantic) return;
  if (globalThis.NovaContentBridge) return globalThis.NovaContentBridge.enable();

  const CHANNEL = "nova-extension-v1";
  const nonceBytes = new Uint8Array(16);
  crypto.getRandomValues(nonceBytes);
  const nonce = `page-${Array.from(nonceBytes, (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("")}`;

  let trustedRoute = null;
  let currentSnapshot = null;
  let mutationRevision = 0;
  let registerTimer = null;
  let enabled = true;
  let registration = null;

  function baseRouteMatches(route) {
    return Boolean(
      trustedRoute &&
        route &&
        route.tabId === trustedRoute.tabId &&
        route.documentId === trustedRoute.documentId &&
        route.nonce === nonce,
    );
  }

  function register() {
    if (!enabled) return Promise.resolve({ ok: false, code: "page_access_revoked" });
    if (registration) return registration;
    registration = new Promise((resolve) => chrome.runtime.sendMessage(
      {
        channel: CHANNEL,
        type: "register_top_frame",
        nonce,
        url: location.href,
        title: document.title,
      },
      (response) => {
        if (!enabled) {
          resolve({ ok: false, code: "page_access_revoked" });
          return;
        }
        if (chrome.runtime.lastError || !response?.ok) {
          clearTimeout(registerTimer);
          registerTimer = setTimeout(register, 1000);
          resolve({ ok: false, code: response?.code ?? "content_unavailable" });
          return;
        }
        if (!baseRouteMatches(response.route)) mutationRevision += 1;
        trustedRoute = response.route;
        resolve(response);
      },
    )).finally(() => { registration = null; });
    return registration;
  }

  async function handleCommand(message) {
    if (!enabled) {
      return { ok: false, action: message.action, code: "page_access_revoked", message: "Page access was revoked; enable and pair the page again" };
    }
    if (!baseRouteMatches(message.route)) {
      return { ok: false, action: message.action, code: "route_mismatch", message: "content route mismatch" };
    }
    if (message.action === "ping") {
      return { ok: true, action: "ping", route: message.route };
    }
    if (message.action === "read") {
      // Invalidate before touching the DOM so even a failed read cannot leave an
      // older snapshot actionable.
      mutationRevision += 1;
      currentSnapshot = null;
      const snapshot = NovaSemantic.createSnapshot(document, {
        maxNodes: message.args?.maxNodes,
        maxChars: message.args?.maxChars,
        deadline: message.deadline,
        includeChildFrames: message.includeChildFrames === true,
      });
      currentSnapshot = {
        id: snapshot.result.snapshotId,
        handles: snapshot.handles,
      };
      return { ok: true, action: "read", route: message.route, result: snapshot.result,
        ...(message.includeChildFrames ? { readBudget: snapshot.budget } : {}) };
    }

    if (!["activate", "focus", "set_value", "scroll"].includes(message.action)) {
      return { ok: false, action: message.action, code: "unknown_action", message: "unknown content action" };
    }
    if (typeof message.args?.nodeId === "string" && message.args.nodeId.startsWith("child:")) {
      return { ok: false, action: message.action, code: "read_only_child", message: "Child document nodes are read-only" };
    }
    if (
      !currentSnapshot ||
      typeof message.args?.snapshotId !== "string" ||
      message.args.snapshotId !== currentSnapshot.id
    ) {
      return { ok: false, action: message.action, code: "stale_snapshot", message: "snapshot is absent or stale" };
    }
    const nodeId = message.args?.nodeId;
    if (typeof nodeId !== "string" || !currentSnapshot.handles.has(nodeId)) {
      return { ok: false, action: message.action, code: "unknown_node", message: "node is not in this snapshot" };
    }

    const handle = currentSnapshot.handles.get(nodeId);
    // One read authorizes at most one mutation. This is intentionally consumed
    // before dispatch, including when the DOM operation fails.
    currentSnapshot = null;
    const authorizedRevision = mutationRevision;
    const assertCurrent = () => {
      if (!enabled) throw Object.assign(new Error("Page access was revoked; enable and pair the page again"), { code: "page_access_revoked" });
      if (!baseRouteMatches(message.route)) throw Object.assign(new Error("content route mismatch"), { code: "route_mismatch" });
      if (mutationRevision !== authorizedRevision) {
        throw Object.assign(new Error("Page authority or snapshot changed; read again before retrying"), { code: "stale_snapshot" });
      }
    };
    try {
      const result = await NovaSemantic.performAction(handle, message.action, message.args, assertCurrent,
        { deadline: message.deadline, document });
      if (message.action === "activate") {
        // Authority can change between the runtime's last check and this await
        // resuming. A dispatched activation must not receive a stale success.
        try {
          assertCurrent();
          if (message.deadline && Date.now() >= message.deadline) {
            throw Object.assign(new Error(), { code: "ambiguous_content_timeout" });
          }
        } catch (error) {
          throw Object.assign(new Error(`Activation receipt could not be confirmed (${error.code}). DOM dispatch may already have had side effects; read or inspect the page before retrying.`), { code: error.code });
        }
      }
      return { ok: true, action: message.action, route: message.route, result };
    } catch (error) {
      return {
        ok: false,
        action: message.action,
        route: message.route,
        code: error?.code ?? "dom_action_failed",
        message: String(error?.message ?? "DOM action failed").slice(0, 512),
      };
    }
  }

  chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    if (
      sender.id !== chrome.runtime.id ||
      message?.channel !== CHANNEL
    ) {
      return false;
    }
    if (message.type === "revoke_access") {
      mutationRevision += 1;
      enabled = false;
      clearTimeout(registerTimer);
      currentSnapshot = null;
      trustedRoute = null;
      sendResponse({ ok: true });
      return false;
    }
    if (message.type !== "semantic_command") return false;
    handleCommand(message)
      .then(sendResponse)
      .catch((error) =>
        sendResponse({
          ok: false,
          action: message?.action,
          code: "content_failure",
          message: String(error?.message ?? "content failure").slice(0, 512),
        }),
      );
    return true;
  });

  function unregister() {
    if (!trustedRoute) return;
    mutationRevision += 1;
    chrome.runtime.sendMessage({
      channel: CHANNEL,
      type: "unregister_top_frame",
      nonce,
      documentId: trustedRoute.documentId,
    });
    currentSnapshot = null;
    trustedRoute = null;
  }

  addEventListener("pagehide", unregister, { once: true });
  addEventListener("pageshow", register, { once: true });
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", register, { once: true });
  }
  globalThis.NovaContentBridge = {
    enable() {
      enabled = true;
      return register();
    },
  };
  return register();
})();
