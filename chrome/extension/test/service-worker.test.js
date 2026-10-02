import assert from "node:assert/strict";
import { webcrypto } from "node:crypto";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";
import { CONTENT_TIMEOUT_MS } from "../lib/protocol.js";

function eventHook() {
  const listeners = [];
  return {
    listeners,
    addListener(listener) {
      listeners.push(listener);
    },
    emit(...args) {
      for (const listener of listeners) listener(...args);
    },
  };
}

function installConsentApis(chrome) {
  chrome.permissions ??= { contains: async () => false, onRemoved: eventHook() };
  chrome.scripting ??= {
    executeScript: async ({ target }) => [{
      frameId: 0,
      documentId: target.documentIds?.[0] ?? "fixture-document",
      result: { ok: true },
    }],
  };
  const send = chrome.tabs.sendMessage;
  chrome.tabs.sendMessage = async (...args) => args[1]?.type === "revoke_access"
    ? { ok: true } : send(...args);
}

function callListener(listener, message, sender) {
  return new Promise((resolve, reject) => {
    let settled = false;
    const timeout = setTimeout(() => reject(new Error(`message timed out: ${message.type}`)), 1_000);
    const sendResponse = (response) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      resolve(response);
    };
    const keepAlive = listener(message, sender, sendResponse);
    if (keepAlive !== true && !settled) {
      settled = true;
      clearTimeout(timeout);
      resolve(undefined);
    }
  });
}

async function waitForValue(read, description) {
  for (let attempt = 0; attempt < 50; attempt += 1) {
    const value = read();
    if (value) return value;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail(`timed out waiting for ${description}`);
}

test("popup state keeps the paired page separate from the active tab", async (t) => {
  const originalChrome = globalThis.chrome;
  const extensionMessages = eventHook();
  const nativeMessages = eventHook();
  const nativeDisconnect = eventHook();
  const posted = [];
  let activeTabId = 1;

  const nativePort = {
    onMessage: nativeMessages,
    onDisconnect: nativeDisconnect,
    postMessage(message) {
      posted.push(structuredClone(message));
    },
  };
  const chrome = {
    runtime: {
      id: "nova-extension-id",
      getManifest: () => ({ version: "0.1.0" }),
      connectNative: () => nativePort,
      onMessage: extensionMessages,
    },
    tabs: {
      query: async () => [{ id: activeTabId }],
      sendMessage: async (_tabId, message) => ({
        ok: true,
        action: message.action,
        route: message.route,
      }),
      onRemoved: eventHook(),
      onReplaced: eventHook(),
      onUpdated: eventHook(),
    },
  };
  installConsentApis(chrome);
  globalThis.chrome = chrome;
  t.after(() => {
    globalThis.chrome = originalChrome;
  });

  await import(`../service-worker.js?test=${Date.now()}`);
  assert.equal(extensionMessages.listeners.length, 1);
  assert.equal(nativeMessages.listeners.length, 1);
  const listener = extensionMessages.listeners[0];

  const pairedSender = {
    id: chrome.runtime.id,
    frameId: 0,
    tab: { id: 1 },
    documentId: "document-paired",
    url: "https://paired.example/private/path?token=redacted",
  };
  const activeSender = {
    id: chrome.runtime.id,
    frameId: 0,
    tab: { id: 2 },
    documentId: "document-active",
    url: "https://active.example/elsewhere",
  };

  assert.deepEqual(
    await callListener(
      listener,
      {
        channel: "nova-extension-v1",
        type: "register_top_frame",
        nonce: "page-paired",
        url: "https://spoofed.example/",
        title: "Paired title",
      },
      pairedSender,
    ),
    {
      ok: true,
      route: {
        tabId: 1,
        documentId: "document-paired",
        nonce: "page-paired",
      },
    },
  );
  await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "register_top_frame",
      nonce: "page-active",
      title: "Active title",
    },
    activeSender,
  );

  nativeMessages.emit({
    protocolVersion: 1,
    kind: "request",
    requestId: "pair-request",
    action: "pair",
    args: {},
  });

  const popupSender = { id: chrome.runtime.id, url: `chrome-extension://${chrome.runtime.id}/popup.html` };
  const pairingState = await callListener(
    listener,
    { channel: "nova-extension-v1", type: "popup_state" },
    popupSender,
  );
  assert.equal(typeof pairingState.candidateId, "string");
  const confirmed = await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "confirm_pair",
      candidateId: pairingState.candidateId,
    },
    popupSender,
  );
  assert.equal(confirmed.ok, true);
  assert.equal(confirmed.route.tabId, 1);

  activeTabId = 2;
  const popupState = await callListener(
    listener,
    { channel: "nova-extension-v1", type: "popup_state" },
    popupSender,
  );
  assert.equal(popupState.status.paired, true);
  assert.deepEqual(popupState.activePage, {
    title: "Active title",
    url: "https://active.example/elsewhere",
  });
  assert.deepEqual(popupState.pairedPage, {
    title: "Paired title",
    // The service worker trusts Chrome's sender URL, never page-supplied metadata.
    url: "https://paired.example/private/path?token=redacted",
  });
  assert.notEqual(popupState.activePage.url, popupState.pairedPage.url);

  assert.ok(posted.some((message) => message.kind === "hello"));
  assert.ok(posted.some((message) => message.name === "pair_pending"));
  assert.ok(posted.some((message) => message.name === "pair_confirmed"));
});

