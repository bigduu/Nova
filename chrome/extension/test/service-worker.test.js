import assert from "node:assert/strict";
import test from "node:test";

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
