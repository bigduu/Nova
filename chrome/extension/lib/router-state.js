import {
  MAX_REQUEST_IDS,
  PAIR_TTL_MS,
  RECEIPT_TTL_MS,
  ProtocolError,
  errorPayload,
  isKnownAction,
  isOpaqueId,
  normalizeRoute,
  randomOpaqueId,
  sameRoute,
  validateReceipt,
  validateRequest,
} from "./protocol.js";

function terminal(status, requestId, action, epoch, result, error, route = null) {
  const response = {
    protocolVersion: 1,
    kind: "result",
    requestId,
    action,
    status,
    epoch,
  };
  if (route) response.route = route;
  if (result !== undefined) response.result = result;
  if (error !== undefined) response.error = error;
  return response;
}

/**
 * Security state for one Manifest V3 worker lifetime.
 *
 * `seenRequests` intentionally survives pairing epochs. A release must not make
 * an old request ID executable again. If its bounded capacity is exhausted the
 * state rejects all new requests until the worker restarts, which is safer than
 * evicting IDs and permitting replay.
 */
export class RouterState {
  constructor({ now = () => Date.now(), randomId = randomOpaqueId } = {}) {
    this.now = now;
    this.randomId = randomId;
    this.epoch = 1;
    this.isNativeConnected = false;
    this.routes = new Map();
    this.paired = null;
    this.pendingPair = null;
    this.seenRequests = new Map();
    this.inflight = new Map();
    this.receipts = new Map();
    this.lastRevocation = null;
  }

  connectNative() {
    if (this.isNativeConnected || this.paired || this.pendingPair) {
      this.revoke("native_reconnect");
    }
    this.isNativeConnected = true;
  }

  disconnectNative() {
    this.isNativeConnected = false;
    return this.revoke("native_disconnect");
  }

  registerTopFrame(route, metadata = {}) {
    const normalized = normalizeRoute(route, false);
    const previous = this.routes.get(normalized.tabId);
    if (previous && !sameRoute(previous.route, normalized, false)) {
      if (this.paired && sameRoute(this.paired.route, previous.route, false)) {
        this.revoke("document_replaced");
      }
    }
    this.routes.set(normalized.tabId, {
      route: normalized,
      url: typeof metadata.url === "string" ? metadata.url.slice(0, 2048) : "",
      title: typeof metadata.title === "string" ? metadata.title.slice(0, 512) : "",
      registeredAt: this.now(),
    });
    return normalized;
  }

  unregisterTopFrame(route, reason = "document_unloaded") {
    const normalized = normalizeRoute(route, false);
    const entry = this.routes.get(normalized.tabId);
    if (!entry || !sameRoute(entry.route, normalized, false)) return false;
    this.routes.delete(normalized.tabId);
    if (this.paired && sameRoute(this.paired.route, normalized, false)) {
      this.revoke(reason);
    }
    return true;
  }

  unregisterTab(tabId, reason = "tab_closed") {
    const entry = this.routes.get(tabId);
    if (!entry) return false;
    this.routes.delete(tabId);
    if (this.paired && this.paired.route.tabId === tabId) this.revoke(reason);
    return true;
  }

  begin(request) {
    validateRequest(request);
    this.purgeExpired();
    this.rememberRequest(request.requestId, request.action);

    if (!this.isNativeConnected) {
      return this.failure(request, "native_disconnected", "Nova.app is not connected", true);
    }

    if (request.action === "status") {
      return { execute: false, response: this.success(request, this.status()) };
    }
    if (request.action === "pair") {
      if (this.pendingPair) {
        return this.failure(request, "pair_in_progress", "another pairing request is pending");
      }
      if (this.paired) {
        return this.failure(request, "already_paired", "release the current route before pairing again");
      }
      this.pendingPair = {
        requestId: request.requestId,
        action: request.action,
        createdAt: this.now(),
        expiresAt: this.now() + PAIR_TTL_MS,
      };
      this.inflight.set(request.requestId, {
        action: request.action,
        epoch: this.epoch,
        route: null,
      });
      return { execute: false, pendingPair: { ...this.pendingPair } };
    }

    const routeFailure = this.checkPairedRoute(request.route);
    if (routeFailure) return this.failure(request, routeFailure.code, routeFailure.message);

    if (request.action === "release") {
      const oldRoute = this.paired.route;
      this.revoke("released");
      return {
        execute: false,
        response: this.success(
          { ...request, route: null },
          { released: true, previousRoute: oldRoute, epoch: this.epoch },
        ),
      };
    }

    this.inflight.set(request.requestId, {
      action: request.action,
      epoch: this.epoch,
      route: this.paired.route,
    });
    return { execute: true, route: this.paired.route };
  }

