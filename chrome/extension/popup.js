(() => {
  "use strict";

  const CHANNEL = "nova-extension-v1";
  const elements = Object.fromEntries(
    [
      "connection",
      "access",
      "access-origin",
      "access-status",
      "site-scope",
      "use-tab",
      "allow-site",
      "revoke-site",
      "pending",
      "paired",
      "idle",
      "page-title",
      "page-origin",
      "paired-origin",
      "countdown",
      "pair",
      "deny",
      "release",
      "frame-status",
      "enable-frames",
      "remove-frame-permission",
      "error",
    ].map((id) => [id, document.getElementById(id)]),
  );
  let expiry = null;
  let countdownTimer = null;
  let pairingCandidateId = null;
  let reviewedAccess = null;
  let reviewedPair = null;
  let currentChildFrames = null;
  let currentStatus = null;
  let busy = false;
  let refreshPending = false;
  let uiRevision = 0;
  let errorRevision = -1;
  let changeReason = null;

  function originOnly(rawUrl) {
    try {
      return new URL(rawUrl).origin;
    } catch {
      return "Unavailable page";
    }
  }

  function showError(message) {
    elements.error.textContent = String(message || "Operation failed").slice(0, 300);
    elements.error.hidden = false;
    errorRevision = uiRevision;
  }

  function send(type, details = {}) {
    return new Promise((resolve, reject) => {
      chrome.runtime.sendMessage({ channel: CHANNEL, type, ...details }, (response) => {
        if (chrome.runtime.lastError) reject(chrome.runtime.lastError);
        else resolve(response);
      });
    });
  }

  function tickCountdown() {
    if (!expiry) return;
    const remaining = Math.max(0, expiry - Date.now());
    elements.countdown.textContent = `${Math.ceil(remaining / 1000)} seconds remaining`;
    updateControls();
    if (remaining === 0) clearInterval(countdownTimer);
  }

  function updateControls() {
    const blocked = busy || refreshPending;
    elements["use-tab"].disabled = blocked || !reviewedAccess;
    elements["allow-site"].disabled = blocked || !reviewedAccess || Boolean(reviewedAccess.siteAllowed);
    elements["revoke-site"].disabled = blocked || !reviewedAccess?.siteAllowed;
    elements.pair.disabled = blocked || !currentStatus?.connected || !pairingCandidateId || !expiry || expiry <= Date.now();
    elements.deny.disabled = blocked || !currentStatus?.pendingPair;
    elements.release.disabled = blocked || !currentStatus?.paired;
    elements["enable-frames"].disabled = blocked || !reviewedPair || Boolean(currentChildFrames?.enabled);
    elements["remove-frame-permission"].disabled = blocked || !currentChildFrames?.permissionGranted;
  }

  function renderAccess(access) {
    reviewedAccess = access?.sitePattern && Number.isSafeInteger(access.tabId) && typeof access.url === "string"
      ? Object.freeze({ ...access }) : null;
    elements["access-origin"].textContent = access?.origin ?? "No supported web page";
    elements["access-status"].textContent = access?.message ?? "Page access is unavailable. Reopen Nova from a web page.";
    elements["site-scope"].textContent = reviewedAccess
      ? `Site scope: ${access.sitePattern} (all ports, no subdomains). Page access alone does not pair Nova.`
      : "Only HTTP(S) pages are supported. File and incognito access stay off by default.";
    elements["revoke-site"].hidden = !reviewedAccess || !access.siteAllowed;
  }

  function idleMessage(status) {
    if (!status.connected) return "Nova.app unavailable. Start or reconnect Nova, then ask it to pair.";
    const reason = changeReason ?? status.lastRevocation?.reason;
    if (reason === "pair_expired") return "Pair request expired. Ask Nova for a new request and confirm it within 30 seconds.";
    if (["navigation", "document_replaced", "document_unloaded", "tab_closed", "tab_replaced"].includes(reason)) {
      return "The page changed. Enable the intended tab, then ask Nova to pair its new document.";
    }
    if (["permission_removed", "site_permission_removed"].includes(reason)) {
      return "Site access was removed. Enable the tab or allow its site, then ask Nova to pair again.";
    }
    if (["popup_release", "released", "pair_denied"].includes(reason)) return "Pairing ended. Ask Nova for a new Pair request when ready.";
    return "No live pairing request. Ask Nova to pair, then confirm the reviewed document within 30 seconds.";
  }

  function renderState(response) {
    if (!response?.ok) throw new Error(response?.code ?? "Could not read Nova state");
    const { status, activePage, pairedPage, candidateId } = response;
    reviewedPair = status.paired && status.route ? Object.freeze({ ...status.route }) : null;
    currentChildFrames = response.childFrames;
    elements["remove-frame-permission"].hidden = !response.childFrames?.permissionGranted;
    elements["frame-status"].textContent = response.childFrames?.enabled
      ? "Child reads and DOM activation enabled for this pairing. Only proven visible same-origin documents are included."
      : "Top document only. Child reads and activation are off for this pairing.";
    currentStatus = status;
    if (errorRevision !== uiRevision) elements.error.hidden = true;
    renderAccess(response.access);
    elements.connection.textContent = status.connected ? "Nova.app connected" : "Nova.app unavailable";
    elements.pending.hidden = true;
    elements.paired.hidden = true;
    elements.idle.hidden = true;
    pairingCandidateId = null;
    expiry = null;
    clearInterval(countdownTimer);

    if (status.paired) {
      elements.paired.hidden = false;
      elements["paired-origin"].textContent = pairedPage
        ? originOnly(pairedPage.url)
        : "Exact paired document";
      return;
    }
    if (status.pendingPair) {
      elements.pending.hidden = false;
      elements["page-title"].textContent = activePage?.title || (response.access?.sitePattern
        ? "Enable this tab to review its document" : "Unsupported page");
      elements["page-origin"].textContent = activePage ? originOnly(activePage.url)
        : response.access?.origin ?? "Nova cannot access this page";
      pairingCandidateId = typeof candidateId === "string" ? candidateId : null;
      expiry = status.pendingPair.expiresAt;
      tickCountdown();
      countdownTimer = setInterval(tickCountdown, 250);
      return;
    }
    elements.idle.hidden = false;
    elements.idle.textContent = idleMessage(status);
  }

  async function refresh() {
    refreshPending = true;
    if (busy) return updateControls();
    busy = true;
    updateControls();
    try {
      while (refreshPending) {
        refreshPending = false;
        const revision = uiRevision;
        try {
          const response = await send("popup_state");
          if (revision === uiRevision) renderState(response);
        } catch (error) {
          if (revision !== uiRevision) continue;
          currentStatus = null;
          reviewedAccess = null;
          reviewedPair = null;
          currentChildFrames = null;
          pairingCandidateId = null;
          elements.connection.textContent = "Nova state unavailable";
          elements.pending.hidden = true;
          elements.paired.hidden = true;
          elements.idle.hidden = false;
          elements.idle.textContent = "Could not refresh Nova state. Reopen the popup; start Nova if it is unavailable.";
          showError(error.message);
        }
      }
    } finally {
      busy = false;
      updateControls();
    }
  }

  async function runAction(operation) {
    if (busy || refreshPending) return;
    busy = true;
    const revision = ++uiRevision;
    updateControls();
    try {
      await operation();
    } catch (error) {
      if (revision === uiRevision) showError(error.message);
    } finally {
      busy = false;
      await refresh();
    }
  }

  elements["use-tab"].addEventListener("click", () => {
    const access = reviewedAccess;
    return runAction(async () => {
      if (!access) throw new Error("Review a supported web tab first");
      const response = await send("bootstrap_tab", { tabId: access.tabId, url: access.url });
      if (!response?.ok) throw new Error(response?.message ?? response?.code ?? "Could not enable this tab");
    });
  });

  elements["allow-site"].addEventListener("click", () => {
    const access = reviewedAccess;
    return runAction(async () => {
      if (!access) throw new Error("Review a supported site first");
      // runAction invokes this operation synchronously in the click gesture;
      // the permission request precedes any await or worker message.
      const granted = await chrome.permissions.request({ origins: [access.sitePattern] });
      if (!granted) throw new Error("Site permission denied. Use this tab for temporary access, or try allowing the site again.");
    });
  });

  elements["revoke-site"].addEventListener("click", () => {
    const access = reviewedAccess;
    return runAction(async () => {
      if (!access) throw new Error("Review the allowed site first");
      const removed = await chrome.permissions.remove({ origins: [access.sitePattern] });
      if (!removed) throw new Error("Chrome did not remove this site's permission");
    });
  });

  elements.pair.addEventListener("click", () => {
    const candidateId = pairingCandidateId;
    return runAction(async () => {
      pairingCandidateId = null;
      if (!candidateId) throw new Error("Pairing candidate expired; reopen the popup");
      const response = await send("confirm_pair", { candidateId });
      if (!response?.ok) throw new Error(response?.message ?? response?.code ?? "Pair failed");
    });
  });

  elements.deny.addEventListener("click", () => {
    return runAction(async () => {
      const response = await send("deny_pair");
      if (!response?.ok) throw new Error(response?.code ?? "Deny failed");
    });
  });

  elements.release.addEventListener("click", () => {
    return runAction(async () => {
      const response = await send("release_pair");
      if (!response?.ok) throw new Error(response?.code ?? "Release failed");
    });
  });

  elements["enable-frames"].addEventListener("click", () => {
    const route = reviewedPair;
    return runAction(async () => {
      if (!route) throw new Error("Review and pair the page first");
      // Optional metadata access must be requested directly in this gesture.
      const granted = await chrome.permissions.request({ permissions: ["webNavigation"] });
      if (!granted) throw new Error("Frame metadata permission denied. Top-document reads and actions remain available.");
      const response = await send("enable_child_frames", { route });
      if (!response?.ok) throw new Error(response?.message ?? response?.code ?? "Could not enable child access");
    });
  });

  elements["remove-frame-permission"].addEventListener("click", () => {
    return runAction(async () => {
      if (!(await chrome.permissions.remove({ permissions: ["webNavigation"] }))) {
        throw new Error("Chrome did not remove frame metadata permission");
      }
    });
  });

  chrome.runtime.onMessage.addListener((message, sender) => {
    if (sender.id !== chrome.runtime.id || sender.tab !== undefined ||
        message?.channel !== CHANNEL || message.type !== "popup_state_changed") return false;
    uiRevision += 1;
    changeReason = message.reason;
    pairingCandidateId = null;
    // Invalidate visible consent immediately, even while an action reply is
    // pending. Only a fresh worker read can show enabled controls again.
    elements.connection.textContent = message.reason === "native_disconnected"
      ? "Nova.app unavailable" : "Updating Nova state…";
    elements.pending.hidden = true;
    elements.paired.hidden = true;
    elements.idle.hidden = false;
    elements.idle.textContent = "Updating Nova state…";
    void refresh();
    return false;
  });

  void refresh();
})();
