(() => {
  "use strict";

  // Manifest configuration already sets all_frames=false. Keep the runtime
  // guard too: a future manifest edit must not silently widen Nova's authority.
  if (window.top !== window || !globalThis.NovaSemantic) return;

  const CHANNEL = "nova-extension-v1";
  const nonceBytes = new Uint8Array(16);
  crypto.getRandomValues(nonceBytes);
  const nonce = `page-${Array.from(nonceBytes, (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("")}`;

  let trustedRoute = null;
  let currentSnapshot = null;
  let registerTimer = null;

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
    chrome.runtime.sendMessage(
      {
        channel: CHANNEL,
        type: "register_top_frame",
        nonce,
        url: location.href,
        title: document.title,
      },
      (response) => {
        if (chrome.runtime.lastError || !response?.ok) {
          clearTimeout(registerTimer);
          registerTimer = setTimeout(register, 1000);
          return;
        }
        trustedRoute = response.route;
      },
    );
  }

  async function handleCommand(message) {
    if (!baseRouteMatches(message.route)) {
      return { ok: false, action: message.action, code: "route_mismatch", message: "content route mismatch" };
    }
    if (message.action === "ping") {
      return { ok: true, action: "ping", route: message.route };
    }
    if (message.action === "read") {
      // Invalidate before touching the DOM so even a failed read cannot leave an
      // older snapshot actionable.
      currentSnapshot = null;
      const snapshot = NovaSemantic.createSnapshot(document, {
        maxNodes: message.args?.maxNodes,
        maxChars: message.args?.maxChars,
      });
      currentSnapshot = {
        id: snapshot.result.snapshotId,
        handles: snapshot.handles,
      };
      return { ok: true, action: "read", route: message.route, result: snapshot.result };
    }

    if (!["activate", "focus", "set_value", "scroll"].includes(message.action)) {
      return { ok: false, action: message.action, code: "unknown_action", message: "unknown content action" };
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
    try {
      const result = await NovaSemantic.performAction(handle, message.action, message.args);
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
      message?.channel !== CHANNEL ||
      message?.type !== "semantic_command"
    ) {
      return false;
    }
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
  register();
})();
