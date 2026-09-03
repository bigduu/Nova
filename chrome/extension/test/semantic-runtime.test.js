import assert from "node:assert/strict";
import test from "node:test";

await import("../lib/semantic-runtime.js");

const semantic = globalThis.NovaSemantic;

class FakeElement {
  constructor(tagName, options = {}) {
    this.tagName = tagName.toUpperCase();
    this.attributes = { ...(options.attributes ?? {}) };
    this.textContent = options.textContent ?? "";
    this.type = options.type ?? this.attributes.type ?? "";
    this.value = options.value ?? "";
    this.disabled = options.disabled ?? false;
    this.checked = options.checked;
    this.multiple = options.multiple ?? false;
    this.isContentEditable = options.isContentEditable ?? false;
    this.isConnected = options.isConnected ?? true;
    this.hidden = options.hidden ?? false;
    this.tabIndex = options.tabIndex ?? -1;
    this.scrollHeight = options.scrollHeight ?? 20;
    this.clientHeight = options.clientHeight ?? 20;
    this.scrollWidth = options.scrollWidth ?? 20;
    this.clientWidth = options.clientWidth ?? 20;
    this.labels = options.labels;
    this.parentElement = options.parentElement ?? null;
    this.sensitiveAncestor = options.sensitiveAncestor ?? false;
    this.hiddenAncestor = options.hiddenAncestor ?? false;
    this.inertAncestor = options.inertAncestor ?? false;
    this.style = {
      display: "block",
      visibility: "visible",
      opacity: "1",
      ...(options.style ?? {}),
    };
    this.rects = options.rects ?? [{}];
    this.rect = options.rect ?? { x: 1.25, y: 2.26, width: 30.04, height: 40.05 };
    this.events = [];
    this.clicks = 0;
    this.scrolls = [];
  }

  getAttribute(name) {
    return Object.hasOwn(this.attributes, name) ? String(this.attributes[name]) : null;
  }

  hasAttribute(name) {
    return Object.hasOwn(this.attributes, name);
  }

  closest(selector) {
    if (selector.includes("data-nova-sensitive") && this.sensitiveAncestor) return this;
    if (selector.includes("[hidden]") && this.hiddenAncestor) return this;
    if (selector.includes("[inert]") && this.inertAncestor) return this;
    return null;
  }

  getClientRects() {
    return this.rects;
  }

  getBoundingClientRect() {
    return this.rect;
  }

  click() {
    this.clicks += 1;
  }

  focus() {
    this.ownerDocument.activeElement = this;
  }

  dispatchEvent(event) {
    this.events.push(event.type);
    return true;
  }

  scrollBy(options) {
    this.scrolls.push(options);
  }
}

class FakeDocument {
  constructor(elements = [], options = {}) {
    this.elements = elements;
    this.title = options.title ?? "Test page";
    this.activeElement = null;
    this.byId = new Map(Object.entries(options.byId ?? {}));
    this.defaultView = {
      Event,
      InputEvent: globalThis.InputEvent ?? Event,
      innerHeight: 800,
      innerWidth: 1200,
      getComputedStyle: (element) => element.style,
    };
    this.documentElement = options.documentElement ?? null;
    this.scrollingElement = options.scrollingElement ?? null;
    for (const element of elements) element.ownerDocument = this;
    if (this.documentElement) this.documentElement.ownerDocument = this;
    if (this.scrollingElement) this.scrollingElement.ownerDocument = this;
  }

  querySelectorAll() {
    return this.elements;
  }

  getElementById(id) {
    return this.byId.get(id) ?? null;
  }
}

function attach(element, document = new FakeDocument([element])) {
  element.ownerDocument = document;
  return element;
}

test("semantic runtime exposes a frozen, bounded API", () => {
  assert.equal(Object.isFrozen(semantic), true);
  assert.equal(semantic.VALID_ROLES.has("button"), true);
  assert.equal(semantic.VALID_ROLES.has("script"), false);
});

test("explicit roles use the first valid token", () => {
  const element = new FakeElement("div", { attributes: { role: "unknown BUTTON link" } });
  assert.equal(semantic.explicitRole(element), "button");
});

test("presentational roles suppress native semantics", () => {
  assert.equal(
    semantic.effectiveRole(new FakeElement("button", { attributes: { role: "presentation" } })),
    null,
  );
});

for (const [tag, options, expected] of [
  ["a", { attributes: { href: "/next" } }, "link"],
  ["button", {}, "button"],
  ["textarea", {}, "textbox"],
  ["select", {}, "combobox"],
  ["select", { multiple: true }, "listbox"],
  ["h3", {}, "heading"],
  ["input", { type: "checkbox" }, "checkbox"],
  ["input", { type: "radio" }, "radio"],
  ["input", { type: "range" }, "slider"],
  ["input", { type: "password" }, null],
]) {
  test(`native role maps ${tag}/${options.type ?? "default"} to ${expected}`, () => {
    assert.equal(semantic.effectiveRole(new FakeElement(tag, options)), expected);
  });
}

