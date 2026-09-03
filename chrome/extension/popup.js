(() => {
  "use strict";

  const CHANNEL = "nova-extension-v1";
  const elements = Object.fromEntries(
    [
      "connection",
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
      "error",
    ].map((id) => [id, document.getElementById(id)]),
  );
  let expiry = null;
  let countdownTimer = null;
  let pairingCandidateId = null;

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

  async function render() {
    const response = await send("popup_state");
    if (!response?.ok) throw new Error(response?.code ?? "Could not read Nova state");
    const { status, activePage, pairedPage, candidateId } = response;
    elements.connection.textContent = status.connected ? "Nova.app connected" : "Nova.app unavailable";
    elements.pending.hidden = true;
    elements.paired.hidden = true;
    elements.idle.hidden = true;
    pairingCandidateId = null;
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
      elements["page-title"].textContent = activePage?.title || "Unsupported page";
      elements["page-origin"].textContent = activePage ? originOnly(activePage.url) : "Nova cannot access this page";
      pairingCandidateId = typeof candidateId === "string" ? candidateId : null;
      expiry = status.pendingPair.expiresAt;
      tickCountdown();
      countdownTimer = setInterval(tickCountdown, 250);
      return;
    }
    elements.idle.hidden = false;
  }

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

  render().catch((error) => showError(error.message));
})();