  confirmPair(activeRoute) {
    this.purgeExpired();
    if (!this.pendingPair) {
      throw new ProtocolError("no_pending_pair", "there is no live pairing request");
    }
    if (this.pendingPair.expiresAt <= this.now()) {
      throw new ProtocolError("pair_expired", "the 30 second pairing window expired");
    }
    const base = normalizeRoute(activeRoute, false);
    const registered = this.routes.get(base.tabId);
    if (!registered || !sameRoute(registered.route, base, false)) {
      throw new ProtocolError("route_not_registered", "the active top-level document is not registered");
    }

    const pair = this.pendingPair;
    this.pendingPair = null;
    this.receipts.clear();
    this.inflight.clear();
    this.epoch += 1;
    const route = Object.freeze({ ...base, epoch: this.epoch });
    this.paired = { route, pairedAt: this.now() };
    this.inflight.set(pair.requestId, { action: "pair", epoch: this.epoch, route });
    return this.success(
      { requestId: pair.requestId, action: "pair", route },
      {
        paired: true,
        route,
        url: registered.url,
        title: registered.title,
      },
    );
  }

  denyPair(code = "pair_denied", message = "pairing was denied by the user") {
    if (!this.pendingPair) {
      throw new ProtocolError("no_pending_pair", "there is no live pairing request");
    }
    const pair = this.pendingPair;
    this.pendingPair = null;
    this.inflight.delete(pair.requestId);
    return this.storeResponse(
      terminal("error", pair.requestId, "pair", this.epoch, undefined, errorPayload(code, message)),
    );
  }

  expirePendingPair() {
    if (!this.pendingPair || this.pendingPair.expiresAt > this.now()) return null;
    const pair = this.pendingPair;
    this.pendingPair = null;
    this.inflight.delete(pair.requestId);
    return this.storeResponse(
      terminal(
        "error",
        pair.requestId,
        "pair",
        this.epoch,
        undefined,
        errorPayload("pair_expired", "the 30 second pairing window expired"),
      ),
    );
  }

  reject(request, code, message, retryable = false) {
    return this.storeResponse(
      terminal(
        code.startsWith("ambiguous") || code === "action_mismatch" ? "ambiguous" : "error",
        request.requestId,
        isKnownAction(request.action) ? request.action : "status",
        this.epoch,
        undefined,
        errorPayload(code, message, retryable),
        request.route ?? null,
      ),
    );
  }

  complete(requestId, action, route, result) {
    if (!isOpaqueId(requestId) || !isKnownAction(action)) {
      throw new ProtocolError("invalid_result", "content result identity is invalid");
    }
    const pending = this.inflight.get(requestId);
    if (!pending) {
      return this.storeResponse(
        terminal(
          "ambiguous",
          requestId,
          action,
          this.epoch,
          undefined,
          errorPayload("ambiguous_result", "there is no matching in-flight request"),
        ),
      );
    }
    if (pending.action !== action) {
      this.inflight.delete(requestId);
      return this.storeResponse(
        terminal(
          "ambiguous",
          requestId,
          action,
          this.epoch,
          undefined,
          errorPayload("action_mismatch", "result action does not match its request"),
          pending.route,
        ),
      );
    }
    if (
      pending.epoch !== this.epoch ||
      (pending.route && !sameRoute(pending.route, route, true))
    ) {
      this.inflight.delete(requestId);
      return this.storeResponse(
        terminal(
          "ambiguous",
          requestId,
          action,
          this.epoch,
          undefined,
          errorPayload("route_mismatch", "result route is stale or does not match"),
        ),
      );
    }
    this.inflight.delete(requestId);
    return this.storeResponse(
      terminal("ok", requestId, action, this.epoch, result, undefined, pending.route),
    );
  }

  failExecution(requestId, action, route, code, message, retryable = false) {
    const pending = this.inflight.get(requestId);
    if (!pending || pending.action !== action) {
      return this.complete(requestId, action, route, undefined);
    }
    this.inflight.delete(requestId);
    return this.storeResponse(
      terminal(
        "error",
        requestId,
        action,
        this.epoch,
        undefined,
        errorPayload(code, message, retryable),
        pending.route,
      ),
    );
  }

