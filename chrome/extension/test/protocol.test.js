import assert from "node:assert/strict";
import test from "node:test";

import {
  ACTIONS,
  MAX_WIRE_BYTES,
  PROTOCOL_VERSION,
  ProtocolError,
  assertWireSize,
  errorPayload,
  isKnownAction,
  isOpaqueId,
  isPlainObject,
  isRoutedAction,
  normalizeRoute,
  randomOpaqueId,
  redactForDiagnostic,
  sameRoute,
  sha256Text,
  validateReceipt,
  validateRequest,
  wireSize,
} from "../lib/protocol.js";

const route = Object.freeze({
  tabId: 7,
  documentId: "doc-7",
  nonce: "page-0123456789abcdef",
  epoch: 3,
});

function request(overrides = {}) {
  return {
    protocolVersion: PROTOCOL_VERSION,
    kind: "request",
    requestId: "request-1",
    action: "status",
    args: {},
    ...overrides,
  };
}

function throwsCode(fn, code) {
  assert.throws(fn, (error) => error instanceof ProtocolError && error.code === code);
}

test("protocol action set is explicit and immutable", () => {
  assert.deepEqual(ACTIONS, [
    "status",
    "pair",
    "release",
    "read",
    "activate",
    "focus",
    "set_value",
    "scroll",
  ]);
  assert.equal(Object.isFrozen(ACTIONS), true);
  assert.equal(isKnownAction("read"), true);
  assert.equal(isKnownAction("click"), false);
  assert.equal(isRoutedAction("read"), true);
  assert.equal(isRoutedAction("pair"), false);
});

test("plain object validation excludes arrays, dates, and class instances", () => {
  assert.equal(isPlainObject({}), true);
  assert.equal(isPlainObject(Object.create(null)), true);
  assert.equal(isPlainObject([]), false);
  assert.equal(isPlainObject(new Date()), false);
  assert.equal(isPlainObject(new (class Example {})()), false);
  assert.equal(isPlainObject(null), false);
});

test("opaque IDs accept only bounded bridge-safe characters", () => {
  assert.equal(isOpaqueId("abc.DEF_9:-"), true);
  assert.equal(isOpaqueId(""), false);
  assert.equal(isOpaqueId("has space"), false);
  assert.equal(isOpaqueId("slash/not-allowed"), false);
  assert.equal(isOpaqueId("x".repeat(161)), false);
  assert.equal(isOpaqueId(42), false);
});

test("route normalization strips untrusted extra fields and freezes output", () => {
  const normalized = normalizeRoute({ ...route, injected: "ignored" });
  assert.deepEqual(normalized, route);
  assert.equal(Object.isFrozen(normalized), true);
  assert.equal(Object.hasOwn(normalized, "injected"), false);
});

test("route normalization can validate an epoch-free registration route", () => {
  const normalized = normalizeRoute({ ...route, epoch: undefined }, false);
  assert.deepEqual(normalized, {
    tabId: route.tabId,
    documentId: route.documentId,
    nonce: route.nonce,
  });
});

for (const [name, invalid] of [
  ["non-object", null],
  ["negative tab", { ...route, tabId: -1 }],
  ["fractional tab", { ...route, tabId: 1.5 }],
  ["unsafe document ID", { ...route, documentId: "bad/id" }],
  ["empty nonce", { ...route, nonce: "" }],
  ["zero epoch", { ...route, epoch: 0 }],
]) {
  test(`route normalization rejects ${name}`, () => {
    throwsCode(() => normalizeRoute(invalid), "invalid_route");
  });
}

test("route comparison optionally ignores pairing epoch", () => {
  assert.equal(sameRoute(route, { ...route }), true);
  assert.equal(sameRoute(route, { ...route, epoch: 4 }), false);
  assert.equal(sameRoute(route, { ...route, epoch: 4 }, false), true);
  assert.equal(sameRoute(route, { ...route, nonce: "different" }, false), false);
  assert.equal(sameRoute(route, null), false);
});

