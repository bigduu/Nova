import assert from "node:assert/strict";
import { webcrypto } from "node:crypto";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const contentSource = await readFile(new URL("../content-script.js", import.meta.url), "utf8");
const semanticSource = await readFile(new URL("../lib/semantic-runtime.js", import.meta.url), "utf8");

function invoke(listener, message, sender) {
  return new Promise((resolve, reject) => {
    const keepAlive = listener(message, sender, resolve);
    if (keepAlive !== true) reject(new Error("listener did not keep the response channel alive"));
  });
}

test("content routing requires an exact route and consumes snapshots after one mutation", async () => {
  let runtimeListener;
  let listenerCount = 0;
  let actionCalls = 0;
  const handle = { element: {}, actions: ["set_value"] };
  const route = { tabId: 4, documentId: "document-4", nonce: null };
  const runtime = {
    id: "nova-extension-id",
    lastError: null,
    onMessage: {
      addListener(listener) {
        listenerCount += 1;
        runtimeListener = listener;
      },
    },
    sendMessage(message, callback) {
      if (message.type === "register_top_frame") {
        route.nonce = message.nonce;
        callback({ ok: true, route: { ...route } });
      }
    },
  };
  const window = {};
  window.top = window;
  const context = {
    addEventListener() {},
    chrome: { runtime },
    console,
    crypto: webcrypto,
    document: {
      readyState: "complete",
      title: "Content test",
      addEventListener() {},
    },
    location: { href: "https://content.example/path" },
    NovaSemantic: {
      createSnapshot() {
        return {
          result: { snapshotId: "snapshot-1", nodes: [] },
          handles: new Map([["n1", handle]]),
        };
      },
      async performAction(_handle, action, args) {
        actionCalls += 1;
        assert.equal(action, "set_value");
        return { valueUtf8Bytes: new TextEncoder().encode(args.value).byteLength };
      },
    },
    setTimeout,
    clearTimeout,
    TextEncoder,
    window,
  };
  const installedContext = vm.createContext(context);
  await vm.runInContext(contentSource, installedContext);
  assert.equal(typeof runtimeListener, "function");

  const sender = { id: runtime.id };
  const envelope = {
    channel: "nova-extension-v1",
    type: "semantic_command",
    route: { ...route, epoch: 2 },
    args: {},
  };
  const mismatch = await invoke(
    runtimeListener,
    { ...envelope, action: "read", route: { ...envelope.route, documentId: "wrong" } },
    sender,
  );
  assert.equal(mismatch.code, "route_mismatch");

  const read = await invoke(runtimeListener, { ...envelope, action: "read" }, sender);
  assert.equal(read.ok, true);
  assert.equal(read.result.snapshotId, "snapshot-1");
  const nonce = route.nonce;
  await vm.runInContext(contentSource, installedContext);
  assert.equal(route.nonce, nonce, "repeated bootstrap must reuse the document nonce");
  assert.equal(listenerCount, 1, "repeated bootstrap must not create a second listener");

  const mutation = {
    ...envelope,
    action: "set_value",
    args: { snapshotId: "snapshot-1", nodeId: "n1", value: "draft" },
  };
  const first = await invoke(runtimeListener, mutation, sender);
  assert.equal(first.ok, true);
  assert.equal(actionCalls, 1);

  const replay = await invoke(runtimeListener, mutation, sender);
  assert.equal(replay.ok, false);
  assert.equal(replay.code, "stale_snapshot");
  assert.equal(actionCalls, 1);
  let revoked;
  runtimeListener({ channel: "nova-extension-v1", type: "revoke_access" }, sender,
    (response) => { revoked = response; });
  assert.equal(revoked.ok, true);
  const denied = await invoke(runtimeListener, { ...envelope, action: "read" }, sender);
  assert.equal(denied.code, "page_access_revoked");
  await vm.runInContext(contentSource, installedContext);
  assert.equal(route.nonce, nonce);
  assert.equal(listenerCount, 1);
  const oldSnapshot = await invoke(runtimeListener, mutation, sender);
  assert.equal(oldSnapshot.code, "stale_snapshot");
  assert.equal(actionCalls, 1);
});