test("mutation transport timeout revokes pairing and cannot be retried on the old route", async (t) => {
  const originalChrome = globalThis.chrome;
  const extensionMessages = eventHook();
  const nativeMessages = eventHook();
  const posted = [];
  let mutationSends = 0;

  const chrome = {
    runtime: {
      id: "nova-extension-transport-test",
      getManifest: () => ({ version: "0.1.0" }),
      connectNative: () => ({
        onMessage: nativeMessages,
        onDisconnect: eventHook(),
        postMessage(message) {
          posted.push(structuredClone(message));
        },
      }),
      onMessage: extensionMessages,
    },
    tabs: {
      query: async () => [{ id: 21 }],
      sendMessage: async (_tabId, message) => {
        if (message.action === "ping") {
          return { ok: true, action: "ping", route: message.route };
        }
        mutationSends += 1;
        throw Object.assign(new Error("content script timed out"), {
          code: "content_timeout",
        });
      },
      onRemoved: eventHook(),
      onReplaced: eventHook(),
      onUpdated: eventHook(),
    },
  };
  installConsentApis(chrome);
  globalThis.chrome = chrome;
  t.after(() => {
    globalThis.chrome = originalChrome;
  });

  await import(`../service-worker.js?transport-ambiguity=${Date.now()}`);
  const listener = extensionMessages.listeners[0];
  const popupSender = {
    id: chrome.runtime.id,
    url: `chrome-extension://${chrome.runtime.id}/popup.html`,
  };
  await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "register_top_frame",
      nonce: "page-transport",
      title: "Transport test",
    },
    {
      id: chrome.runtime.id,
      frameId: 0,
      tab: { id: 21 },
      documentId: "document-transport",
      url: "https://transport.example/form",
    },
  );
  nativeMessages.emit({
    protocolVersion: 1,
    kind: "request",
    requestId: "pair-transport-request",
    action: "pair",
    args: {},
  });
  const candidate = await callListener(
    listener,
    { channel: "nova-extension-v1", type: "popup_state" },
    popupSender,
  );
  const confirmed = await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "confirm_pair",
      candidateId: candidate.candidateId,
    },
    popupSender,
  );
  assert.equal(confirmed.ok, true);
  const oldRoute = confirmed.route;

  nativeMessages.emit({
    protocolVersion: 1,
    kind: "request",
    requestId: "mutation-timeout",
    action: "set_value",
    route: oldRoute,
    args: { snapshotId: "snapshot-1", nodeId: "field-1", value: "new value" },
  });
  const terminal = await waitForValue(
    () => posted.find((message) => message.requestId === "mutation-timeout"),
    "the ambiguous mutation terminal",
  );
  assert.equal(terminal.status, "ambiguous");
  assert.equal(terminal.error.code, "ambiguous_content_timeout");
  assert.equal(terminal.error.retryable, false);
  assert.deepEqual(terminal.route, oldRoute);
  assert.equal(terminal.epoch, oldRoute.epoch + 1);
  assert.ok(terminal.receipt.receiptId);

  const revocation = posted.find(
    (message) =>
      message.kind === "event" &&
      message.name === "route_revoked" &&
      message.details?.reason === "content_transport_ambiguous",
  );
  assert.ok(revocation);
  assert.equal(revocation.epoch, terminal.epoch);
  assert.deepEqual(revocation.details.previousRoute, oldRoute);

  const receipt = {
    protocolVersion: 1,
    kind: "receipt",
    receiptId: terminal.receipt.receiptId,
    requestId: terminal.requestId,
    action: terminal.action,
    epoch: terminal.epoch,
  };
  const rejectedBefore = posted.filter((message) => message.name === "receipt_rejected").length;
  nativeMessages.emit(receipt);
  nativeMessages.emit(receipt);
  const rejectedAfter = posted.filter((message) => message.name === "receipt_rejected").length;
  assert.equal(rejectedAfter - rejectedBefore, 1, "the first receipt acknowledgement must be valid");

  nativeMessages.emit({
    protocolVersion: 1,
    kind: "request",
    requestId: "status-after-timeout",
    action: "status",
    args: {},
  });
  const status = posted.find((message) => message.requestId === "status-after-timeout");
  assert.equal(status.result.paired, false);
  assert.equal(status.result.epoch, terminal.epoch);

  nativeMessages.emit({
    protocolVersion: 1,
    kind: "request",
    requestId: "mutation-retry",
    action: "set_value",
    route: oldRoute,
    args: { snapshotId: "snapshot-1", nodeId: "field-1", value: "new value" },
  });
  const retry = posted.find((message) => message.requestId === "mutation-retry");
  assert.equal(retry.status, "error");
  assert.equal(retry.error.code, "not_paired");
  assert.equal(mutationSends, 1, "the revoked route must not reach the content script again");
});