  failTransportAmbiguity(requestId, action, route, code, message) {
    const pending = this.inflight.get(requestId);
    const boundRoute = pending?.route ?? route ?? null;

    // Losing the content-script transport does not tell us whether a mutation
    // was applied before its reply was lost. Revoke only the pairing that
    // authorized this request; an old, late rejection must not revoke a newer
    // pairing created at a later epoch.
    if (boundRoute && this.paired && sameRoute(boundRoute, this.paired.route, true)) {
      this.revoke("content_transport_ambiguous");
    } else {
      this.inflight.delete(requestId);
    }

    // Store the terminal after revocation so its receipt belongs to the new
    // epoch and can still be acknowledged by Nova.app.
    return this.storeResponse(
      terminal(
        "ambiguous",
        requestId,
        action,
        this.epoch,
        undefined,
        errorPayload(code, message, false),
        boundRoute,
      ),
    );
  }

  acknowledge(message) {
    validateReceipt(message);
    this.purgeExpired();
    const stored = this.receipts.get(message.receiptId);
    if (!stored) {
      return { ok: false, code: "unknown_or_expired_receipt" };
    }
    if (stored.epoch !== this.epoch || message.epoch !== stored.epoch) {
      this.receipts.delete(message.receiptId);
      return { ok: false, code: "revoked_receipt" };
    }
    if (stored.requestId !== message.requestId || stored.action !== message.action) {
      this.receipts.delete(message.receiptId);
      return { ok: false, code: "ambiguous_receipt" };
    }
    this.receipts.delete(message.receiptId);
    return { ok: true };
  }

  checkPairedRoute(rawRoute) {
    if (!this.paired) return errorPayload("not_paired", "pair a Chrome document first");
    let route;
    try {
      route = normalizeRoute(rawRoute, true);
    } catch (error) {
      return errorPayload(error.code ?? "invalid_route", error.message);
    }
    if (route.epoch !== this.epoch || !sameRoute(route, this.paired.route, true)) {
      return errorPayload("route_mismatch", "route does not match the paired document");
    }
    const registered = this.routes.get(route.tabId);
    if (!registered || !sameRoute(registered.route, route, false)) {
      this.revoke("route_disappeared");
      return errorPayload("route_revoked", "the paired document is no longer registered");
    }
    return null;
  }

  status() {
    this.purgeExpired();
    return {
      connected: this.isNativeConnected,
      paired: Boolean(this.paired),
      route: this.paired?.route ?? null,
      pendingPair: this.pendingPair
        ? { expiresAt: this.pendingPair.expiresAt }
        : null,
      epoch: this.epoch,
      registeredTopFrames: this.routes.size,
      lastRevocation: this.lastRevocation,
    };
  }

  revoke(reason) {
    const previousRoute = this.paired?.route ?? null;
    this.epoch += 1;
    this.paired = null;
    this.pendingPair = null;
    this.inflight.clear();
    this.receipts.clear();
    this.lastRevocation = { reason, at: this.now(), previousRoute };
    return this.lastRevocation;
  }

  purgeExpired() {
    const now = this.now();
    for (const [receiptId, receipt] of this.receipts) {
      if (receipt.expiresAt <= now || receipt.epoch !== this.epoch) {
        this.receipts.delete(receiptId);
      }
    }
  }

  rememberRequest(requestId, action) {
    const previous = this.seenRequests.get(requestId);
    if (previous) {
      const code = previous.action === action ? "replayed_request" : "ambiguous_request";
      const message =
        previous.action === action
          ? "requestId has already been used"
          : "requestId was previously used for a different action";
      throw new ProtocolError(code, message);
    }
    if (this.seenRequests.size >= MAX_REQUEST_IDS) {
      throw new ProtocolError(
        "request_capacity_exhausted",
        "request replay ledger is full; reconnect the extension",
      );
    }
    this.seenRequests.set(requestId, { action, firstSeenAt: this.now() });
  }

  success(request, result) {
    this.inflight.delete(request.requestId);
    return this.storeResponse(
      terminal(
        "ok",
        request.requestId,
        request.action,
        this.epoch,
        result,
        undefined,
        request.route ?? null,
      ),
    );
  }

  failure(request, code, message, retryable = false) {
    this.inflight.delete(request.requestId);
    return {
      execute: false,
      response: this.storeResponse(
        terminal(
          code.startsWith("ambiguous") ? "ambiguous" : "error",
          request.requestId,
          request.action,
          this.epoch,
          undefined,
          errorPayload(code, message, retryable),
          request.route ?? null,
        ),
      ),
    };
  }

  storeResponse(response) {
    const receiptId = this.randomId("receipt");
    const expiresAt = this.now() + RECEIPT_TTL_MS;
    response.receipt = { receiptId, expiresAt };
    this.receipts.set(receiptId, {
      requestId: response.requestId,
      action: response.action,
      epoch: response.epoch,
      expiresAt,
    });
    return response;
  }
}
