import assert from "node:assert/strict";
import { createHash, webcrypto } from "node:crypto";
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

async function valueReceiptFixture(crypto = webcrypto) {
  const rect = { x: 1, y: 2, width: 20, height: 15 };
  const document = {
    readyState: "complete", title: "Value receipt handler", addEventListener() {},
    defaultView: { Event, getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }) },
  };
  function element(tagName, attributes = {}) {
    return {
      nodeType: 1, tagName, attributes, isConnected: true, ownerDocument: document,
      getAttribute(name) { return this.attributes[name] ?? null; },
      hasAttribute(name) { return Object.hasOwn(this.attributes, name); }, closest: () => null,
      getClientRects: () => [rect], getBoundingClientRect: () => rect,
    };
  }
  const body = element("BODY");
  const host = element("DIV");
  const fields = ["First initial ✓", "Second initial 🪷"].map((value) => {
    const field = element("INPUT", { "aria-label": "Shared message" });
    field.value = value;
    field.events = [];
    field.dispatchEvent = (event) => { field.events.push(event.type); return true; };
    field.focus = () => { document.activeElement = field; };
    field.parentElement = host;
    return field;
  });
  fields[0].nextSibling = fields[1];
  host.firstChild = fields[0];
  host.parentElement = body;
  body.firstChild = host;
  document.body = body;
  let listener;
  const route = { tabId: 67, documentId: "receipt-document", nonce: null };
  const runtime = {
    id: "nova-receipt-test", lastError: null,
    onMessage: { addListener(value) { listener = value; } },
    sendMessage(message, callback) {
      if (message.type !== "register_top_frame") return;
      route.nonce = message.nonce;
      callback({ ok: true, route: { ...route } });
    },
  };
  const window = {};
  window.top = window;
  const events = new Map();
  const context = vm.createContext({
    addEventListener(type, callback) { events.set(type, callback); },
    chrome: { runtime }, crypto, document, location: { href: "http://ordinary.example/" },
    window, TextEncoder, setTimeout, clearTimeout,
  });
  vm.runInContext(semanticSource, context);
  await vm.runInContext(contentSource, context);
  const sender = { id: runtime.id };
  const envelope = () => ({ channel: "nova-extension-v1", type: "semantic_command", route: { ...route, epoch: 7 } });
  return {
    fields, host, context, route,
    read: () => invoke(listener, { ...envelope(), action: "read", args: {} }, sender),
    command: (message) => invoke(listener, message, sender),
    mutation: (snapshot, value, index = 1) => ({ ...envelope(), action: "set_value", args: {
      snapshotId: snapshot.result.snapshotId, nodeId: snapshot.result.nodes[index].nodeId, value,
    } }),
    enable: () => context.NovaContentBridge.enable(),
    pagehide: () => events.get("pagehide")(),
    revoke() {
      let response;
      listener({ channel: "nova-extension-v1", type: "revoke_access" }, sender, (value) => { response = value; });
      assert.equal(response.ok, true);
    },
  };
}

function pendingReceipt() {
  let started;
  let release;
  const ready = new Promise((resolve) => { started = resolve; });
  const pending = new Promise((resolve) => { release = resolve; });
  return {
    ready, release,
    crypto: {
      getRandomValues: (bytes) => webcrypto.getRandomValues(bytes),
      subtle: {
        async digest(algorithm, bytes) {
          assert.equal(algorithm, "SHA-256");
          started();
          await pending;
          return webcrypto.subtle.digest(algorithm, bytes);
        },
      },
    },
  };
}

test("reserved child mutations have no top effects and do not consume the root snapshot", async () => {
  const fixture = await valueReceiptFixture();
  for (const action of ["activate", "focus", "set_value", "scroll"]) {
    const snapshot = await fixture.read();
    const mutation = fixture.mutation(snapshot, "Must not write");
    mutation.action = action;
    mutation.args.nodeId = `child:17:child-document:${mutation.args.nodeId}`;
    mutation.args.direction = "down";
    const rejected = await fixture.command(mutation);
    assert.equal(rejected.code, "read_only_child");
    assert.deepEqual(fixture.fields.map((field) => field.value), ["First initial ✓", "Second initial 🪷"]);
    assert.deepEqual(fixture.fields.map((field) => field.events), [[], []]);
    const top = fixture.mutation(snapshot, "unused");
    top.action = "focus";
    assert.equal((await fixture.command(top)).result.focused, true);
    assert.equal((await fixture.command(top)).code, "stale_snapshot");
  }
});