test("top-frame registration rejects non-top-level and foreign senders", async (t) => {
  const originalChrome = globalThis.chrome;
  const extensionMessages = eventHook();
  const chrome = {
    runtime: {
      id: "nova-extension-security-test",
      getManifest: () => ({ version: "0.1.0" }),
      connectNative: () => ({
        onMessage: eventHook(),
        onDisconnect: eventHook(),
        postMessage() {},
      }),
      onMessage: extensionMessages,
    },
    tabs: {
      query: async () => [],
      sendMessage: async () => null,
      onRemoved: eventHook(),
      onReplaced: eventHook(),
      onUpdated: eventHook(),
    },
  };
  installConsentApis(chrome);
  globalThis.chrome = chrome;
  t.after(() => {
    globalThis.chrome = originalChrome;
  });

  await import(`../service-worker.js?security=${Date.now()}`);
  const listener = extensionMessages.listeners[0];
  const message = {
    channel: "nova-extension-v1",
    type: "register_top_frame",
    nonce: "page-security",
    title: "No access",
  };
  const subframe = await callListener(listener, message, {
    id: chrome.runtime.id,
    frameId: 1,
    tab: { id: 3 },
    documentId: "document-3",
    url: "https://example.test/",
  });
  assert.deepEqual(subframe, { ok: false, code: "untrusted_sender" });

  const foreign = await callListener(listener, message, {
    id: "other-extension",
    frameId: 0,
    tab: { id: 3 },
    documentId: "document-3",
    url: "https://example.test/",
  });
  assert.deepEqual(foreign, { ok: false, code: "untrusted_sender" });
});

test("content scripts cannot impersonate the user-confirmation popup", async (t) => {
  const originalChrome = globalThis.chrome;
  const extensionMessages = eventHook();
  const chrome = {
    runtime: {
      id: "nova-extension-popup-boundary",
      getManifest: () => ({ version: "0.1.0" }),
      connectNative: () => ({
        onMessage: eventHook(),
        onDisconnect: eventHook(),
        postMessage() {},
      }),
      onMessage: extensionMessages,
    },
    tabs: {
      query: async () => [{ id: 9 }],
      sendMessage: async () => ({ ok: true, action: "ping" }),
      onRemoved: eventHook(),
      onReplaced: eventHook(),
      onUpdated: eventHook(),
    },
  };
  installConsentApis(chrome);
  globalThis.chrome = chrome;
  t.after(() => {
    globalThis.chrome = originalChrome;
  });

  await import(`../service-worker.js?popup-boundary=${Date.now()}`);
  const listener = extensionMessages.listeners[0];
  const response = await callListener(
    listener,
    { channel: "nova-extension-v1", type: "confirm_pair" },
    {
      id: chrome.runtime.id,
      frameId: 0,
      tab: { id: 9 },
      documentId: "document-9",
      url: "https://page.example/",
    },
  );
  assert.equal(response, undefined);
});

test("pair confirmation rejects random and superseded candidate IDs", async (t) => {
  const originalChrome = globalThis.chrome;
  const extensionMessages = eventHook();
  const nativeMessages = eventHook();
  const chrome = {
    runtime: {
      id: "nova-extension-candidate-test",
      getManifest: () => ({ version: "0.1.0" }),
      connectNative: () => ({
        onMessage: nativeMessages,
        onDisconnect: eventHook(),
        postMessage() {},
      }),
      onMessage: extensionMessages,
    },
    tabs: {
      query: async () => [{ id: 11 }],
      sendMessage: async (_tabId, message) => ({
        ok: true,
        action: message.action,
        route: message.route,
      }),
      onRemoved: eventHook(),
      onReplaced: eventHook(),
      onUpdated: eventHook(),
    },
  };
  installConsentApis(chrome);
  globalThis.chrome = chrome;
  t.after(() => {
    globalThis.chrome = originalChrome;
  });

  await import(`../service-worker.js?candidate=${Date.now()}`);
  const listener = extensionMessages.listeners[0];
  const popupSender = { id: chrome.runtime.id, url: `chrome-extension://${chrome.runtime.id}/popup.html` };
  await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "register_top_frame",
      nonce: "page-candidate",
      title: "Candidate page",
    },
    {
      id: chrome.runtime.id,
      frameId: 0,
      tab: { id: 11 },
      documentId: "document-candidate",
      url: "https://candidate.example/review",
    },
  );
  nativeMessages.emit({
    protocolVersion: 1,
    kind: "request",
    requestId: "pair-candidate-request",
    action: "pair",
    args: {},
  });

  const first = await callListener(
    listener,
    { channel: "nova-extension-v1", type: "popup_state" },
    popupSender,
  );
  const second = await callListener(
    listener,
    { channel: "nova-extension-v1", type: "popup_state" },
    popupSender,
  );
  assert.match(first.candidateId, /^pair-candidate-[a-f0-9]{32}$/u);
  assert.match(second.candidateId, /^pair-candidate-[a-f0-9]{32}$/u);
  assert.notEqual(first.candidateId, second.candidateId);

  const stale = await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "confirm_pair",
      candidateId: first.candidateId,
    },
    popupSender,
  );
  assert.equal(stale.ok, false);
  assert.equal(stale.code, "invalid_pair_candidate");

  const random = await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "confirm_pair",
      candidateId: "pair-candidate-ffffffffffffffffffffffffffffffff",
    },
    popupSender,
  );
  assert.equal(random.ok, false);
  assert.equal(random.code, "invalid_pair_candidate");

  const confirmed = await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "confirm_pair",
      candidateId: second.candidateId,
    },
    popupSender,
  );
  assert.equal(confirmed.ok, true);
  assert.equal(confirmed.route.documentId, "document-candidate");
});

