import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const [popupSource, popupMarkup] = await Promise.all([
  readFile(new URL("../popup.js", import.meta.url), "utf8"),
  readFile(new URL("../popup.html", import.meta.url), "utf8"),
]);
const manifest = JSON.parse(await readFile(new URL("../manifest.json", import.meta.url), "utf8"));
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
  "frame-status",
  "enable-frames",
  "remove-frame-permission",
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
  const listeners = [];
  let countdownTick;
  const chrome = {
    permissions,
    runtime: {
      id: "nova-popup-fixture",
      onMessage: { addListener: (listener) => listeners.push(listener) },
      lastError: null,
      sendMessage(message, callback) {
        sent.push(message);
        const value = typeof response === "function" ? response(message) : response;
        void Promise.resolve(value).then((result) => callback(structuredClone(result)), (error) => {
          chrome.runtime.lastError = error;
          callback();
          chrome.runtime.lastError = null;
        });
      },
    },
  };
  vm.runInNewContext(popupSource, {
    chrome,
    console,
    Date,
    document: { getElementById: (id) => elements[id] },
    Promise,
    setInterval: (callback) => { countdownTick = callback; return 1; },
    clearInterval: () => {},
    URL,
  });
  await new Promise((resolve) => setImmediate(resolve));
  return { elements, sent, tick: () => countdownTick?.(),
    notify(reason, sender = { id: chrome.runtime.id }) {
      for (const listener of listeners) {
        listener({ channel: "nova-extension-v1", type: "popup_state_changed", reason }, sender);
      }
    },
  };
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
  assert.match(elements.error.textContent, /permission denied/iu);
  assert.equal(elements.error.hidden, false);
  assert.equal(elements["allow-site"].disabled, false);
  assert.deepEqual(sent.map((message) => message.type), ["popup_state", "popup_state"],
    "denial refreshes state but neither bootstraps nor confirms a pairing");
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

const pairedRoute = { tabId: 31, documentId: "paired-document", nonce: "paired-nonce", epoch: 4 };
test("frame metadata permission is optional", () => {
  assert.deepEqual(manifest.optional_permissions, ["webNavigation"]);
  assert.equal(manifest.permissions.includes("webNavigation"), false);
});
const frameState = (childFrames = {}) => ({ ...consentState(),
  status: { connected: true, paired: true, route: pairedRoute }, childFrames });

test("optional frame request is a direct gesture and denial leaves top access available", async () => {
  let inGesture = false;
  const requests = [];
  const { elements, sent } = await renderPopup(frameState(), {
    request(options) {
      assert.equal(inGesture, true);
      requests.push(structuredClone(options));
      return Promise.resolve(false);
    },
  });
  assert.equal(elements["enable-frames"].disabled, false);
  inGesture = true;
  elements["enable-frames"].listeners.get("click")();
  inGesture = false;
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(requests, [{ permissions: ["webNavigation"] }]);
  assert.match(elements.error.textContent, /denied.*Top-document reads and actions/);
  assert.equal(elements.error.hidden, false);
  assert.equal(elements["enable-frames"].disabled, false);
  assert.deepEqual(sent.map((message) => message.type), ["popup_state", "popup_state"],
    "denied metadata access refreshes current state without enabling child frames");
  assert.match(popupMarkup, /across your browser/);
  assert.match(popupMarkup, /queries only the paired tab/);
});

test("granted metadata alone stays off until the exact reviewed pairing is enabled", async () => {
  let enabled = false;
  const { elements, sent } = await renderPopup((message) => {
    if (message.type === "enable_child_frames") { enabled = true; return { ok: true }; }
    return frameState({ enabled, permissionGranted: true });
  }, { request: () => Promise.resolve(true) });
  assert.match(elements["frame-status"].textContent, /Top document only/);
  elements["enable-frames"].listeners.get("click")();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(structuredClone(sent[1].route), pairedRoute);
  assert.equal(sent[1].type, "enable_child_frames");
  assert.equal(elements["enable-frames"].disabled, true);
  assert.match(elements["frame-status"].textContent, /Child reads and DOM activation enabled/);
  assert.match(popupMarkup, /Enable child reads and activation/);
  assert.match(popupMarkup, /Child focus, value writes, scrolling and native coordinates remain unavailable/);
});

