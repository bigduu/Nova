import assert from "node:assert/strict";
import { webcrypto } from "node:crypto";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const contentSource = await readFile(new URL("../content-script.js", import.meta.url), "utf8");

function invoke(listener, message, sender) {
  return new Promise((resolve, reject) => {
    const keepAlive = listener(message, sender, resolve);
    if (keepAlive !== true) reject(new Error("listener did not keep the response channel alive"));
  });
}

test("content routing requires an exact route and consumes snapshots after one mutation", async () => {
  let runtimeListener;
  let actionCalls = 0;
  const handle = { element: {}, actions: ["set_value"] };
  const route = { tabId: 4, documentId: "document-4", nonce: null };
  const runtime = {
    id: "nova-extension-id",
    lastError: null,
    onMessage: {
      addListener(listener) {
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
  vm.runInNewContext(contentSource, context);
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
