import assert from "node:assert/strict";
import test from "node:test";

await import("../lib/semantic-runtime.js");

const semantic = globalThis.NovaSemantic;

class FakeElement {
  constructor(tagName, options = {}) {
    this.nodeType = 1;
    this.childNodes = [];
    this.nextSibling = null;
    this.parentNode = null;
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

  get firstChild() { return this.childNodes[0] ?? null; }

  get textContent() {
    return this.childNodes.map((child) => child.textContent).join("");
  }

  set textContent(value) {
    this.childNodes = [];
    if (value) this.appendChild(new FakeText(value));
  }

  appendChild(child) {
    const previous = this.childNodes.at(-1);
    if (previous) previous.nextSibling = child;
    child.parentNode = this;
    child.parentElement = this;
    child.ownerDocument = this.ownerDocument;
    this.childNodes.push(child);
    return child;
  }

  getAttribute(name) {
    return Object.hasOwn(this.attributes, name) ? String(this.attributes[name]) : null;
  }

  hasAttribute(name) {
    return Object.hasOwn(this.attributes, name);
  }

  closest(selector) {
    for (let current = this; current; current = current.parentElement) {
      if (selector.includes("data-nova-sensitive") && current.sensitiveAncestor) return current;
      if (selector.includes("[hidden]") && (current.hiddenAncestor || current.hidden)) return current;
      if (selector.includes("[inert]") && current.inertAncestor) return current;
      for (const match of selector.matchAll(/\[([\w-]+)\]/gu)) {
        if (current.hasAttribute(match[1])) return current;
      }
    }
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

class FakeText {
  constructor(data, options = {}) {
    this.nodeType = 3;
    this._data = data;
    this.isConnected = true;
    this.parentElement = null;
    this.parentNode = null;
    this.nextSibling = null;
    this.firstChild = null;
    this.rects = options.rects ?? [{ width: 20, height: 15 }];
    this.rect = { x: 1, y: 2, width: 20, height: 15 };
    this.reads = 0;
  }
  get data() { this.reads += 1; return this._data; }
  get length() { return this._data.length; }
  get textContent() { return this.data; }
}

class FakeDocument {
  constructor(elements = [], options = {}) {
    this.nodeType = 9;
    this.parentElement = null;
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
    this.body = options.body ?? null;
    this.firstChild = this.body ?? this.documentElement ?? elements[0] ?? null;
    if (!this.body && !this.documentElement) {
      for (let index = 0; index < elements.length; index += 1) {
        elements[index].nextSibling = elements[index + 1] ?? null;
        elements[index].parentNode = this;
      }
    }
    const pending = [...elements, this.body, this.documentElement, this.scrollingElement];
    while (pending.length) {
      const node = pending.pop();
      if (!node) continue;
      node.ownerDocument = this;
      if (node.nodeType === 1 && node.hasAttribute("id")) this.byId.set(node.getAttribute("id"), node);
      for (const child of node.childNodes ?? []) pending.push(child);
    }
  }

  querySelectorAll() {
    return this.elements;
  }

  getElementById(id) {
    return this.byId.get(id) ?? null;
  }

  createRange() {
    let text;
    return {
      setStart(node) { text = node; },
      setEnd(node, end) { assert.ok(end <= Math.min(node.length, 4096)); },
      getClientRects() { return text.rects; },
      getBoundingClientRect() { return text.rect; },
    };
  }
}

function attach(element, document = new FakeDocument([element])) {
  element.ownerDocument = document;
  return element;
}

function branch(tag, children, options = {}) {
  const element = new FakeElement(tag, options);
  for (const child of children) element.appendChild(typeof child === "string" ? new FakeText(child) : child);
  return element;
}

function page(...children) {
  return new FakeDocument([], { body: branch("body", children) });
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

test("static paragraphs, labels and mixed Unicode text follow document order without control descendants", () => {
  const label = branch("label", ["消息字段标签"]);
  const field = new FakeElement("input", { value: "可见字段 Unicode ✓", labels: [label] });
  const increment = branch("button", [branch("span", ["增加计数"])]);
  const document = page(
    branch("h1", ["Nova ", branch("em", ["静态文本 🪷"])]),
    branch("p", ["段落开头 ", branch("em", ["强调文本"]), " 段落结尾 ✓"]),
    branch("div", ["混合开头 ", branch("a", ["可见链接"], { attributes: { href: "#end" } }), " 混合结尾"]),
    label, field, increment, branch("output", ["计数 0"]), branch("p", ["结束文本"]),
  );
  const { result, handles } = semantic.createSnapshot(document);
  assert.deepEqual(result.nodes.map(({ role, name }) => [role, name]), [
    ["heading", "Nova 静态文本 🪷"],
    ["text", "段落开头"], ["text", "强调文本"], ["text", "段落结尾 ✓"],
    ["text", "混合开头"], ["link", "可见链接"], ["text", "混合结尾"],
    ["text", "消息字段标签"], ["textbox", "消息字段标签"], ["button", "增加计数"],
    ["text", "计数 0"], ["text", "结束文本"],
  ]);
  assert.equal(result.truncated, false);
  assert.equal(result.coverage, "top_document");
  assert.equal(result.nodes.filter((node) => node.name === "增加计数").length, 1);
  assert.equal(result.nodes.filter((node) => node.name === "可见链接").length, 1);
  const control = result.nodes.find((node) => node.role === "button");
  assert.ok(control.actions.includes("activate"));
  assert.equal(handles.get(control.nodeId).element, increment);
  assert.equal(result.nodes.find((node) => node.role === "textbox").value.text, "可见字段 Unicode ✓");
});

test("text nodes expose no actions and reject every mutation", async () => {
  const { result, handles } = semantic.createSnapshot(page(branch("p", ["Read only 🪷"])));
  const node = result.nodes[0];
  assert.equal(node.role, "text");
  assert.deepEqual(node.actions, []);
  const handle = handles.get(node.nodeId);
  assert.equal(handle.element.nodeType, 3);
  for (const action of ["activate", "focus", "set_value", "scroll"]) {
    await assert.rejects(semantic.performAction(handle, action, { value: "must not be set", direction: "down" }),
      (error) => error.code === "unsupported_action");
  }
  assert.equal(node.name, "Read only 🪷");
  assert.equal(handle.element.data, "Read only 🪷");
});

test("hidden, inert, sensitive and non-page descendants are never read, output or cached", () => {
  const markers = [];
  const cases = [
    ["div", { hidden: true }],
    ["div", { attributes: { inert: "" } }],
    ["div", { attributes: { "aria-hidden": "true" } }],
    ["div", { attributes: { "data-private": "" } }],
    ["div", { attributes: { "data-sensitive": "" } }],
    ["div", { attributes: { "data-nova-sensitive": "" } }],
    ["div", { style: { display: "none" } }],
    ["div", { style: { opacity: "0" } }],
    ["span", { style: { visibility: "hidden" } }],
    ["script", {}], ["style", {}], ["template", {}],
  ];
  const excluded = cases.map(([tag, options], index) => {
    const text = new FakeText(`EXCLUDE_TEXT_${index}`);
    markers.push(text);
    return branch(tag, [text], options);
  });
  const group = branch("div", ["Safe beginning ", ...excluded, " safe ending"], { attributes: { role: "group" } });
  const { result, handles } = semantic.createSnapshot(page(group));
  assert.equal(result.nodes.find((node) => node.role === "group").name, "Safe beginning safe ending");
  assert.equal(JSON.stringify(result).includes("EXCLUDE_TEXT_"), false);
  for (const text of markers) {
    assert.equal(text.reads, 0, "exclusion must happen before reading text data");
    assert.equal([...handles.values()].some((handle) => handle.element === text), false);
  }
});

test("snapshot name priority is preserved and sensitive label references cannot leak text", () => {
  const sensitiveText = new FakeText("EXCLUDE_LABEL_SECRET");
  const sensitive = branch("span", [sensitiveText], { attributes: { id: "private-name", "data-private": "" } });
  const fallback = branch("span", ["Safe associated label"]);
  const aria = branch("button", ["Descendant name"], { attributes: { "aria-label": "Explicit name 🪷" } });
  const labelled = new FakeElement("input", {
    attributes: { "aria-labelledby": "private-name", placeholder: "Placeholder" }, labels: [fallback],
  });
  const snapshot = semantic.createSnapshot(page(sensitive, fallback, aria, labelled));
  assert.equal(snapshot.result.nodes.find((node) => node.role === "button").name, "Explicit name 🪷");
  assert.equal(snapshot.result.nodes.find((node) => node.role === "textbox").name, "Safe associated label");
  assert.equal(sensitiveText.reads, 0);
  assert.equal(JSON.stringify(snapshot.result).includes("EXCLUDE_LABEL_SECRET"), false);
});

test("rendered display-contents text is included, but a text range without a box is not", () => {
  const invisible = new FakeText("EXCLUDE_NO_RANGE", { rects: [] });
  const document = page(
    branch("div", ["Contents 🪷"], { style: { display: "contents" }, rects: [] }),
    branch("p", [invisible]),
  );
  const { result, handles } = semantic.createSnapshot(document);
  assert.deepEqual(result.nodes.map((node) => node.name), ["Contents 🪷"]);
  assert.equal(invisible.reads, 0);
  assert.equal([...handles.values()].some((handle) => handle.element === invisible), false);
});

test("visible descendants of a non-rendered named parent are not suppressed", () => {
  const document = page(
    branch("button", [branch("span", ["Visible caption"], { style: { visibility: "visible" } })],
      { style: { visibility: "hidden" } }),
    branch("h2", ["Contents heading text"], { style: { display: "contents" }, rects: [] }),
  );
  const { result } = semantic.createSnapshot(document);
  assert.deepEqual(result.nodes.map(({ role, name, actions }) => [role, name, actions]), [
    ["text", "Visible caption", []], ["text", "Contents heading text", []],
  ]);
  assert.equal(result.truncated, false);
});

test("static text clips on Unicode boundaries and reports lost text", () => {
  const document = page(branch("p", ["a".repeat(511) + "🪷 end"]), branch("p", ["bad\uD800middle\uDC00end"]));
  const { result } = semantic.createSnapshot(document);
  assert.equal(result.nodes[0].name, "a".repeat(511));
  assert.equal(result.nodes[1].name, "bad�middle�end");
  assert.ok(result.nodes.every((node) => node.name.isWellFormed()));
  assert.equal(result.truncated, true);
});

test("static text respects exact node, complete JSON character and UTF-8 wire bounds", () => {
  const document = page(...Array.from({ length: 1200 }, (_, index) => branch("p", [`${index}: ${"界".repeat(490)}`])));
  const small = semantic.createSnapshot(document, { maxNodes: 3 });
  assert.equal(small.result.nodes.length, 3);
  assert.equal(small.result.truncated, true);
  const chars = semantic.createSnapshot(document, { maxNodes: 1000, maxChars: 1024 });
  assert.ok(JSON.stringify(chars.result).length <= 1024);
  assert.ok(chars.result.nodes.length < 3);
  assert.equal(chars.result.truncated, true);
  const wire = semantic.createSnapshot(document, { maxNodes: 1000, maxChars: 500_000 });
  assert.ok(new TextEncoder().encode(JSON.stringify(wire.result)).byteLength <= 1024 * 1024 - 4096);
  assert.ok(wire.result.nodes.length < 1000);
  assert.equal(wire.result.truncated, true);
  assert.equal(wire.handles.size, wire.result.nodes.length);
});

test("large empty and deeply nested trees stop boundedly without aggregating textContent", () => {
  const empty = page(...Array.from({ length: 12_000 }, () => new FakeElement("div")));
  let stylesRead = 0;
  empty.defaultView.getComputedStyle = (element) => { stylesRead += 1; return element.style; };
  empty.querySelectorAll = () => assert.fail("must not materialize the full document");
  assert.equal(semantic.createSnapshot(empty).result.truncated, true);
  assert.ok(stylesRead <= 20_000, `unbounded traversal: ${stylesRead}`);

  const container = branch("div", [], { attributes: { role: "group" } });
  let current = container;
  for (let index = 0; index < 2000; index += 1) current = current.appendChild(new FakeElement("div"));
  const terminal = current.appendChild(new FakeText("EXCLUDE_TOO_DEEP"));
  Object.defineProperty(container, "textContent", { get() { assert.fail("must not aggregate the full subtree"); } });
  const deep = semantic.createSnapshot(page(container));
  assert.equal(deep.result.truncated, true);
  assert.equal(JSON.stringify(deep.result).includes("EXCLUDE_TOO_DEEP"), false);
  assert.equal(terminal.reads, 0);
});

test("deep external label references use the snapshot budget without reading terminal text", () => {
  const container = new FakeElement("div");
  let current = container;
  for (let index = 0; index < 2000; index += 1) current = current.appendChild(new FakeElement("div"));
  const terminal = new FakeText("EXCLUDE_DEEP_LABEL");
  current.appendChild(branch("span", [terminal], { attributes: { id: "deep-label" } }));
  const field = new FakeElement("input", { attributes: { "aria-labelledby": "deep-label", placeholder: "Safe fallback" } });
  const snapshot = semantic.createSnapshot(page(field, container));
  assert.equal(snapshot.result.nodes.find((node) => node.role === "textbox").name, "Safe fallback");
  assert.equal(snapshot.result.truncated, true);
  assert.equal(JSON.stringify(snapshot.result).includes("EXCLUDE_DEEP_LABEL"), false);
  assert.equal(terminal.reads, 0);
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