test("frame permission removal requests only the optional metadata permission", async () => {
  const removed = [];
  const { elements } = await renderPopup(frameState({ enabled: true, permissionGranted: true }), {
    remove(options) { removed.push(structuredClone(options)); return Promise.resolve(true); },
  });
  await elements["remove-frame-permission"].listeners.get("click")();
  assert.deepEqual(removed, [{ permissions: ["webNavigation"] }]);
  const unpaired = await renderPopup(consentState(), { request() { assert.fail("unpaired request"); } });
  assert.equal(unpaired.elements["enable-frames"].disabled, true);
});

test("child metadata permission serializes controls and a late result cannot restore a revoked pairing", async (t) => {
  for (const granted of [true, false]) {
    await t.test(granted ? "late grant" : "late denial", async () => {
      let finishPermission;
      let inGesture = false;
      let revoked = false;
      const fixture = await renderPopup((message) => message.type === "enable_child_frames"
        ? { ok: false, message: "The old pairing changed" }
        : revoked ? consentState() : frameState({ permissionGranted: true }), {
        request(options) {
          assert.equal(inGesture, true);
          assert.deepEqual(structuredClone(options), { permissions: ["webNavigation"] });
          return new Promise((resolve) => { finishPermission = resolve; });
        },
      });
      inGesture = true;
      const action = fixture.elements["enable-frames"].listeners.get("click")();
      inGesture = false;
      for (const id of ["use-tab", "allow-site", "revoke-site", "pair", "deny", "release", "enable-frames", "remove-frame-permission"]) {
        assert.equal(fixture.elements[id].disabled, true, id);
      }
      await fixture.elements.release.listeners.get("click")();
      await fixture.elements["remove-frame-permission"].listeners.get("click")();
      assert.equal(fixture.sent.length, 1, "overlapping actions cannot reach the worker or remove permission");
      revoked = true;
      fixture.notify("navigation");
      finishPermission(granted);
      await action;
      assert.equal(fixture.elements.paired.hidden, true);
      assert.equal(fixture.elements["enable-frames"].disabled, true);
      assert.equal(fixture.elements["remove-frame-permission"].disabled, true);
      assert.equal(fixture.elements.error.hidden, true);
      assert.match(fixture.elements.idle.textContent, /page changed/iu);
    });
  }
});

test("a state-read failure also disables controls from the former child pairing", async () => {
  let fails = false;
  const fixture = await renderPopup(() => fails ? { ok: false, code: "popup_state_failed" }
    : frameState({ enabled: true, permissionGranted: true }));
  assert.equal(fixture.elements["remove-frame-permission"].disabled, false);
  fails = true;
  fixture.notify("native_disconnected");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fixture.elements.paired.hidden, true);
  assert.equal(fixture.elements["enable-frames"].disabled, true);
  assert.equal(fixture.elements["remove-frame-permission"].disabled, true);
  assert.equal(fixture.elements.error.hidden, false);
});

function pendingState() {
  return {
    ...consentState(),
    status: { connected: true, paired: false, pendingPair: { expiresAt: Date.now() + 10_000 } },
    activePage: { title: "Reviewed page", url: reviewedAccess.url },
    candidateId: "pair-candidate-current",
  };
}

test("a notification replaces a delayed state read without restoring its old candidate", async () => {
  let finishOldRead;
  let reads = 0;
  const fixture = await renderPopup(() => {
    if (++reads === 1) return new Promise((resolve) => { finishOldRead = resolve; });
    return consentState();
  });
  assert.equal(fixture.elements["use-tab"].disabled, true, "initial loading blocks actions");
  await fixture.elements["use-tab"].listeners.get("click")();
  assert.equal(fixture.sent.length, 1);
  fixture.notify("navigation");
  finishOldRead(pendingState());
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(reads, 2);
  assert.equal(fixture.elements.pending.hidden, true);
  assert.equal(fixture.elements.pair.disabled, true);
  assert.match(fixture.elements.idle.textContent, /page changed/iu);
});