test("reviewed document token cannot pair its same-tab navigation replacement", async (t) => {
  const originalChrome = globalThis.chrome;
  const extensionMessages = eventHook();
  const nativeMessages = eventHook();
  const posted = [];
  const sentRoutes = [];
  const tabsUpdated = eventHook();
  const chrome = {
    runtime: {
      id: "nova-extension-toctou-test",
      getManifest: () => ({ version: "0.1.0" }),
      connectNative: () => ({
        onMessage: nativeMessages,
        onDisconnect: eventHook(),
        postMessage(message) {
          posted.push(structuredClone(message));
        },
      }),
      onMessage: extensionMessages,
    },
    tabs: {
      query: async () => [{ id: 7 }],
      sendMessage: async (_tabId, message) => {
        sentRoutes.push(structuredClone(message.route));
        return { ok: true, action: message.action, route: message.route };
      },
      onRemoved: eventHook(),
      onReplaced: eventHook(),
      onUpdated: tabsUpdated,
    },
  };
  installConsentApis(chrome);
  globalThis.chrome = chrome;
  t.after(() => {
    globalThis.chrome = originalChrome;
  });

  await import(`../service-worker.js?toctou=${Date.now()}`);
  const listener = extensionMessages.listeners[0];
  const popupSender = { id: chrome.runtime.id, url: `chrome-extension://${chrome.runtime.id}/popup.html` };
  const senderA = {
    id: chrome.runtime.id,
    frameId: 0,
    tab: { id: 7 },
    documentId: "document-a",
    url: "https://a.example/reviewed",
  };
  await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "register_top_frame",
      nonce: "page-a",
      title: "Reviewed A",
    },
    senderA,
  );
  nativeMessages.emit({
    protocolVersion: 1,
    kind: "request",
    requestId: "pair-navigation-request",
    action: "pair",
    args: {},
  });

  const reviewed = await callListener(
    listener,
    { channel: "nova-extension-v1", type: "popup_state" },
    popupSender,
  );
  assert.equal(reviewed.activePage.title, "Reviewed A");
  assert.equal(sentRoutes.length, 1);
  assert.equal(sentRoutes[0].documentId, "document-a");

  tabsUpdated.emit(7, { status: "loading" });
  await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "register_top_frame",
      nonce: "page-b",
      title: "Unreviewed B",
    },
    {
      ...senderA,
      documentId: "document-b",
      url: "https://b.example/unreviewed",
    },
  );

  const rejected = await callListener(
    listener,
    {
      channel: "nova-extension-v1",
      type: "confirm_pair",
      candidateId: reviewed.candidateId,
    },
    popupSender,
  );
  assert.equal(rejected.ok, false);
  assert.equal(rejected.code, "invalid_pair_candidate");
  assert.equal(sentRoutes.length, 1, "stale confirmation must not ping the replacement document");
  assert.equal(posted.some((message) => message.name === "pair_confirmed"), false);

  const current = await callListener(
    listener,
    { channel: "nova-extension-v1", type: "popup_state" },
    popupSender,
  );
  assert.equal(current.status.paired, false);
  assert.equal(current.activePage.title, "Unreviewed B");
  assert.notEqual(current.candidateId, reviewed.candidateId);
  const denied = await callListener(
    listener,
    { channel: "nova-extension-v1", type: "deny_pair" },
    popupSender,
  );
  assert.equal(denied.ok, true);
});

let consentFixtureId = 0;
async function consentFixture(t, url = "https://consent.example:8443/review") {
  const originalChrome = globalThis.chrome;
  const extensionMessages = eventHook();
  const nativeMessages = eventHook();
  const nativeDisconnect = eventHook();
  const posted = [];
  const injections = [];
  const contentMessages = [];
  const grants = new Set();
  let listener;
  let denied = false;
  let readReply = null;
  const tab = { id: 56, url, title: "Consent fixture" };
  const sender = {
    id: "nova-consent-fixture", frameId: 0, tab: { id: tab.id },
    documentId: "document-consent", url,
  };
  const chrome = {
    runtime: {
      id: sender.id,
      getManifest: () => ({ version: "0.1.0" }),
      connectNative: () => ({
        onMessage: nativeMessages, onDisconnect: nativeDisconnect,
        postMessage: (message) => posted.push(structuredClone(message)),
      }),
      onMessage: extensionMessages,
    },
    permissions: {
      contains: async ({ origins }) => origins.every((origin) => grants.has(origin)),
      onRemoved: eventHook(),
    },
    scripting: {
      async executeScript(options) {
        injections.push(structuredClone(options));
        if (denied) throw new Error("Chrome denied host access");
        if (options.target.documentIds && options.target.documentIds[0] !== sender.documentId) {
          return [{ frameId: 0, documentId: sender.documentId, result: { ok: true } }];
        }
        const result = await callListener(listener, {
          channel: "nova-extension-v1", type: "register_top_frame",
          nonce: "page-consent", title: tab.title,
        }, sender);
        return [{ frameId: 0, documentId: sender.documentId, result }];
      },
    },
    tabs: {
      query: async () => [{ ...tab }],
      async sendMessage(_id, message) {
        contentMessages.push(structuredClone(message));
        if (message.type === "revoke_access") return { ok: true };
        if (message.action === "read" && readReply) return readReply(message);
        return { ok: true, action: message.action, route: message.route,
          result: { nodes: [{ text: "fixture DOM" }] } };
      },
      onRemoved: eventHook(), onReplaced: eventHook(), onUpdated: eventHook(),
    },
  };
  globalThis.chrome = chrome;
  await import(`../service-worker.js?consent=${++consentFixtureId}`);
  listener = extensionMessages.listeners[0];
  const popupSender = { id: chrome.runtime.id, url: `chrome-extension://${chrome.runtime.id}/popup.html` };
  const popup = (type, details = {}) => callListener(listener,
    { channel: "nova-extension-v1", type, ...details }, popupSender);
  const request = (requestId, action, route, args = {}) => nativeMessages.emit({
    protocolVersion: 1, kind: "request", requestId, action, args, ...(route ? { route } : {}),
  });
  t.after(async () => {
    await popup("deny_pair"); // clear pending pair timers without reconnecting a fake host
    globalThis.chrome = originalChrome;
  });
  const enable = () => popup("bootstrap_tab", { tabId: tab.id, url: tab.url });
  const candidate = async () => {
    request("pair-consent", "pair");
    return popup("popup_state");
  };
  const pair = async () => {
    assert.equal((await enable()).ok, true);
    const reviewed = await candidate();
    assert.ok(reviewed.candidateId);
    const confirmed = await popup("confirm_pair", { candidateId: reviewed.candidateId });
    assert.equal(confirmed.ok, true);
    return confirmed.route;
  };
  return { chrome, tab, sender, posted, injections, contentMessages, grants, popup, enable,
    candidate, pair, request,
    disconnect: () => nativeDisconnect.emit(),
    denyAccess: () => { denied = true; },
    deferRead: (reply) => { readReply = reply; },
    removeSite(pattern) {
      grants.delete(pattern);
      chrome.permissions.onRemoved.emit({ origins: [pattern] });
    },
  };
}

