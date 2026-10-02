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
    elements.pair.disabled = !pairingCandidateId || remaining === 0;
    if (remaining === 0) clearInterval(countdownTimer);
  }

  function renderAccess(access) {
    reviewedAccess = access?.sitePattern && Number.isSafeInteger(access.tabId) && typeof access.url === "string"
      ? Object.freeze({ ...access }) : null;
    elements["access-origin"].textContent = access?.origin ?? "No supported web page";
    elements["access-status"].textContent = access?.message ?? "Page access is unavailable. Reopen Nova from a web page.";
    elements["site-scope"].textContent = reviewedAccess
      ? `Site scope: ${access.sitePattern} (all ports, no subdomains). Page access alone does not pair Nova.`
      : "Only HTTP(S) pages are supported. File and incognito access stay off by default.";
    elements["use-tab"].disabled = !reviewedAccess;
    elements["allow-site"].disabled = !reviewedAccess || Boolean(access.siteAllowed);
    elements["revoke-site"].hidden = !reviewedAccess || !access.siteAllowed;
    elements["revoke-site"].disabled = !reviewedAccess || !access.siteAllowed;
  }

  async function render() {
    const response = await send("popup_state");
    if (!response?.ok) throw new Error(response?.code ?? "Could not read Nova state");
    const { status, activePage, pairedPage, candidateId } = response;
    reviewedPair = status.paired && status.route ? Object.freeze({ ...status.route }) : null;
    elements["enable-frames"].disabled = !reviewedPair || Boolean(response.childFrames?.enabled);
    elements["remove-frame-permission"].hidden = !response.childFrames?.permissionGranted;
    elements["remove-frame-permission"].disabled = !response.childFrames?.permissionGranted;
    elements["frame-status"].textContent = response.childFrames?.enabled
      ? "Child reads enabled for this pairing. Only proven visible same-origin documents are included."
      : "Top document only. Child reads are off for this pairing.";
    renderAccess(response.access);
    elements.connection.textContent = status.connected ? "Nova.app connected" : "Nova.app unavailable";
    elements.pending.hidden = true;
    elements.paired.hidden = true;
    elements.idle.hidden = true;
    pairingCandidateId = null;
    elements.pair.disabled = true;
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
  }

  elements["use-tab"].addEventListener("click", async () => {
    const access = reviewedAccess;
    if (!access) return showError("Review a supported web tab first");
    elements["use-tab"].disabled = true;
    try {
      const response = await send("bootstrap_tab", { tabId: access.tabId, url: access.url });
      if (!response?.ok) throw new Error(response?.message ?? response?.code ?? "Could not enable this tab");
      elements.error.hidden = true;
      await render();
    } catch (error) {
      elements["access-status"].textContent = error.message;
      showError(error.message);
      elements["use-tab"].disabled = false;
    }
  });

  elements["allow-site"].addEventListener("click", () => {
    const access = reviewedAccess;
    if (!access) return showError("Review a supported site first");
    elements["allow-site"].disabled = true;
    try {
      // Call directly in the popup gesture, before any asynchronous work.
      const request = chrome.permissions.request({ origins: [access.sitePattern] });
      void request.then(async (granted) => {
        if (!granted) {
          elements["access-status"].textContent = "Site permission denied. Use this tab for temporary access, or try allowing the site again.";
          elements["allow-site"].disabled = false;
          return;
        }
        elements.error.hidden = true;
        await render();
      }).catch((error) => {
        showError(error.message);
        elements["allow-site"].disabled = false;
      });
    } catch (error) {
      showError(error.message);
      elements["allow-site"].disabled = false;
    }
  });

  elements["revoke-site"].addEventListener("click", async () => {
    const access = reviewedAccess;
    if (!access) return showError("Review the allowed site first");
    elements["revoke-site"].disabled = true;
    try {
      const removed = await chrome.permissions.remove({ origins: [access.sitePattern] });
      if (!removed) throw new Error("Chrome did not remove this site's permission");
      elements.error.hidden = true;
      await render();
    } catch (error) {
      showError(error.message);
      elements["revoke-site"].disabled = false;
    }
  });

  elements.pair.addEventListener("click", async () => {
    elements.pair.disabled = true;
    try {
      const candidateId = pairingCandidateId;
      pairingCandidateId = null;
      if (!candidateId) throw new Error("Pairing candidate expired; reopen the popup");
      const response = await send("confirm_pair", { candidateId });
      if (!response?.ok) throw new Error(response?.message ?? response?.code ?? "Pair failed");
      await render();
    } catch (error) {
      showError(error.message);
    }
  });

  elements.deny.addEventListener("click", async () => {
    try {
      const response = await send("deny_pair");
      if (!response?.ok) throw new Error(response?.code ?? "Deny failed");
      await render();
    } catch (error) {
      showError(error.message);
    }
  });

  elements.release.addEventListener("click", async () => {
    try {
      const response = await send("release_pair");
      if (!response?.ok) throw new Error(response?.code ?? "Release failed");
      await render();
    } catch (error) {
      showError(error.message);
    }
  });

  elements["enable-frames"].addEventListener("click", () => {
    const route = reviewedPair;
    if (!route) return showError("Review and pair the page first");
    elements["enable-frames"].disabled = true;
    try {
      // Optional metadata access must be requested directly in this gesture.
      const request = chrome.permissions.request({ permissions: ["webNavigation"] });
      void request.then(async (granted) => {
        if (!granted) {
          elements["frame-status"].textContent = "Frame metadata permission denied. Top-document reads and actions remain available.";
          elements["enable-frames"].disabled = false;
          return;
        }
        const response = await send("enable_child_frames", { route });
        if (!response?.ok) throw new Error(response?.message ?? response?.code ?? "Could not enable child reads");
        elements.error.hidden = true;
        await render();
      }).catch((error) => {
        showError(error.message);
        elements["enable-frames"].disabled = false;
      });
    } catch (error) {
      showError(error.message);
      elements["enable-frames"].disabled = false;
    }
  });

  elements["remove-frame-permission"].addEventListener("click", async () => {
    elements["remove-frame-permission"].disabled = true;
    try {
      if (!(await chrome.permissions.remove({ permissions: ["webNavigation"] }))) {
        throw new Error("Chrome did not remove frame metadata permission");
      }
      elements.error.hidden = true;
      await render();
    } catch (error) {
      showError(error.message);
      elements["remove-frame-permission"].disabled = false;
    }
  });

  render().catch((error) => showError(error.message));
})();