test("valid requests preserve their original envelope", () => {
  const message = request();
  assert.equal(validateRequest(message), message);
  const routed = request({ action: "read", route });
  assert.equal(validateRequest(routed), routed);
});

for (const [name, overrides, code] of [
  ["non-object envelope", null, "invalid_message"],
  ["wrong version", { protocolVersion: 2 }, "unsupported_protocol"],
  ["wrong kind", { kind: "event" }, "invalid_kind"],
  ["unsafe request ID", { requestId: "../../escape" }, "invalid_request_id"],
  ["unknown action", { action: "navigate" }, "unknown_action"],
  ["array args", { args: [] }, "invalid_args"],
  ["missing routed route", { action: "read" }, "invalid_route"],
]) {
  test(`request validation rejects ${name}`, () => {
    const value = overrides === null ? null : request(overrides);
    throwsCode(() => validateRequest(value), code);
  });
}

test("non-routed requests still validate a supplied route", () => {
  throwsCode(
    () => validateRequest(request({ route: { ...route, nonce: "bad nonce" } })),
    "invalid_route",
  );
});

test("receipt validation accepts an exact receipt envelope", () => {
  const receipt = {
    protocolVersion: PROTOCOL_VERSION,
    kind: "receipt",
    receiptId: "receipt-1",
    requestId: "request-1",
    action: "read",
    epoch: 3,
  };
  assert.equal(validateReceipt(receipt), receipt);
});

for (const [name, mutate] of [
  ["wrong kind", { kind: "result" }],
  ["wrong version", { protocolVersion: 9 }],
  ["unsafe receipt ID", { receiptId: "bad receipt" }],
  ["unknown action", { action: "navigate" }],
  ["invalid epoch", { epoch: 0 }],
]) {
  test(`receipt validation rejects ${name}`, () => {
    throwsCode(
      () =>
        validateReceipt({
          protocolVersion: PROTOCOL_VERSION,
          kind: "receipt",
          receiptId: "receipt-1",
          requestId: "request-1",
          action: "read",
          epoch: 3,
          ...mutate,
        }),
      "invalid_receipt",
    );
  });
}

test("wire size counts UTF-8 bytes rather than JavaScript code units", () => {
  assert.equal(wireSize("é"), 4); // JSON quotes plus two UTF-8 bytes.
  assert.equal(assertWireSize({ ok: true }).ok, true);
});

test("wire size rejects messages beyond the 1 MiB bridge limit", () => {
  const oversized = { value: "x".repeat(MAX_WIRE_BYTES) };
  throwsCode(() => assertWireSize(oversized), "message_too_large");
});

test("random IDs retain the requested safe prefix and have entropy", () => {
  const first = randomOpaqueId("receipt");
  const second = randomOpaqueId("receipt");
  assert.match(first, /^receipt-[0-9a-f]{32}$/u);
  assert.notEqual(first, second);
  assert.equal(isOpaqueId(first), true);
});

test("diagnostics redact set_value plaintext and retain verifiable metadata", async () => {
  const secret = "correct horse 🐴";
  const diagnostic = await redactForDiagnostic(
    request({
      action: "set_value",
      route,
      args: { snapshotId: "snapshot-1", nodeId: "n1", value: secret },
    }),
  );
  assert.equal(JSON.stringify(diagnostic).includes(secret), false);
  assert.equal(diagnostic.args.valueUtf8Bytes, new TextEncoder().encode(secret).byteLength);
  assert.equal(diagnostic.args.valueSha256, await sha256Text(secret));
  assert.equal(diagnostic.args.snapshotId, "snapshot-1");
  assert.equal(diagnostic.args.nodeId, "n1");
});

test("non-sensitive diagnostics preserve action arguments", async () => {
  const args = { maxNodes: 100 };
  const diagnostic = await redactForDiagnostic(request({ action: "read", route, args }));
  assert.equal(diagnostic.args, args);
});

test("error payloads normalize retryable to a boolean and are immutable", () => {
  const payload = errorPayload("offline", "not connected", 1);
  assert.deepEqual(payload, { code: "offline", message: "not connected", retryable: true });
  assert.equal(Object.isFrozen(payload), true);
});