test("fresh consent state never injects or pairs until the reviewed HTTP tab is enabled", async (t) => {
  const fixture = await consentFixture(t);
  const initial = await fixture.popup("popup_state");
  assert.equal(initial.access.status, "needs_tab_access");
  assert.equal(initial.access.sitePattern, "https://consent.example/*");
  assert.equal(initial.status.registeredTopFrames, 0);
  assert.equal(fixture.injections.length, 0);
  const changed = await fixture.popup("bootstrap_tab", { tabId: fixture.tab.id, url: "https://other.example/" });
  assert.equal(changed.code, "stale_reviewed_tab");
  assert.equal(fixture.injections.length, 0);
  assert.equal((await fixture.enable()).ok, true);
  assert.deepEqual(fixture.injections[0], {
    target: { tabId: 56, frameIds: [0] },
    files: ["lib/semantic-runtime.js", "content-script.js"], world: "ISOLATED",
  });
  const enabled = await fixture.popup("popup_state");
  assert.equal(enabled.access.status, "tab_enabled");
  assert.equal(enabled.status.paired, false);
  assert.equal(fixture.posted.some((message) => message.result?.nodes), false);
  fixture.grants.add("https://consent.example/*");
  const allowed = await fixture.popup("popup_state");
  assert.equal(allowed.access.status, "site_allowed");
  assert.equal(fixture.injections.length, 1, "a site grant does not bootstrap or pair implicitly");
});

test("restricted and unknown pages report typed access outcomes without injection", async (t) => {
  for (const url of ["chrome://newtab/", "file:///tmp/private.txt", "https://chromewebstore.google.com/detail/test", undefined]) {
    await t.test(String(url), async (child) => {
      const fixture = await consentFixture(child, url);
      if (url === undefined) fixture.tab.url = undefined;
      const current = await fixture.popup("popup_state");
      assert.equal(current.access.status, url === undefined ? "unknown" : "unsupported");
      assert.equal(current.access.code, url === undefined ? "page_url_unavailable" : "restricted_page");
      const enabled = await fixture.enable();
      assert.equal(enabled.ok, false);
      assert.equal(enabled.code, current.access.code);
      assert.equal(fixture.injections.length, 0);
    });
  }
});

test("Chrome access denial returns an explicit native error and revokes the paired route", async (t) => {
  const fixture = await consentFixture(t);
  const route = await fixture.pair();
  fixture.denyAccess();
  fixture.request("denied-read", "read", route);
  const result = await waitForValue(() => fixture.posted.find((message) => message.requestId === "denied-read"), "denied read");
  assert.equal(result.status, "error");
  assert.equal(result.error.code, "page_access_denied");
  assert.equal(result.result, undefined);
  assert.equal(fixture.contentMessages.some((message) => message.action === "read"), false);
  const popup = await fixture.popup("popup_state");
  assert.equal(popup.status.paired, false);
  assert.equal(popup.status.lastRevocation.reason, "page_access_denied");
  const enable = await fixture.enable();
  assert.equal(enable.code, "page_access_denied");
});

test("permission removal invalidates a reviewed candidate before confirmation", async (t) => {
  const fixture = await consentFixture(t);
  await fixture.enable();
  const candidate = await fixture.candidate();
  fixture.removeSite("https://consent.example/*");
  const confirmed = await fixture.popup("confirm_pair", { candidateId: candidate.candidateId });
  assert.equal(confirmed.ok, false);
  assert.equal(confirmed.code, "invalid_pair_candidate");
  assert.equal(fixture.contentMessages.some((message) => message.type === "revoke_access"), true);
});

