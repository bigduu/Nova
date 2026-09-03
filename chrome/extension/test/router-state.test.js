import assert from "node:assert/strict";
import test from "node:test";

import { PAIR_TTL_MS, RECEIPT_TTL_MS, ProtocolError } from "../lib/protocol.js";
import { RouterState } from "../lib/router-state.js";

function harness() {
  let now = 1_000;
  let nextId = 0;
  const state = new RouterState({
    now: () => now,
    randomId: (prefix) => `${prefix}-${++nextId}`,
  });
  return {
    state,
    advance(milliseconds) {
      now += milliseconds;
    },
  };
}

function request(requestId, action = "status", route, args = {}) {
  const value = {
    protocolVersion: 1,
    kind: "request",
    requestId,
    action,
    args,
  };
  if (route !== undefined) value.route = route;
  return value;
}

const baseRoute = Object.freeze({
  tabId: 11,
  documentId: "doc-11",
  nonce: "page-11",
});

function pair(state, requestId = "pair-1") {
  state.connectNative();
  state.registerTopFrame(baseRoute, {
    url: "https://paired.example/account",
    title: "Paired account",
  });
  const pending = state.begin(request(requestId, "pair"));
  assert.equal(pending.execute, false);
  return state.confirmPair(baseRoute);
}

function throwsCode(fn, code) {
  assert.throws(fn, (error) => error instanceof ProtocolError && error.code === code);
}

test("disconnected requests fail closed and mark the error retryable", () => {
  const { state } = harness();
  const decision = state.begin(request("status-1"));
  assert.equal(decision.execute, false);
  assert.equal(decision.response.status, "error");
  assert.equal(decision.response.error.code, "native_disconnected");
  assert.equal(decision.response.error.retryable, true);
  assert.ok(decision.response.receipt.receiptId);
});

test("connected status reports no route before pairing", () => {
  const { state } = harness();
  state.connectNative();
  const response = state.begin(request("status-1")).response;
  assert.deepEqual(response.result, {
    connected: true,
    paired: false,
    route: null,
    pendingPair: null,
    epoch: 1,
    registeredTopFrames: 0,
    lastRevocation: null,
  });
});

test("pairing binds the exact registered document and metadata", () => {
  const { state } = harness();
  const response = pair(state);
  assert.equal(response.status, "ok");
  assert.equal(response.result.paired, true);
  assert.deepEqual(response.result.route, { ...baseRoute, epoch: 2 });
  assert.equal(Object.isFrozen(response.result.route), true);
  assert.equal(response.result.url, "https://paired.example/account");
  assert.equal(response.result.title, "Paired account");
  assert.equal(state.status().paired, true);
});

test("pairing rejects an unregistered active document", () => {
  const { state } = harness();
  state.connectNative();
  state.begin(request("pair-1", "pair"));
  throwsCode(() => state.confirmPair(baseRoute), "route_not_registered");
});

test("only one pair request may be pending", () => {
  const { state } = harness();
  state.connectNative();
  state.begin(request("pair-1", "pair"));
  const second = state.begin(request("pair-2", "pair"));
  assert.equal(second.response.error.code, "pair_in_progress");
});

test("an existing pair must be released before another pair", () => {
  const { state } = harness();
  pair(state);
  const second = state.begin(request("pair-2", "pair"));
  assert.equal(second.response.error.code, "already_paired");
});

test("pending pairing expires at the security deadline", () => {
  const { state, advance } = harness();
  state.connectNative();
  state.begin(request("pair-1", "pair"));
  advance(PAIR_TTL_MS);
  const response = state.expirePendingPair();
  assert.equal(response.status, "error");
  assert.equal(response.error.code, "pair_expired");
  assert.equal(state.status().pendingPair, null);
  throwsCode(() => state.confirmPair(baseRoute), "no_pending_pair");
});

test("user denial completes the pending request without creating a pair", () => {
  const { state } = harness();
  state.connectNative();
  state.begin(request("pair-1", "pair"));
  const response = state.denyPair();
  assert.equal(response.error.code, "pair_denied");
  assert.equal(state.status().paired, false);
  throwsCode(() => state.denyPair(), "no_pending_pair");
});

test("routed reads execute only on the exact paired route", () => {
  const { state } = harness();
  const paired = pair(state).result.route;
  const decision = state.begin(request("read-1", "read", paired, { maxNodes: 5 }));
  assert.equal(decision.execute, true);
  assert.deepEqual(decision.route, paired);
});