test("packaged content bridge never registers a child document", () => {
  const context = vm.createContext({ window: { top: {} }, NovaSemantic: {}, chrome: {
    runtime: { onMessage: { addListener() { assert.fail("child listener"); } } },
  } });
  vm.runInContext(contentSource, context);
  assert.equal(context.NovaContentBridge, undefined);
});

test("missing crypto.subtle rejects a value receipt before writing or emitting events", async () => {
  const fixture = await valueReceiptFixture({ getRandomValues: (bytes) => webcrypto.getRandomValues(bytes) });
  const snapshot = await fixture.read();
  const value = "Attempt 中文 🪷 ✓";
  const mutation = fixture.mutation(snapshot, value);
  const response = await fixture.command(mutation);
  assert.equal(fixture.fields[1].value, "Second initial 🪷", "receipt failure must precede mutation");
  assert.equal(fixture.fields[0].value, "First initial ✓");
  assert.deepEqual(fixture.fields.map((field) => field.events), [[], []]);
  assert.equal(response.ok, false);
  assert.equal(response.code, "value_receipt_unavailable");
  assert.equal(JSON.stringify(response).includes(value), false);
  assert.equal((await fixture.command(mutation)).code, "stale_snapshot");
  const fresh = await fixture.read();
  assert.equal(fresh.result.nodes[1].value.text, "Second initial 🪷");
});

test("receipt preparation failures are constant errors without mutation or plaintext", async (t) => {
  const value = "Attempt 中文 🪷 ✓";
  const cases = [
    ["throws", () => { throw new Error(value); }],
    ["rejects", () => Promise.reject(new Error(value))],
    ["invalid receipt", () => Promise.resolve(new ArrayBuffer(8))],
  ];
  for (const [name, digest] of cases) {
    await t.test(name, async () => {
      const fixture = await valueReceiptFixture({ getRandomValues: (bytes) => webcrypto.getRandomValues(bytes), subtle: { digest } });
      const snapshot = await fixture.read();
      const mutation = fixture.mutation(snapshot, value);
      const response = await fixture.command(mutation);
      assert.equal(response.code, "value_receipt_unavailable");
      assert.equal(JSON.stringify(response).includes(value), false);
      assert.equal(fixture.fields[1].value, "Second initial 🪷");
      assert.deepEqual(fixture.fields.map((field) => field.events), [[], []]);
      assert.equal((await fixture.command(mutation)).code, "stale_snapshot");
    });
  }
});

test("pending receipt writes only its exact target and prepared value after unchanged registration", async () => {
  const receipt = pendingReceipt();
  const fixture = await valueReceiptFixture(receipt.crypto);
  const snapshot = await fixture.read();
  assert.equal(snapshot.result.nodes[0].name, snapshot.result.nodes[1].name);
  assert.notEqual(snapshot.result.nodes[0].nodeId, snapshot.result.nodes[1].nodeId);
  const value = "Prepared Unicode 🪷 ✓";
  const mutation = fixture.mutation(snapshot, value);
  const responsePromise = fixture.command(mutation);
  await receipt.ready;
  assert.equal(fixture.fields[1].value, "Second initial 🪷");
  assert.deepEqual(fixture.fields.map((field) => field.events), [[], []]);
  assert.equal((await fixture.command(mutation)).code, "stale_snapshot", "the attempt is consumed while preparation waits");
  await fixture.enable(); // A new object for the same route is not a revocation.
  mutation.args.value = "Later argument must not replace prepared input";
  receipt.release();
  const response = await responsePromise;
  assert.equal(response.ok, true);
  assert.equal(response.result.valueSha256, createHash("sha256").update(value).digest("hex"));
  assert.equal(response.result.valueUtf8Bytes, new TextEncoder().encode(value).byteLength);
  assert.equal(JSON.stringify(response).includes(value), false);
  assert.equal(fixture.fields[1].value, value);
  assert.equal(fixture.fields[0].value, "First initial ✓");
  assert.deepEqual(fixture.fields.map((field) => field.events), [[], ["input", "change"]]);
  assert.equal((await fixture.command(mutation)).code, "stale_snapshot");
  assert.equal((await fixture.read()).result.nodes[1].value.text, value);
});

