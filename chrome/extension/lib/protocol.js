export const PROTOCOL_VERSION = 1;
export const PAIR_TTL_MS = 30_000;
export const RECEIPT_TTL_MS = 30_000;
export const CONTENT_TIMEOUT_MS = 10_000;
export const MAX_WIRE_BYTES = 1024 * 1024;
export const MAX_REQUEST_IDS = 4096;

export const ACTIONS = Object.freeze([
  "status",
  "pair",
  "release",
  "read",
  "activate",
  "focus",
  "set_value",
  "scroll",
]);

export const ROUTED_ACTIONS = Object.freeze([
  "release",
  "read",
  "activate",
  "focus",
  "set_value",
  "scroll",
]);

const ACTION_SET = new Set(ACTIONS);
const ROUTED_ACTION_SET = new Set(ROUTED_ACTIONS);
const SAFE_ID = /^[A-Za-z0-9._:-]+$/u;

export class ProtocolError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "ProtocolError";
    this.code = code;
  }
}
export function isPlainObject(value) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

export function isOpaqueId(value, maxLength = 160) {
  return (
    typeof value === "string" &&
    value.length >= 1 &&
    value.length <= maxLength &&
    SAFE_ID.test(value)
  );
}

export function isKnownAction(action) {
  return typeof action === "string" && ACTION_SET.has(action);
}

export function isRoutedAction(action) {
  return ROUTED_ACTION_SET.has(action);
}

export function normalizeRoute(route, requireEpoch = true) {
  if (!isPlainObject(route)) {
    throw new ProtocolError("invalid_route", "route must be an object");
  }
  if (!Number.isSafeInteger(route.tabId) || route.tabId < 0) {
    throw new ProtocolError("invalid_route", "route.tabId must be a non-negative integer");
  }
  if (!isOpaqueId(route.documentId, 256)) {
    throw new ProtocolError("invalid_route", "route.documentId is invalid");
  }
  if (!isOpaqueId(route.nonce, 128)) {
    throw new ProtocolError("invalid_route", "route.nonce is invalid");
  }
  if (
    requireEpoch &&
    (!Number.isSafeInteger(route.epoch) || route.epoch < 1)
  ) {
    throw new ProtocolError("invalid_route", "route.epoch must be a positive integer");
  }
  const normalized = {
    tabId: route.tabId,
    documentId: route.documentId,
    nonce: route.nonce,
  };
  if (requireEpoch) {
    normalized.epoch = route.epoch;
  }
  return Object.freeze(normalized);
}

export function sameRoute(left, right, includeEpoch = true) {
  if (!left || !right) return false;
  return (
    left.tabId === right.tabId &&
    left.documentId === right.documentId &&
    left.nonce === right.nonce &&
    (!includeEpoch || left.epoch === right.epoch)
  );
}

export function validateRequest(message) {
  if (!isPlainObject(message)) {
    throw new ProtocolError("invalid_message", "message must be an object");
  }
  if (message.protocolVersion !== PROTOCOL_VERSION) {
    throw new ProtocolError("unsupported_protocol", "protocolVersion must be 1");
  }
  if (message.kind !== "request") {
    throw new ProtocolError("invalid_kind", "expected a request message");
  }
  if (!isOpaqueId(message.requestId)) {
    throw new ProtocolError("invalid_request_id", "requestId is invalid");
  }
  if (!isKnownAction(message.action)) {
    throw new ProtocolError("unknown_action", "action is not supported");
  }
  if (isRoutedAction(message.action)) {
    normalizeRoute(message.route, true);
  } else if (message.route !== undefined && message.route !== null) {
    normalizeRoute(message.route, true);
  }
  if (message.args !== undefined && !isPlainObject(message.args)) {
    throw new ProtocolError("invalid_args", "args must be an object");
  }
  return message;
}

export function validateReceipt(message) {
  if (!isPlainObject(message)) {
    throw new ProtocolError("invalid_message", "message must be an object");
  }
  if (message.protocolVersion !== PROTOCOL_VERSION || message.kind !== "receipt") {
    throw new ProtocolError("invalid_receipt", "invalid receipt envelope");
  }
  if (!isOpaqueId(message.receiptId) || !isOpaqueId(message.requestId)) {
    throw new ProtocolError("invalid_receipt", "receipt IDs are invalid");
  }
  if (!isKnownAction(message.action)) {
    throw new ProtocolError("invalid_receipt", "receipt action is invalid");
  }
  if (!Number.isSafeInteger(message.epoch) || message.epoch < 1) {
    throw new ProtocolError("invalid_receipt", "receipt epoch is invalid");
  }
  return message;
}

export function errorPayload(code, message, retryable = false) {
  return Object.freeze({ code, message, retryable: Boolean(retryable) });
}

export function wireSize(value) {
  return new TextEncoder().encode(JSON.stringify(value)).byteLength;
}

export function assertWireSize(value) {
  if (wireSize(value) > MAX_WIRE_BYTES) {
    throw new ProtocolError("message_too_large", "message exceeds the 1 MiB bridge limit");
  }
  return value;
}

export function randomOpaqueId(prefix = "id") {
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  const token = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${prefix}-${token}`;
}

export async function sha256Text(text) {
  const bytes = new TextEncoder().encode(text);
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

export async function redactForDiagnostic(request) {
  const safe = {
    kind: request?.kind,
    requestId: request?.requestId,
    action: request?.action,
    route: request?.route,
  };
  if (request?.action === "set_value" && typeof request?.args?.value === "string") {
    safe.args = {
      snapshotId: request.args.snapshotId,
      nodeId: request.args.nodeId,
      valueUtf8Bytes: new TextEncoder().encode(request.args.value).byteLength,
      valueSha256: await sha256Text(request.args.value),
    };
  } else if (request?.args !== undefined) {
    safe.args = request.args;
  }
  return safe;
}