test("routed requests reject a wrong document, nonce, or epoch", () => {
  const mutations = [
    { documentId: "other-doc" },
    { nonce: "other-page" },
    { epoch: 99 },
  ];
  for (const [index, mutation] of mutations.entries()) {
    const { state } = harness();
    const paired = pair(state).result.route;
    const decision = state.begin(
      request(`read-${index}`, "read", { ...paired, ...mutation }),
    );
    assert.equal(decision.response.error.code, "route_mismatch");
  }
});

test("missing route registration revokes a formerly paired document", () => {
  const { state } = harness();
  const paired = pair(state).result.route;
  state.routes.delete(paired.tabId);
  const decision = state.begin(request("read-1", "read", paired));
  assert.equal(decision.response.error.code, "route_revoked");
  assert.equal(state.status().paired, false);
  assert.equal(state.lastRevocation.reason, "route_disappeared");
});

test("successful content completion is bound to request, action, route, and epoch", () => {
  const { state } = harness();
  const paired = pair(state).result.route;
  state.begin(request("read-1", "read", paired));
  const response = state.complete("read-1", "read", paired, { snapshotId: "snapshot-1" });
  assert.equal(response.status, "ok");
  assert.deepEqual(response.result, { snapshotId: "snapshot-1" });
  assert.deepEqual(response.route, paired);
});

test("content completion with the wrong action is ambiguous", () => {
  const { state } = harness();
  const paired = pair(state).result.route;
  state.begin(request("read-1", "read", paired));
  const response = state.complete("read-1", "activate", paired, {});
  assert.equal(response.status, "ambiguous");
  assert.equal(response.error.code, "action_mismatch");
});

test("content completion with a stale route is ambiguous", () => {
  const { state } = harness();
  const paired = pair(state).result.route;
  state.begin(request("read-1", "read", paired));
  const response = state.complete("read-1", "read", { ...paired, nonce: "other" }, {});
  assert.equal(response.status, "ambiguous");
  assert.equal(response.error.code, "route_mismatch");
});

test("content completion without an in-flight request is ambiguous", () => {
  const { state } = harness();
  state.connectNative();
  const response = state.complete("read-1", "read", null, {});
  assert.equal(response.status, "ambiguous");
  assert.equal(response.error.code, "ambiguous_result");
});

test("explicit content failures retain the paired route", () => {
  const { state } = harness();
  const paired = pair(state).result.route;
  state.begin(request("read-1", "read", paired));
  const response = state.failExecution(
    "read-1",
    "read",
    paired,
    "dom_action_failed",
    "DOM action failed",
  );
  assert.equal(response.status, "error");
  assert.equal(response.error.code, "dom_action_failed");
  assert.equal(response.error.retryable, false);
  assert.deepEqual(response.route, paired);
  assert.equal(state.status().paired, true);
});

test("ambiguous transport failure revokes authority and stores a current receipt", () => {
  const { state } = harness();
  const paired = pair(state).result.route;
  state.begin(request("mutation-1", "set_value", paired));
  const response = state.failTransportAmbiguity(
    "mutation-1",
    "set_value",
    paired,
    "ambiguous_content_timeout",
    "content response timed out; the action may have completed",
  );

  assert.equal(response.status, "ambiguous");
  assert.equal(response.error.code, "ambiguous_content_timeout");
  assert.equal(response.error.retryable, false);
  assert.deepEqual(response.route, paired);
  assert.equal(response.epoch, paired.epoch + 1);
  assert.equal(state.status().paired, false);
  assert.equal(state.lastRevocation.reason, "content_transport_ambiguous");

  assert.deepEqual(
    state.acknowledge({
      protocolVersion: 1,
      kind: "receipt",
      receiptId: response.receipt.receiptId,
      requestId: response.requestId,
      action: response.action,
      epoch: response.epoch,
    }),
    { ok: true },
  );

  const retry = state.begin(request("mutation-2", "set_value", paired));
  assert.equal(retry.response.error.code, "not_paired");
});

test("a late old transport rejection does not revoke a newer pairing", () => {
  const { state } = harness();
  const oldRoute = pair(state).result.route;
  state.begin(request("mutation-1", "set_value", oldRoute));
  state.revoke("navigation");
  state.registerTopFrame(baseRoute);
  state.begin(request("pair-2", "pair"));
  const newRoute = state.confirmPair(baseRoute).result.route;

  const response = state.failTransportAmbiguity(
    "mutation-1",
    "set_value",
    oldRoute,
    "ambiguous_content_timeout",
    "content response timed out; the action may have completed",
  );
  assert.equal(response.status, "ambiguous");
  assert.equal(state.status().paired, true);
  assert.deepEqual(state.status().route, newRoute);
});