test("permission removal drops in-flight DOM results and blocks stale snapshot actions", async (t) => {
  const fixture = await consentFixture(t);
  const route = await fixture.pair();
  let resolveRead;
  fixture.deferRead((message) => new Promise((resolve) => { resolveRead = () => resolve({
    ok: true, action: "read", route: message.route, result: { nodes: [{ text: "must not escape" }] },
  }); }));
  fixture.request("revoked-read", "read", route);
  await waitForValue(() => resolveRead, "in-flight read");
  fixture.removeSite("https://consent.example/*");
  resolveRead();
  const result = await waitForValue(() => fixture.posted.find((message) => message.requestId === "revoked-read"), "revoked result");
  assert.equal(result.result, undefined);
  assert.equal(result.status, "ambiguous");
  fixture.request("old-snapshot-action", "activate", route);
  const action = fixture.posted.find((message) => message.requestId === "old-snapshot-action");
  assert.equal(action.error.code, "not_paired");
  assert.equal(fixture.contentMessages.some((message) => message.action === "activate"), false);
  assert.equal(fixture.posted.some((message) => message.name === "route_revoked" && message.details.reason === "permission_removed"), true);
});

test("document navigation revokes pairing and future dispatch targets only the original document", async (t) => {
  const fixture = await consentFixture(t);
  const route = await fixture.pair();
  const probes = fixture.injections.filter((injection) => injection.target.documentIds);
  assert.ok(probes.length >= 2);
  assert.ok(probes.every((probe) => probe.target.documentIds[0] === route.documentId));
  fixture.chrome.tabs.onUpdated.emit(fixture.tab.id, { status: "loading" });
  fixture.request("after-navigation", "read", route);
  assert.equal(fixture.posted.find((message) => message.requestId === "after-navigation").error.code, "not_paired");
  assert.equal(fixture.contentMessages.some((message) => message.action === "read"), false);
});

test("a late denied permission probe cannot revoke a newer pairing in the same document", async (t) => {
  const fixture = await consentFixture(t);
  const oldRoute = await fixture.pair();
  const executeScript = fixture.chrome.scripting.executeScript;
  let rejectProbe;
  fixture.chrome.scripting.executeScript = (options) => {
    if (options.target.documentIds && !rejectProbe) {
      return new Promise((_, reject) => { rejectProbe = () => reject(new Error("old access denied")); });
    }
    return executeScript(options);
  };
  fixture.request("old-permission-probe", "read", oldRoute);
  await waitForValue(() => rejectProbe, "old permission probe");
  await fixture.popup("release_pair");
  assert.equal((await fixture.enable()).ok, true);
  fixture.request("new-pair-consent", "pair");
  const candidate = await fixture.popup("popup_state");
  const confirmed = await fixture.popup("confirm_pair", { candidateId: candidate.candidateId });
  assert.equal(confirmed.ok, true);
  assert.equal(confirmed.route.documentId, oldRoute.documentId);
  assert.equal(confirmed.route.nonce, oldRoute.nonce);
  assert.notEqual(confirmed.route.epoch, oldRoute.epoch);
  const revocationsBeforeLateFailure = fixture.contentMessages.filter((message) => message.type === "revoke_access").length;

  rejectProbe();
  const result = await waitForValue(() => fixture.posted.find((message) => message.requestId === "old-permission-probe"), "old probe result");
  assert.equal(result.error.code, "page_access_denied");
  assert.equal(result.result, undefined);
  const current = await fixture.popup("popup_state");
  assert.equal(current.status.paired, true);
  assert.deepEqual(current.status.route, confirmed.route);
  assert.equal(fixture.contentMessages.filter((message) => message.type === "revoke_access").length, revocationsBeforeLateFailure,
    "the old failed probe must not deliver another revocation to the repaired pairing");
});

