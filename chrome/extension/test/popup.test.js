import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const popupSource = await readFile(new URL("../popup.js", import.meta.url), "utf8");
const ids = [
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
];

class FakeElement {
  constructor() {
    this.textContent = "";
    this.hidden = false;
    this.disabled = false;
    this.listeners = new Map();
  }

  addEventListener(type, listener) {
    this.listeners.set(type, listener);
  }
}

async function renderPopup(response) {
  const elements = Object.fromEntries(ids.map((id) => [id, new FakeElement()]));
  const sent = [];
  const chrome = {
    runtime: {
      lastError: null,
      sendMessage(message, callback) {
        sent.push(message);
        const value = typeof response === "function" ? response(message) : response;
        callback(structuredClone(value));
      },
    },
  };
  vm.runInNewContext(popupSource, {
    chrome,
    console,
    Date,
    document: { getElementById: (id) => elements[id] },
    Promise,
    setInterval: () => 1,
    clearInterval: () => {},
    URL,
  });
  await new Promise((resolve) => setImmediate(resolve));
  return { elements, sent };
}

test("paired view displays the actual paired origin, never the active tab", async () => {
  const { elements, sent } = await renderPopup({
    ok: true,
    status: { connected: true, paired: true, pendingPair: null },
    activePage: { title: "Current tab", url: "https://current.example/not-paired" },
    pairedPage: { title: "Paired tab", url: "https://paired.example/private/path?q=secret" },
  });
  assert.equal(sent.length, 1);
  assert.equal(sent[0].channel, "nova-extension-v1");
  assert.equal(sent[0].type, "popup_state");
  assert.equal(elements.paired.hidden, false);
  assert.equal(elements["paired-origin"].textContent, "https://paired.example");
  assert.notEqual(elements["paired-origin"].textContent, "https://current.example");
});

test("paired view fails closed when paired metadata is unavailable", async () => {
  const { elements } = await renderPopup({
    ok: true,
    status: { connected: true, paired: true, pendingPair: null },
    activePage: { title: "Unrelated", url: "https://unrelated.example/" },
    pairedPage: null,
  });
  assert.equal(elements["paired-origin"].textContent, "Exact paired document");
  assert.equal(elements["paired-origin"].textContent.includes("unrelated.example"), false);
});

test("pending view uses text content and reduces URLs to origins", async () => {
  const title = '<img src=x onerror="globalThis.compromised=true">';
  const { elements } = await renderPopup({
    ok: true,
    status: {
      connected: true,
      paired: false,
      pendingPair: { expiresAt: Date.now() + 10_000 },
    },
    activePage: {
      title,
      url: "https://user:password@candidate.example/private?token=secret#fragment",
    },
    pairedPage: null,
    candidateId: "pair-candidate-test",
  });
  assert.equal(elements.pending.hidden, false);
  assert.equal(elements["page-title"].textContent, title);
  assert.equal(elements["page-origin"].textContent, "https://candidate.example");
  assert.equal(elements.pair.disabled, false);
});

test("pending view disables pairing for unsupported active pages", async () => {
  const { elements } = await renderPopup({
    ok: true,
    status: {
      connected: false,
      paired: false,
      pendingPair: { expiresAt: Date.now() + 10_000 },
    },
    activePage: null,
    pairedPage: null,
  });
  assert.equal(elements.connection.textContent, "Nova.app unavailable");
  assert.equal(elements["page-title"].textContent, "Unsupported page");
  assert.equal(elements["page-origin"].textContent, "Nova cannot access this page");
  assert.equal(elements.pair.disabled, true);
});

test("pair button confirms only the candidate returned by popup state", async () => {
  const candidateId = "pair-candidate-reviewed-token";
  const { elements, sent } = await renderPopup((message) => {
    if (message.type === "confirm_pair") return { ok: true };
    return {
      ok: true,
      status: {
        connected: true,
        paired: false,
        pendingPair: { expiresAt: Date.now() + 10_000 },
      },
      activePage: { title: "Reviewed", url: "https://reviewed.example/path" },
      pairedPage: null,
      candidateId,
    };
  });

  await elements.pair.listeners.get("click")();
  assert.equal(sent[1].channel, "nova-extension-v1");
  assert.equal(sent[1].type, "confirm_pair");
  assert.equal(sent[1].candidateId, candidateId);
});