test("permission actions stay in the gesture and block overlapping actions and countdown enabling", async () => {
  let finishPermission;
  let inGesture = false;
  const fixture = await renderPopup(pendingState(), {
    request() {
      assert.equal(inGesture, true);
      return new Promise((resolve) => { finishPermission = resolve; });
    },
  });
  inGesture = true;
  const action = fixture.elements["allow-site"].listeners.get("click")();
  inGesture = false;
  fixture.tick();
  for (const id of ["use-tab", "allow-site", "pair", "deny", "release"]) {
    assert.equal(fixture.elements[id].disabled, true, id);
  }
  await fixture.elements.pair.listeners.get("click")();
  await fixture.elements["use-tab"].listeners.get("click")();
  assert.equal(fixture.sent.length, 1, "overlapping actions cannot reach the worker");
  finishPermission(false);
  await action;
  assert.equal(fixture.elements.error.hidden, false);
  assert.match(fixture.elements.error.textContent, /permission denied/iu);
  assert.equal(fixture.elements.pair.disabled, false);
  fixture.notify("site_permission_added");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fixture.elements.error.hidden, true, "a subsequent successful state change clears the old failure");
});

test("successful retry clears the old error while a current failure survives its state refresh", async () => {
  let attempts = 0;
  const fixture = await renderPopup((message) => message.type === "bootstrap_tab"
    ? ++attempts === 1 ? { ok: false, message: "Current Chrome access denial" } : { ok: true }
    : consentState());
  await fixture.elements["use-tab"].listeners.get("click")();
  assert.equal(fixture.elements.error.hidden, false);
  assert.match(fixture.elements.error.textContent, /Current Chrome access denial/u);
  await fixture.elements["use-tab"].listeners.get("click")();
  assert.equal(fixture.elements.error.hidden, true);
});

test("late action success or failure cannot restore revoked consent or a stale error", async (t) => {
  for (const lateReply of [{ ok: true }, { ok: false, message: "Old confirmation failure" }]) {
    await t.test(lateReply.ok ? "success" : "failure", async () => {
      let finishConfirmation;
      let revoked = false;
      const fixture = await renderPopup((message) => {
        if (message.type === "confirm_pair") return new Promise((resolve) => { finishConfirmation = resolve; });
        return revoked ? consentState() : pendingState();
      });
      const action = fixture.elements.pair.listeners.get("click")();
      revoked = true;
      fixture.notify("navigation");
      assert.equal(fixture.elements.pair.disabled, true);
      assert.equal(fixture.elements.pending.hidden, true, "revoked consent disappears before a delayed action finishes");
      finishConfirmation(lateReply);
      await action;
      assert.equal(fixture.elements.pair.disabled, true);
      assert.equal(fixture.elements.pending.hidden, true);
      assert.equal(fixture.elements.paired.hidden, true);
      assert.equal(fixture.elements.error.hidden, true);
      assert.match(fixture.elements.idle.textContent, /page changed/iu);
    });
  }
});

test("content-script and foreign notifications cannot trigger popup refreshes", async () => {
  const fixture = await renderPopup(consentState());
  fixture.notify("pair_pending", { id: "foreign-extension" });
  fixture.notify("pair_pending", { id: "nova-popup-fixture", tab: { id: 31 } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fixture.sent.length, 1);
});

test("a current state-read failure removes old controls and remains visible until recovery", async () => {
  let fails = false;
  const fixture = await renderPopup(() => fails ? { ok: false, code: "popup_state_failed" } : pendingState());
  fails = true;
  fixture.notify("navigation");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fixture.elements.pending.hidden, true);
  assert.equal(fixture.elements.pair.disabled, true);
  assert.equal(fixture.elements["allow-site"].disabled, true);
  assert.equal(fixture.elements.error.hidden, false);
  assert.equal(fixture.elements.error.textContent, "popup_state_failed");
  assert.equal(fixture.elements.connection.textContent, "Nova state unavailable");
  fails = false;
  fixture.notify("page_enabled");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fixture.elements.error.hidden, true);
  assert.equal(fixture.elements.pair.disabled, false);
});