test("accessible name precedence is aria-label, labelledby, label, alt, then text", () => {
  const labelled = new FakeElement("span", { textContent: "Account email" });
  const document = new FakeDocument([], { byId: { label: labelled } });
  labelled.ownerDocument = document;
  const element = attach(
    new FakeElement("input", {
      attributes: {
        "aria-label": "Preferred name",
        "aria-labelledby": "label",
        placeholder: "Placeholder",
      },
      labels: [{ textContent: "Associated label" }],
    }),
    document,
  );
  assert.equal(semantic.accessibleName(element), "Preferred name");
  delete element.attributes["aria-label"];
  assert.equal(semantic.accessibleName(element), "Account email");
  delete element.attributes["aria-labelledby"];
  assert.equal(semantic.accessibleName(element), "Associated label");
});

test("accessible names remove control characters, collapse whitespace, and clip", () => {
  const element = new FakeElement("button", {
    attributes: { "aria-label": `  hello\u0000  ${"x".repeat(600)}  ` },
  });
  const name = semantic.accessibleName(element);
  assert.equal(name.includes("\u0000"), false);
  assert.equal(name.length, 512);
  assert.match(name, /^hello x/u);
});

test("ARIA boolean states ignore malformed values", () => {
  const element = new FakeElement("div", {
    attributes: {
      "aria-disabled": "TRUE",
      "aria-expanded": "sometimes",
      "aria-checked": "mixed",
    },
  });
  assert.deepEqual(semantic.ariaStates(element), { disabled: true, checked: "mixed" });
  assert.equal(semantic.validBooleanAria("mixed"), null);
  assert.equal(semantic.validBooleanAria("mixed", true), "mixed");
});

for (const [name, element] of [
  ["password", new FakeElement("input", { type: "password" })],
  ["file", new FakeElement("input", { type: "file" })],
  [
    "credit card autocomplete",
    new FakeElement("input", { attributes: { autocomplete: "section-pay cc-number" } }),
  ],
  ["sensitive ancestor", new FakeElement("textarea", { sensitiveAncestor: true })],
]) {
  test(`sensitive detection blocks ${name}`, () => {
    assert.equal(semantic.isSensitiveElement(element), true);
  });
}

test("ordinary text controls are not considered sensitive", () => {
  assert.equal(
    semantic.isSensitiveElement(
      new FakeElement("input", { type: "email", attributes: { autocomplete: "email" } }),
    ),
    false,
  );
});

for (const [name, element] of [
  ["disconnected", new FakeElement("button", { isConnected: false })],
  ["hidden property", new FakeElement("button", { hidden: true })],
  ["hidden ancestor", new FakeElement("button", { hiddenAncestor: true })],
  ["display none", new FakeElement("button", { style: { display: "none" } })],
  ["transparent", new FakeElement("button", { style: { opacity: "0" } })],
  ["no layout box", new FakeElement("button", { rects: [] })],
]) {
  test(`visibility rejects ${name}`, () => {
    attach(element);
    assert.equal(semantic.isVisible(element), false);
  });
}

test("visibility walks aria-hidden ancestors", () => {
  const parent = new FakeElement("div", { attributes: { "aria-hidden": "true" } });
  const child = attach(new FakeElement("button", { parentElement: parent }));
  assert.equal(semantic.isVisible(child), false);
});

test("capabilities expose semantic actions but not disabled actions", () => {
  const button = new FakeElement("button");
  assert.deepEqual(semantic.capabilities(button, "button"), ["activate", "focus"]);
  button.disabled = true;
  assert.deepEqual(semantic.capabilities(button, "button"), []);
});

test("set_value capability and values are removed for sensitive controls", () => {
  const text = new FakeElement("input", { type: "text", value: "visible" });
  assert.equal(semantic.capabilities(text, "textbox").includes("set_value"), true);
  assert.deepEqual(semantic.safeValue(text, "textbox"), "visible");

  const secret = new FakeElement("input", { type: "text", value: "4111111111111111" });
  secret.attributes.autocomplete = "cc-number";
  assert.equal(semantic.capabilities(secret, "textbox").includes("set_value"), false);
  assert.equal(semantic.safeValue(secret, "textbox"), undefined);
});

test("snapshots include visible semantic nodes and omit sensitive or hidden controls", () => {
  const button = new FakeElement("button", { textContent: "Save" });
  const textbox = new FakeElement("input", { type: "text", value: "hello" });
  textbox.attributes["aria-label"] = "Message";
  const password = new FakeElement("input", { type: "password", value: "secret" });
  const hidden = new FakeElement("button", { textContent: "Invisible", hidden: true });
  const document = new FakeDocument([button, textbox, password, hidden]);

  const snapshot = semantic.createSnapshot(document);
  assert.equal(snapshot.result.coverage, "top_document");
  assert.equal(snapshot.result.truncated, false);
  assert.deepEqual(
    snapshot.result.nodes.map(({ role, name }) => ({ role, name })),
    [
      { role: "button", name: "Save" },
      { role: "textbox", name: "Message" },
    ],
  );
  assert.equal(JSON.stringify(snapshot.result).includes("secret"), false);
  assert.equal(snapshot.handles.size, 2);
});

