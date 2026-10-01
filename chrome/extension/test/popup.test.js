import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const [popupSource, popupMarkup] = await Promise.all([
  readFile(new URL("../popup.js", import.meta.url), "utf8"),
  readFile(new URL("../popup.html", import.meta.url), "utf8"),
]);
const ids = [
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

async function renderPopup(response, permissions = {}) {
  const elements = Object.fromEntries(ids.map((id) => [id, new FakeElement()]));
  const sent = [];
  const chrome = {
    permissions,
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

const reviewedAccess = {
  tabId: 31,
  url: "https://reviewed.example:8443/private?secret=hidden",
  origin: "https://reviewed.example:8443",
  sitePattern: "https://reviewed.example/*",
  status: "needs_tab_access",
  message: "Enable this tab temporarily",
  siteAllowed: false,
};

function consentState(access = reviewedAccess) {
  return { ok: true, status: { connected: true, paired: false, pendingPair: null }, access };
}

test("site request runs directly in the gesture for the already displayed exact host", async () => {
  const requested = [];
  let inGesture = false;
  const { elements, sent } = await renderPopup(consentState(), {
    request(options) {
      assert.equal(inGesture, true, "request cannot wait for an async worker round trip");
      requested.push(structuredClone(options));
      return Promise.resolve(false);
    },
  });
  assert.match(elements["site-scope"].textContent, /https:\/\/reviewed\.example\/\*/u);
  assert.equal(elements["site-scope"].textContent.includes("secret"), false);
  inGesture = true;
  elements["allow-site"].listeners.get("click")();
  inGesture = false;
  assert.deepEqual(requested, [{ origins: ["https://reviewed.example/*"] }]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.match(elements["access-status"].textContent, /permission denied/iu);
  assert.equal(elements["allow-site"].disabled, false);
  assert.equal(sent.length, 1, "denial neither bootstraps nor confirms a pairing");
});

test("tab access passes only the reviewed tab and URL, never pairs implicitly", async () => {
  const { elements, sent } = await renderPopup((message) =>
    message.type === "bootstrap_tab" ? { ok: true } : consentState());
  await elements["use-tab"].listeners.get("click")();
  assert.deepEqual(sent.map((message) => message.type), ["popup_state", "bootstrap_tab", "popup_state"]);
  assert.equal(sent[1].tabId, reviewedAccess.tabId);
  assert.equal(sent[1].url, reviewedAccess.url);
});

test("site revocation removes exactly the displayed host permission", async () => {
  const removed = [];
  const { elements } = await renderPopup(consentState({ ...reviewedAccess, siteAllowed: true }), {
    remove(options) {
      removed.push(structuredClone(options));
      return Promise.resolve(true);
    },
  });
  assert.equal(elements["revoke-site"].hidden, false);
  assert.equal(elements["allow-site"].disabled, true);
  await elements["revoke-site"].listeners.get("click")();
  assert.deepEqual(removed, [{ origins: ["https://reviewed.example/*"] }]);
});

test("unknown or restricted pages disable both permission controls", async () => {
  const { elements } = await renderPopup(consentState({
    status: "unsupported", code: "restricted_page", message: "Chrome does not allow Nova on this page",
  }));
  assert.equal(elements["use-tab"].disabled, true);
  assert.equal(elements["allow-site"].disabled, true);
  assert.equal(elements["revoke-site"].hidden, true);
  assert.match(elements["access-status"].textContent, /does not allow/iu);
});

test("an unenabled HTTP tab is distinct from an unsupported page in the pair review", async () => {
  const { elements } = await renderPopup({
    ...consentState(),
    status: { connected: true, paired: false, pendingPair: { expiresAt: Date.now() + 10_000 } },
    activePage: null, candidateId: null,
  });
  assert.equal(elements["page-title"].textContent, "Enable this tab to review its document");
  assert.equal(elements["page-origin"].textContent, reviewedAccess.origin);
  assert.equal(elements.pair.disabled, true);
});

test("popup displays the Nova icon instead of the temporary letter mark", () => {
  assert.match(
    popupMarkup,
    /<img class="mark" src="icons\/nova-128\.png" width="38" height="38" alt="" \/>/,
  );
  assert.doesNotMatch(popupMarkup, /<span class="mark"[^>]*>\s*N\s*<\/span>/);
});

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