test("content listener ignores messages from a foreign extension", () => {
  let runtimeListener;
  const runtime = {
    id: "nova-extension-id-foreign-test",
    lastError: null,
    onMessage: { addListener: (listener) => (runtimeListener = listener) },
    sendMessage(message, callback) {
      callback({
        ok: true,
        route: { tabId: 1, documentId: "doc", nonce: message.nonce },
      });
    },
  };
  const window = {};
  window.top = window;
  vm.runInNewContext(contentSource, {
    addEventListener() {},
    chrome: { runtime },
    crypto: webcrypto,
    document: { readyState: "complete", title: "", addEventListener() {} },
    location: { href: "https://example.test/" },
    NovaSemantic: { createSnapshot() {}, performAction() {} },
    setTimeout,
    clearTimeout,
    window,
  });
  const accepted = runtimeListener(
    { channel: "nova-extension-v1", type: "semantic_command" },
    { id: "foreign-extension" },
    () => assert.fail("foreign messages must not receive a response"),
  );
  assert.equal(accepted, false);
});

test("real content snapshot rejects text mutations and preserves a fresh control action", async () => {
  const rect = { x: 1, y: 2, width: 20, height: 15 };
  const style = { display: "block", visibility: "visible", opacity: "1" };
  const document = {
    readyState: "complete", title: "Static text handler", addEventListener() {},
    defaultView: { getComputedStyle: () => style },
    createRange() {
      return { setStart() {}, setEnd() {}, getClientRects: () => [rect], getBoundingClientRect: () => rect };
    },
  };
  function element(tagName) {
    return {
      nodeType: 1, tagName, isConnected: true, ownerDocument: document,
      getAttribute: () => null, hasAttribute: () => false, closest: () => null,
      getClientRects: () => [rect], getBoundingClientRect: () => rect,
    };
  }
  function text(data, parentElement) {
    return { nodeType: 3, data, length: data.length, isConnected: true, parentElement, ownerDocument: document };
  }
  const body = element("BODY");
  const paragraph = text("Read only 中文 🪷", body);
  const button = element("BUTTON");
  const caption = text("Increment", button);
  let clicks = 0;
  button.click = () => { clicks += 1; };
  button.parentElement = body;
  button.firstChild = caption;
  paragraph.nextSibling = button;
  body.firstChild = paragraph;
  document.body = body;

  let listener;
  const route = { tabId: 64, documentId: "static-document", nonce: null };
  const runtime = {
    id: "nova-static-text-test", lastError: null,
    onMessage: { addListener(value) { listener = value; } },
    sendMessage(message, callback) {
      route.nonce = message.nonce;
      callback({ ok: true, route: { ...route } });
    },
  };
  const window = {};
  window.top = window;
  const context = vm.createContext({
    addEventListener() {}, chrome: { runtime }, crypto: webcrypto, document,
    location: { href: "https://static.example/" }, window, TextEncoder, setTimeout, clearTimeout,
  });
  vm.runInContext(semanticSource, context);
  await vm.runInContext(contentSource, context);
  const sender = { id: runtime.id };
  const envelope = { channel: "nova-extension-v1", type: "semantic_command", route: { ...route, epoch: 1 } };
  let previousSnapshot;
  for (const action of ["activate", "focus", "set_value", "scroll"]) {
    const read = await invoke(listener, { ...envelope, action: "read", args: {} }, sender);
    assert.equal(read.ok, true);
    assert.equal(read.result.coverage, "top_document");
    assert.equal(read.result.truncated, false);
    assert.notEqual(read.result.snapshotId, previousSnapshot);
    previousSnapshot = read.result.snapshotId;
    assert.equal(read.result.nodes.map((node) => node.name).join("|"), "Read only 中文 🪷|Increment");
    const node = read.result.nodes.find((node) => node.role === "text");
    assert.equal(node.actions.length, 0);
    const mutation = { ...envelope, action, args: {
      snapshotId: read.result.snapshotId, nodeId: node.nodeId, value: "must not change", direction: "down",
    } };
    const rejected = await invoke(listener, mutation, sender);
    assert.equal(rejected.ok, false);
    assert.equal(rejected.code, "unsupported_action");
    assert.equal((await invoke(listener, mutation, sender)).code, "stale_snapshot");
    assert.equal(paragraph.data, "Read only 中文 🪷");
    assert.equal(clicks, 0);
  }
  const fresh = await invoke(listener, { ...envelope, action: "read", args: {} }, sender);
  const control = fresh.result.nodes.find((node) => node.role === "button");
  assert.ok(control.actions.includes("activate"));
  const activated = await invoke(listener, { ...envelope, action: "activate", args: {
    snapshotId: fresh.result.snapshotId, nodeId: control.nodeId,
  } }, sender);
  assert.equal(activated.ok, true);
  assert.equal(activated.result.activated, true);
  assert.equal(clicks, 1);
});