test("snapshot node limit is bounded and reports truncation", () => {
  const elements = Array.from(
    { length: 4 },
    (_, index) => new FakeElement("button", { textContent: `Button ${index}` }),
  );
  const snapshot = semantic.createSnapshot(new FakeDocument(elements), { maxNodes: 2 });
  assert.equal(snapshot.result.nodes.length, 2);
  assert.equal(snapshot.result.truncated, true);
});

test("scrolling document root gets a semantic handle", () => {
  const scrolling = new FakeElement("html", { scrollHeight: 2000, clientHeight: 800 });
  const snapshot = semantic.createSnapshot(
    new FakeDocument([], { title: "Scrollable", scrollingElement: scrolling }),
  );
  assert.deepEqual(snapshot.result.nodes[0], {
    nodeId: "root",
    role: "document",
    name: "Scrollable",
    actions: ["scroll"],
    states: {},
  });
  assert.equal(snapshot.handles.get("root").element, scrolling);
});

test("bounds are finite, rounded viewport CSS coordinates", () => {
  const button = new FakeElement("button", { textContent: "Round me" });
  const snapshot = semantic.createSnapshot(new FakeDocument([button]));
  assert.deepEqual(snapshot.result.nodes[0].bounds, {
    coordinateSpace: "viewport_css",
    x: 1.3,
    y: 2.3,
    width: 30,
    height: 40.1,
  });
});

test("activate clicks an authorized live semantic node", async () => {
  const element = new FakeElement("button");
  const result = await semantic.performAction(
    { element, actions: ["activate"], sensitive: false },
    "activate",
  );
  assert.deepEqual(result, { activated: true });
  assert.equal(element.clicks, 1);
});

test("focus reports whether the target became active", async () => {
  const element = attach(new FakeElement("input"));
  const result = await semantic.performAction(
    { element, actions: ["focus"], sensitive: false },
    "focus",
  );
  assert.deepEqual(result, { focused: true });
});

test("set_value dispatches input/change but returns only size and hash", async () => {
  const element = attach(new FakeElement("input", { type: "text" }));
  const value = "private draft";
  const result = await semantic.performAction(
    { element, actions: ["set_value"], sensitive: false },
    "set_value",
    { value },
  );
  assert.equal(element.value, value);
  assert.deepEqual(element.events, ["input", "change"]);
  assert.equal(result.valueUtf8Bytes, new TextEncoder().encode(value).byteLength);
  assert.match(result.valueSha256, /^[0-9a-f]{64}$/u);
  assert.equal(JSON.stringify(result).includes(value), false);
});

test("set_value rejects non-strings and oversized values", async () => {
  const element = attach(new FakeElement("input", { type: "text" }));
  const handle = { element, actions: ["set_value"], sensitive: false };
  await assert.rejects(
    semantic.performAction(handle, "set_value", { value: 42 }),
    (error) => error.code === "invalid_value",
  );
  await assert.rejects(
    semantic.performAction(handle, "set_value", { value: "x".repeat(256 * 1024 + 1) }),
    (error) => error.code === "value_too_large",
  );
});

test("scroll validates direction/amount and uses element dimensions", async () => {
  const element = new FakeElement("div", { clientHeight: 600, clientWidth: 1000 });
  const handle = { element, actions: ["scroll"], sensitive: false };
  assert.deepEqual(await semantic.performAction(handle, "scroll", { direction: "down" }), {
    scrolled: true,
    direction: "down",
    amount: "half_page",
  });
  assert.deepEqual(element.scrolls[0], { top: 300, left: 0, behavior: "auto" });
  await assert.rejects(
    semantic.performAction(handle, "scroll", { direction: "diagonal" }),
    (error) => error.code === "invalid_scroll",
  );
});

test("actions reject stale, sensitive, unsupported, and coordinate targets", async () => {
  const stale = new FakeElement("button", { isConnected: false });
  await assert.rejects(
    semantic.performAction({ element: stale, actions: ["activate"] }, "activate"),
    (error) => error.code === "stale_node",
  );

  const sensitive = new FakeElement("input", { type: "password" });
  await assert.rejects(
    semantic.performAction(
      { element: sensitive, actions: ["set_value"], sensitive: true },
      "set_value",
      { value: "secret" },
    ),
    (error) => error.code === "sensitive_control",
  );

  const button = new FakeElement("button");
  await assert.rejects(
    semantic.performAction({ element: button, actions: [] }, "activate"),
    (error) => error.code === "unsupported_action",
  );
  await assert.rejects(
    semantic.performAction(
      { element: button, actions: ["activate"] },
      "activate",
      { x: 10 },
    ),
    (error) => error.code === "coordinate_fallback_forbidden",
  );
});