for (const [name, code, invalidate] of [
  ["revocation", "page_access_revoked", (fixture) => fixture.revoke()],
  ["revocation then same-document enable", "stale_snapshot", async (fixture) => { fixture.revoke(); await fixture.enable(); }],
  ["pagehide", "route_mismatch", (fixture) => fixture.pagehide()],
  ["pagehide then enable", "stale_snapshot", async (fixture) => { fixture.pagehide(); await fixture.enable(); }],
  ["replacement document route", "route_mismatch", async (fixture) => { fixture.route.documentId = "replacement-document"; await fixture.enable(); }],
  ["new private host", "sensitive_control", (fixture) => { fixture.host.attributes["data-private"] = ""; }],
  ["disconnected target", "stale_node", (fixture) => { fixture.fields[1].isConnected = false; }],
  ["fresh read", "stale_snapshot", async (fixture) => { assert.equal((await fixture.read()).ok, true); }],
  ["failed read", "stale_snapshot", async (fixture) => {
    const document = fixture.context.document;
    const body = document.body;
    Object.defineProperty(document, "body", { configurable: true, get() { throw new Error("controlled read failure"); } });
    assert.equal((await fixture.read()).code, "content_failure");
    Object.defineProperty(document, "body", { configurable: true, value: body });
  }],
  ["read then another consumed action (ABA)", "stale_snapshot", async (fixture) => {
    const fresh = await fixture.read();
    const focus = fixture.mutation(fresh, "unused", 0);
    focus.action = "focus";
    assert.equal((await fixture.command(focus)).result.focused, true);
  }],
]) {
  test(`pending receipt rejects ${name} before any value or events`, async () => {
    const receipt = pendingReceipt();
    const fixture = await valueReceiptFixture(receipt.crypto);
    const snapshot = await fixture.read();
    const value = "Must not write 中文 🪷 ✓";
    const responsePromise = fixture.command(fixture.mutation(snapshot, value));
    await receipt.ready;
    await invalidate(fixture);
    receipt.release();
    const response = await responsePromise;
    assert.equal(response.ok, false);
    assert.equal(response.code, code);
    assert.equal(JSON.stringify(response).includes(value), false);
    assert.deepEqual(fixture.fields.map((field) => field.value), ["First initial ✓", "Second initial 🪷"]);
    assert.deepEqual(fixture.fields.map((field) => field.events), [[], []]);
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

test("real content handler preserves shadow routes, exact controls and one mutation per snapshot", async () => {
  const rect = { x: 1, y: 2, width: 20, height: 15 };
  const document = {
    nodeType: 9, readyState: "complete", title: "Open shadow handler", addEventListener() {},
    defaultView: { Event, getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }) },
    createRange() {
      return { setStart() {}, setEnd() {}, getClientRects: () => [rect], getBoundingClientRect: () => rect };
    },
  };
  function element(tagName, attributes = {}) {
    return {
      nodeType: 1, tagName, attributes, isConnected: true, ownerDocument: document,
      getAttribute(name) { return this.attributes[name] ?? null; },
      hasAttribute(name) { return Object.hasOwn(this.attributes, name); }, closest: () => null,
      getClientRects: () => [rect], getBoundingClientRect: () => rect,
      getRootNode() { let node = this; while (node.parentNode) node = node.parentNode; return node; },
    };
  }
  function text(data) { return { nodeType: 3, data, length: data.length, isConnected: true, ownerDocument: document }; }
  function append(parent, ...children) {
    parent.firstChild = children[0];
    children.forEach((child, index) => {
      child.parentNode = parent;
      child.parentElement = parent.nodeType === 1 ? parent : null;
      child.nextSibling = children[index + 1];
    });
  }
  const body = element("BODY");
  body.parentNode = document;
  document.body = body;
  const host = element("DIV");
  const root = { nodeType: 11, host, getElementById: (id) => id === "message-name" ? label : null };
  host.shadowRoot = root;
  const label = element("SPAN");
  append(label, text("Shadow message 🪷"));
  const input = element("INPUT", { "aria-labelledby": "message-name" });
  input.value = "Initial Unicode ✓";
  input.events = [];
  input.dispatchEvent = (event) => { input.events.push(event.type); return true; };
  input.focus = () => { root.activeElement = input; document.activeElement = host; };
  const first = element("BUTTON");
  const second = element("BUTTON");
  append(first, text("Increment"));
  append(second, text("Increment"));
  let firstCount = 0;
  let secondCount = 0;
  first.click = () => { firstCount += 1; };
  second.click = () => { secondCount += 1; };
  const readOnly = text("Shadow read-only 🪷");
  append(root, readOnly, label, input, first, second);
  append(body, host);

  let listener;
  const route = { tabId: 66, documentId: "shadow-document", nonce: null };
  const runtime = {
    id: "nova-shadow-test", lastError: null,
    onMessage: { addListener(value) { listener = value; } },
    sendMessage(message, callback) { route.nonce = message.nonce; callback({ ok: true, route: { ...route } }); },
  };
  const window = {};
  window.top = window;
  const context = vm.createContext({
    addEventListener() {}, chrome: { runtime }, crypto: webcrypto, document,
    location: { href: "https://shadow.example/" }, window, TextEncoder, setTimeout, clearTimeout,
  });
  vm.runInContext(semanticSource, context);
  await vm.runInContext(contentSource, context);
  const sender = { id: runtime.id };
  const envelope = { channel: "nova-extension-v1", type: "semantic_command", route: { ...route, epoch: 7 } };
  const read = () => invoke(listener, { ...envelope, action: "read", args: {} }, sender);
  const mutate = (snapshot, action, node, args = {}) => invoke(listener, {
    ...envelope, action, args: { snapshotId: snapshot.result.snapshotId, nodeId: node.nodeId, ...args },
  }, sender);

  let snapshot = await read();
  assert.equal(snapshot.ok, true);
  assert.equal(snapshot.result.coverage, "top_document");
  assert.equal(snapshot.result.truncated, false);
  assert.equal(snapshot.result.nodes.map((node) => node.name).join("|"), "Shadow read-only 🪷|Shadow message 🪷|Shadow message 🪷|Increment|Increment");
  const buttons = snapshot.result.nodes.filter((node) => node.role === "button");
  const wrongRoute = await invoke(listener, { ...envelope, route: { ...envelope.route, documentId: "other-document" }, action: "activate",
    args: { snapshotId: snapshot.result.snapshotId, nodeId: buttons[1].nodeId } }, sender);
  assert.equal(wrongRoute.code, "route_mismatch");
  assert.equal((await mutate(snapshot, "activate", buttons[1])).result.activated, true);
  assert.equal(firstCount, 0);
  assert.equal(secondCount, 1);
  assert.equal((await mutate(snapshot, "activate", buttons[0])).code, "stale_snapshot");

  snapshot = await read();
  let field = snapshot.result.nodes.find((node) => node.role === "textbox");
  assert.equal((await mutate(snapshot, "focus", field)).result.focused, true);
  assert.equal(root.activeElement, input);
  assert.equal(document.activeElement, host);
  snapshot = await read();
  field = snapshot.result.nodes.find((node) => node.role === "textbox");
  const value = "更新 Unicode 🪷 ✓";
  assert.equal((await mutate(snapshot, "set_value", field, { value })).result.valueUtf8Bytes, new TextEncoder().encode(value).byteLength);
  assert.equal(input.value, value);
  assert.deepEqual(input.events, ["input", "change"]);
  snapshot = await read();
  assert.equal(snapshot.result.nodes.find((node) => node.role === "textbox").value.text, value);
  const textNode = snapshot.result.nodes.find((node) => node.name === "Shadow read-only 🪷");
  assert.equal(textNode.actions.length, 0);
  assert.equal((await mutate(snapshot, "activate", textNode)).code, "unsupported_action");
  assert.equal((await mutate(snapshot, "activate", textNode)).code, "stale_snapshot");

  snapshot = await read();
  const oldButton = snapshot.result.nodes.find((node) => node.role === "button");
  host.attributes["data-private"] = "";
  assert.equal((await mutate(snapshot, "activate", oldButton)).code, "sensitive_control");
  assert.equal((await mutate(snapshot, "activate", oldButton)).code, "stale_snapshot");
  assert.equal(firstCount, 0);
  assert.equal(secondCount, 1);
  assert.equal((await read()).result.nodes.length, 0);
});