async function pendingValueWorkerFixture(t) {
  const fixture = await consentFixture(t);
  const timers = new Set();
  const setTimer = globalThis.setTimeout;
  t.mock.method(globalThis, "setTimeout", (...args) => { const timer = setTimer(...args); timers.add(timer); return timer; });
  t.after(() => { for (const timer of timers) clearTimeout(timer); });
  const rect = { x: 1, y: 2, width: 20, height: 15 };
  const document = {
    readyState: "complete", title: "Pending receipt worker", addEventListener() {},
    defaultView: { Event, getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }) },
  };
  const field = {
    nodeType: 1, tagName: "INPUT", ownerDocument: document, isConnected: true, value: "Initial ✓", events: [],
    getAttribute: (name) => name === "aria-label" ? "Message" : null, hasAttribute: () => false, closest: () => null,
    getClientRects: () => [rect], getBoundingClientRect: () => rect,
    dispatchEvent(event) { this.events.push(event.type); return true; },
  };
  document.body = {
    ...field, tagName: "BODY", firstChild: field, getAttribute: () => null,
  };
  field.parentElement = document.body;
  let digestStarted;
  let resumeDigest;
  const ready = new Promise((resolve) => { digestStarted = resolve; });
  const hold = new Promise((resolve) => { resumeDigest = resolve; });
  const crypto = {
    getRandomValues: (bytes) => webcrypto.getRandomValues(bytes),
    subtle: { async digest(algorithm, bytes) { digestStarted(); await hold; return webcrypto.subtle.digest(algorithm, bytes); } },
  };
  const workerListener = fixture.chrome.runtime.onMessage.listeners[0];
  let contentListener;
  const contentRuntime = {
    id: fixture.chrome.runtime.id, lastError: null,
    onMessage: { addListener(value) { contentListener = value; } },
    sendMessage(message, callback) {
      void callListener(workerListener, message, fixture.sender).then((reply) => callback?.(reply));
    },
  };
  const window = {};
  window.top = window;
  const context = vm.createContext({
    addEventListener() {}, chrome: { runtime: contentRuntime }, crypto, document,
    location: { href: fixture.tab.url }, window, TextEncoder, setTimeout, clearTimeout,
  });
  const [semanticSource, contentSource] = await Promise.all([
    readFile(new URL("../lib/semantic-runtime.js", import.meta.url), "utf8"),
    readFile(new URL("../content-script.js", import.meta.url), "utf8"),
  ]);
  vm.runInContext(semanticSource, context);
  await vm.runInContext(contentSource, context);
  fixture.chrome.scripting.executeScript = async ({ target }) => {
    assert.equal(target.tabId, fixture.tab.id);
    if (target.documentIds) assert.equal(target.documentIds[0], fixture.sender.documentId);
    await context.NovaContentBridge.enable();
    return [{ frameId: 0, documentId: fixture.sender.documentId, result: { ok: true } }];
  };
  let startRevocation;
  let deliverRevocation;
  let failTransport;
  const revocationReady = new Promise((resolve) => { startRevocation = resolve; });
  const transportFailure = new Promise((_, reject) => { failTransport = () => reject(Object.assign(new Error("controlled transport timeout"), { code: "content_timeout" })); });
  let revocationHold = null;
  const targets = [];
  const valueReplies = [];
  fixture.chrome.tabs.sendMessage = async (tabId, message, options) => {
    targets.push({ tabId, type: message.type, documentId: options.documentId });
    if (message.type === "revoke_access") {
      startRevocation();
      if (revocationHold) await revocationHold;
    }
    const reply = callListener(contentListener, message, { id: contentRuntime.id });
    if (message.action !== "set_value") return reply;
    const captured = reply.then((value) => { valueReplies.push(value); return value; });
    return Promise.race([captured, transportFailure]);
  };
  return { ...fixture, field, ready, resumeDigest, revocationReady, targets, valueReplies, failTransport,
    delayRevocation() { revocationHold = new Promise((resolve) => { deliverRevocation = resolve; }); },
    deliverRevocation: () => deliverRevocation(),
  };
}

for (const mode of ["native release", "popup release", "native disconnect", "content timeout"]) {
  test(`${mode} forwards old exact-document revocation before a pending value write resumes`, async (t) => {
    const fixture = await pendingValueWorkerFixture(t);
    const route = await fixture.pair();
    fixture.request("receipt-read", "read", route);
    const read = await waitForValue(() => fixture.posted.find((message) => message.requestId === "receipt-read"), "initial receipt snapshot");
    fixture.request("receipt-write", "set_value", route, {
      snapshotId: read.result.snapshotId, nodeId: read.result.nodes[0].nodeId, value: "Must not write 🪷",
    });
    await fixture.ready;
    assert.equal(fixture.field.value, "Initial ✓");
    assert.deepEqual(fixture.field.events, []);
    fixture.delayRevocation();
    let popupReleased = false;
    let popupReply;
    if (mode === "native release") fixture.request("receipt-release", "release", route);
    if (mode === "popup release") popupReply = fixture.popup("release_pair").then((response) => { popupReleased = true; return response; });
    if (mode === "native disconnect") fixture.disconnect();
    if (mode === "content timeout") fixture.failTransport();
    await fixture.revocationReady;
    const revocation = fixture.targets.find((target) => target.type === "revoke_access");
    assert.deepEqual(revocation, { tabId: route.tabId, type: "revoke_access", documentId: route.documentId });
    assert.equal(fixture.targets.filter((target) => target.type === "revoke_access").length, 1);
    assert.equal(fixture.posted.some((message) => message.requestId === "receipt-release"), false);
    assert.equal(popupReleased, false);
    fixture.deliverRevocation();
    if (mode === "native release") {
      const released = await waitForValue(() => fixture.posted.find((message) => message.requestId === "receipt-release"), "native release after invalidation");
      assert.equal(released.status, "ok");
    }
    if (mode === "popup release") assert.equal((await popupReply).ok, true);
    if (mode === "content timeout") {
      const timeout = await waitForValue(() => fixture.posted.find((message) => message.requestId === "receipt-write"), "timeout after invalidation");
      assert.equal(timeout.error.code, "ambiguous_content_timeout");
    }
    fixture.resumeDigest();
    const reply = await waitForValue(() => fixture.valueReplies[0], "held hash response after forwarded revoke");
    assert.equal(reply.code, "page_access_revoked");
    assert.equal(fixture.field.value, "Initial ✓");
    assert.deepEqual(fixture.field.events, []);

    if (mode === "native release" || mode === "popup release") {
      assert.equal((await fixture.enable()).ok, true);
      fixture.request("receipt-repair", "pair");
      const candidate = await fixture.popup("popup_state");
      const repaired = await fixture.popup("confirm_pair", { candidateId: candidate.candidateId });
      assert.equal(repaired.ok, true);
      assert.equal(repaired.route.documentId, route.documentId);
      assert.equal(repaired.route.nonce, route.nonce);
      assert.notEqual(repaired.route.epoch, route.epoch);
      fixture.request("repaired-read", "read", repaired.route);
      const fresh = await waitForValue(() => fixture.posted.find((message) => message.requestId === "repaired-read"), "repaired snapshot");
      fixture.request("repaired-write", "set_value", repaired.route, {
        snapshotId: fresh.result.snapshotId, nodeId: fresh.result.nodes[0].nodeId, value: "Fresh authorized 🪷 ✓",
      });
      const written = await waitForValue(() => fixture.posted.find((message) => message.requestId === "repaired-write"), "repaired authorized write");
      assert.equal(written.status, "ok");
      assert.equal(fixture.field.value, "Fresh authorized 🪷 ✓");
      assert.deepEqual(fixture.field.events, ["input", "change"]);
      assert.equal(fixture.targets.filter((target) => target.type === "revoke_access").length, 1,
        "late completion must not revoke the newer pairing");
    }
  });
}