test("release revokes the route and advances the epoch", () => {
  const { state } = harness();
  const paired = pair(state).result.route;
  const response = state.begin(request("release-1", "release", paired)).response;
  assert.equal(response.result.released, true);
  assert.deepEqual(response.result.previousRoute, paired);
  assert.equal(response.epoch, 3);
  assert.equal(state.status().paired, false);
  assert.equal(state.lastRevocation.reason, "released");
});

test("top-frame replacement revokes only the matching paired document", () => {
  const { state } = harness();
  pair(state);
  state.registerTopFrame({ ...baseRoute, documentId: "doc-12", nonce: "page-12" });
  assert.equal(state.status().paired, false);
  assert.equal(state.lastRevocation.reason, "document_replaced");
});

test("unrelated top-frame replacement leaves the pair intact", () => {
  const { state } = harness();
  pair(state);
  state.registerTopFrame({ tabId: 12, documentId: "doc-a", nonce: "page-a" });
  state.registerTopFrame({ tabId: 12, documentId: "doc-b", nonce: "page-b" });
  assert.equal(state.status().paired, true);
});

test("tab removal revokes its pair and ignores unknown tabs", () => {
  const { state } = harness();
  pair(state);
  assert.equal(state.unregisterTab(999), false);
  assert.equal(state.unregisterTab(baseRoute.tabId), true);
  assert.equal(state.status().paired, false);
  assert.equal(state.lastRevocation.reason, "tab_closed");
});

test("native disconnect and reconnect both revoke prior authority", () => {
  const { state } = harness();
  pair(state);
  state.disconnectNative();
  assert.equal(state.status().connected, false);
  assert.equal(state.lastRevocation.reason, "native_disconnect");

  state.connectNative();
  state.registerTopFrame(baseRoute);
  state.begin(request("pair-2", "pair"));
  state.confirmPair(baseRoute);
  state.connectNative();
  assert.equal(state.status().paired, false);
  assert.equal(state.lastRevocation.reason, "native_reconnect");
});

test("request IDs cannot be replayed, even after revocation", () => {
  const { state } = harness();
  const paired = pair(state);
  state.begin(request("release-1", "release", paired.result.route));
  throwsCode(() => state.begin(request("release-1", "release", paired.result.route)), "replayed_request");
});

test("reusing a request ID for a different action is ambiguous", () => {
  const { state } = harness();
  state.connectNative();
  state.begin(request("same-id", "status"));
  throwsCode(() => state.begin(request("same-id", "pair")), "ambiguous_request");
});

test("valid receipts are one-shot acknowledgements", () => {
  const { state } = harness();
  state.connectNative();
  const response = state.begin(request("status-1")).response;
  const receipt = {
    protocolVersion: 1,
    kind: "receipt",
    receiptId: response.receipt.receiptId,
    requestId: response.requestId,
    action: response.action,
    epoch: response.epoch,
  };
  assert.deepEqual(state.acknowledge(receipt), { ok: true });
  assert.deepEqual(state.acknowledge(receipt), {
    ok: false,
    code: "unknown_or_expired_receipt",
  });
});

test("receipt identity mismatch is rejected and consumed", () => {
  const { state } = harness();
  state.connectNative();
  const response = state.begin(request("status-1")).response;
  const mismatched = {
    protocolVersion: 1,
    kind: "receipt",
    receiptId: response.receipt.receiptId,
    requestId: "other-request",
    action: response.action,
    epoch: response.epoch,
  };
  assert.deepEqual(state.acknowledge(mismatched), { ok: false, code: "ambiguous_receipt" });
  assert.deepEqual(state.acknowledge({ ...mismatched, requestId: response.requestId }), {
    ok: false,
    code: "unknown_or_expired_receipt",
  });
});

test("expired receipts cannot be acknowledged", () => {
  const { state, advance } = harness();
  state.connectNative();
  const response = state.begin(request("status-1")).response;
  advance(RECEIPT_TTL_MS);
  assert.deepEqual(
    state.acknowledge({
      protocolVersion: 1,
      kind: "receipt",
      receiptId: response.receipt.receiptId,
      requestId: response.requestId,
      action: response.action,
      epoch: response.epoch,
    }),
    { ok: false, code: "unknown_or_expired_receipt" },
  );
});