test("a late old value timeout never forwards revocation to a newly paired document", async (t) => {
  const fixture = await consentFixture(t);
  const oldRoute = await fixture.pair();
  const send = fixture.chrome.tabs.sendMessage;
  let rejectTransport;
  fixture.chrome.tabs.sendMessage = (tabId, message, options) => {
    if (message.action === "set_value") return new Promise((_, reject) => { rejectTransport = reject; });
    return send(tabId, message, options);
  };
  fixture.request("old-value-timeout", "set_value", oldRoute, { snapshotId: "old-snapshot", nodeId: "old-node", value: "controlled value" });
  await waitForValue(() => rejectTransport, "old value transport");
  assert.equal((await fixture.popup("release_pair")).ok, true);
  fixture.sender.documentId = "newly-paired-document";
  assert.equal((await fixture.enable()).ok, true);
  fixture.request("replacement-document-pair", "pair");
  const candidate = await fixture.popup("popup_state");
  const repaired = await fixture.popup("confirm_pair", { candidateId: candidate.candidateId });
  assert.equal(repaired.ok, true);
  assert.notEqual(repaired.route.documentId, oldRoute.documentId);
  const revocations = fixture.contentMessages.filter((message) => message.type === "revoke_access").length;
  rejectTransport(Object.assign(new Error("controlled late timeout"), { code: "content_timeout" }));
  await waitForValue(() => fixture.posted.find((message) => message.requestId === "old-value-timeout"), "old timeout response");
  assert.equal(fixture.contentMessages.filter((message) => message.type === "revoke_access").length, revocations);
  const current = await fixture.popup("popup_state");
  assert.equal(current.status.paired, true);
  assert.deepEqual(current.status.route, repaired.route);
});

for (const mode of ["native release", "popup release", "content timeout"]) {
  test(`${mode} completes when exact-document revocation remains unanswered`, async (t) => {
    const fixture = await consentFixture(t);
    const route = await fixture.pair();
    const send = fixture.chrome.tabs.sendMessage;
    const targets = [];
    fixture.chrome.tabs.sendMessage = (tabId, message, options) => {
      if (message.type === "revoke_access") {
        targets.push({ tabId, documentId: options.documentId });
        return new Promise(() => {});
      }
      if (mode === "content timeout" && message.action === "set_value") {
        return Promise.reject(Object.assign(new Error("controlled content timeout"), { code: "content_timeout" }));
      }
      return send(tabId, message, options);
    };
    const deadlines = [];
    const setTimer = globalThis.setTimeout;
    const clearTimer = globalThis.clearTimeout;
    t.mock.method(globalThis, "setTimeout", (callback, delay, ...args) => {
      if (delay !== CONTENT_TIMEOUT_MS) return setTimer(callback, delay, ...args);
      const deadline = { callback, cleared: false };
      deadlines.push(deadline);
      return deadline;
    });
    t.mock.method(globalThis, "clearTimeout", (timer) => {
      if (deadlines.includes(timer)) timer.cleared = true;
      else clearTimer(timer);
    });
    let popupResult;
    let popupReply;
    if (mode === "native release") fixture.request("unanswered-release", "release", route);
    if (mode === "popup release") {
      popupReply = fixture.popup("release_pair").then((response) => { popupResult = response; return response; });
      void popupReply.catch(() => {});
    }
    if (mode === "content timeout") {
      fixture.request("unanswered-value", "set_value", route, { snapshotId: "snapshot", nodeId: "node", value: "controlled fixture" });
    }
    await waitForValue(() => targets.length, "unanswered revocation attempt");
    assert.deepEqual(targets, [{ tabId: route.tabId, documentId: route.documentId }]);
    assert.equal(popupResult, undefined);
    assert.equal(fixture.posted.some((message) => ["unanswered-release", "unanswered-value"].includes(message.requestId)), false);
    const pending = deadlines.filter((deadline) => !deadline.cleared);
    assert.equal(pending.length, 1, "revocation must retain the existing content deadline");
    pending[0].callback();
    if (mode === "popup release") assert.equal((await popupReply).ok, true);
    else {
      const requestId = mode === "native release" ? "unanswered-release" : "unanswered-value";
      const result = await waitForValue(() => fixture.posted.find((message) => message.requestId === requestId), "bounded terminal response");
      if (mode === "native release") assert.equal(result.result.released, true);
      else assert.equal(result.error.code, "ambiguous_content_timeout");
    }
    assert.equal(pending[0].cleared, true);
    assert.equal((await fixture.popup("popup_state")).status.paired, false);
  });
}
