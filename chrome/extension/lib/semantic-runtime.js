(function installNovaSemantic(global) {
  "use strict";

  const MAX_NAME = 512;
  const MAX_VALUE = 1024;
  const MAX_TEXT_SCAN = 4096;
  const MAX_VISITED = 10_000;
  const MAX_DEPTH = 128;
  // protocol.js permits 1 MiB on the wire; leave room for its route envelope.
  const MAX_SNAPSHOT_BYTES = 1024 * 1024 - 4096;
  const NON_TEXT_TAGS = new Set(["head", "script", "style", "template", "noscript", "iframe", "object"]);
  const MAX_SET_VALUE_BYTES = 256 * 1024;
  const VALID_ROLES = new Set([
    "alert",
    "alertdialog",
    "article",
    "banner",
    "button",
    "cell",
    "checkbox",
    "columnheader",
    "combobox",
    "complementary",
    "contentinfo",
    "definition",
    "dialog",
    "directory",
    "document",
    "feed",
    "figure",
    "form",
    "grid",
    "gridcell",
    "group",
    "heading",
    "img",
    "link",
    "list",
    "listbox",
    "listitem",
    "log",
    "main",
    "marquee",
    "math",
    "menu",
    "menubar",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "navigation",
    "none",
    "note",
    "option",
    "presentation",
    "progressbar",
    "radio",
    "radiogroup",
    "region",
    "row",
    "rowgroup",
    "rowheader",
    "scrollbar",
    "search",
    "searchbox",
    "separator",
    "slider",
    "spinbutton",
    "status",
    "switch",
    "tab",
    "table",
    "tablist",
    "tabpanel",
    "term",
    "textbox",
    "timer",
    "toolbar",
    "tooltip",
    "tree",
    "treegrid",
    "treeitem",
  ]);
  const PRESENTATIONAL_ROLES = new Set(["none", "presentation"]);
  const ACTIVATABLE_ROLES = new Set([
    "button",
    "checkbox",
    "link",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "switch",
    "tab",
    "treeitem",
  ]);
  const SETTABLE_ROLES = new Set([
    "combobox",
    "searchbox",
    "slider",
    "spinbutton",
    "textbox",
  ]);
  const SENSITIVE_AUTOCOMPLETE = new Set([
    "cc-csc",
    "cc-exp",
    "cc-exp-month",
    "cc-exp-year",
    "cc-number",
    "current-password",
    "new-password",
    "one-time-code",
  ]);

  function clipped(value, max) {
    if (typeof value !== "string") return "";
    const normalized = value
      .replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/gu, " ")
      .replace(/\s+/gu, " ")
      .trim();
    const result = normalized.slice(0, max);
    // A name boundary must not split a supplementary Unicode character.
    return /[\uD800-\uDBFF]$/u.test(result) ? result.slice(0, -1) : result;
  }

  function attr(element, name) {
    const value = element?.getAttribute?.(name);
    return typeof value === "string" ? value : null;
  }

  function composedParent(node) {
    return node?.assignedSlot || node?.parentElement || node?.parentNode?.host || null;
  }

  function validBooleanAria(value, allowMixed = false) {
    if (typeof value !== "string") return null;
    const normalized = value.trim().toLowerCase();
    if (normalized === "true" || normalized === "false") return normalized;
    if (allowMixed && normalized === "mixed") return normalized;
    return null;
  }

  function explicitRole(element) {
    const role = attr(element, "role");
    if (!role) return null;
    for (const token of role.toLowerCase().trim().split(/\s+/u)) {
      if (VALID_ROLES.has(token)) return token;
    }
    return null;
  }

  function nativeRole(element) {
    const tag = String(element?.tagName ?? "").toLowerCase();
    if (tag === "a" && element.hasAttribute?.("href")) return "link";
    if (tag === "area" && element.hasAttribute?.("href")) return "link";
    if (tag === "button" || tag === "summary") return "button";
    if (tag === "textarea") return "textbox";
    if (tag === "select") return element.multiple ? "listbox" : "combobox";
    if (tag === "option") return "option";
    if (/^h[1-6]$/u.test(tag)) return "heading";
    if (tag === "img") return "img";
    if (tag === "nav") return "navigation";
    if (tag === "main") return "main";
    if (tag === "form") return "form";
    if (tag === "table") return "table";
    if (tag === "ul" || tag === "ol") return "list";
    if (tag === "li") return "listitem";
    if (tag === "input") {
      const type = String(element.type || attr(element, "type") || "text").toLowerCase();
      if (type === "button" || type === "submit" || type === "reset" || type === "image") {
        return "button";
      }
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      if (type === "range") return "slider";
      if (type === "number") return "spinbutton";
      if (type === "search") return "searchbox";
      if (!["hidden", "file", "password"].includes(type)) return "textbox";
    }
    if (element?.isContentEditable) return "textbox";
    return null;
  }

  function effectiveRole(element) {
    const explicit = explicitRole(element);
    if (explicit && !PRESENTATIONAL_ROLES.has(explicit)) return explicit;
    if (explicit && PRESENTATIONAL_ROLES.has(explicit)) return null;
    return nativeRole(element);
  }

  function autocompleteTokens(element) {
    return String(attr(element, "autocomplete") ?? "")
      .toLowerCase()
      .split(/\s+/u)
      .filter(Boolean);
  }

  function isSensitiveElement(element, includeAncestors = true) {
    if (!element || typeof element !== "object") return true;
    const tag = String(element.tagName ?? "").toLowerCase();
    if (tag === "input") {
      const type = String(element.type || attr(element, "type") || "text").toLowerCase();
      if (["password", "file", "hidden"].includes(type)) return true;
    }
    if (autocompleteTokens(element).some((token) => SENSITIVE_AUTOCOMPLETE.has(token))) {
      return true;
    }
    if (["data-nova-sensitive", "data-private", "data-sensitive"].some((name) => element.hasAttribute?.(name))) {
      return true;
    }
    if (includeAncestors && element.closest?.("[data-nova-sensitive], [data-private], [data-sensitive]")) {
      return true;
    }
    if (includeAncestors) {
      let depth = 0;
      for (let parent = composedParent(element); parent; parent = composedParent(parent)) {
        if (++depth > MAX_DEPTH || isSensitiveElement(parent, false)) return true;
      }
    }
    return false;
  }

  function isAriaHidden(element, budget) {
    let current = element;
    let depth = 0;
    while (current?.getAttribute) {
      if (budget && !visit(budget)) return true;
      if (++depth > MAX_DEPTH) {
        if (budget) budget.truncated = true;
        return true;
      }
      const value = validBooleanAria(attr(current, "aria-hidden"));
      if (value === "true") return true;
      current = composedParent(current);
    }
    return false;
  }

  function isVisible(element) {
    if (!element?.isConnected || element.hidden || element.closest?.("[hidden], [inert]")) {
      return false;
    }
    if (isAriaHidden(element)) return false;
    const view = element.ownerDocument?.defaultView;
    if (view?.getComputedStyle) {
      const style = view.getComputedStyle(element);
      if (
        style.display === "none" ||
        style.visibility === "hidden" ||
        style.visibility === "collapse" ||
        Number.parseFloat(style.opacity) === 0
      ) {
        return false;
      }
    }
    if (typeof element.getClientRects === "function" && view) {
      const rects = element.getClientRects();
      if (rects.length === 0 && String(element.tagName ?? "").toLowerCase() !== "area") {
        return false;
      }
    }
    return true;
  }

  function labelledByName(element, read) {
    const ids = read.clip(String(attr(element, "aria-labelledby") ?? ""), MAX_TEXT_SCAN)
      .trim()
      .split(/\s+/u)
      .filter(Boolean)
      .slice(0, 16);
    if (ids.length === 0) return "";
    const treeRoot = element.getRootNode?.() ?? element.ownerDocument;
    return read.clip(
      ids
        .map((id) => treeRoot?.getElementById?.(id))
        .filter((node) => node && !isAriaHidden(node, read.budget))
        .map((node) => read.text(node))
        .join(" "),
      MAX_NAME,
    );
  }

  function associatedLabelName(element, read) {
    if (element.labels && typeof element.labels[Symbol.iterator] === "function") {
      const names = [];
      for (const label of element.labels) {
        if (names.length === 16) {
          read.truncate();
          break;
        }
        names.push(read.text(label));
      }
      return read.clip(names.join(" "), MAX_NAME);
    }
    return "";
  }

  function accessibleName(element, read = {
    clip: clipped,
    text: (node) => node.textContent ?? "",
    truncate() {},
  }) {
    const ariaLabel = read.clip(attr(element, "aria-label") ?? "", MAX_NAME);
    if (ariaLabel) return ariaLabel;
    const labelledBy = labelledByName(element, read);
    if (labelledBy) return labelledBy;
    const label = associatedLabelName(element, read);
    if (label) return label;
    const alt = read.clip(attr(element, "alt") ?? "", MAX_NAME);
    if (alt) return alt;
    const tag = String(element.tagName ?? "").toLowerCase();
    if (tag === "input") {
      const type = String(element.type || "text").toLowerCase();
      if (["button", "submit", "reset"].includes(type)) {
        const value = read.clip(String(element.value ?? ""), MAX_NAME);
        if (value) return value;
      }
      const placeholder = read.clip(attr(element, "placeholder") ?? "", MAX_NAME);
      if (placeholder) return placeholder;
    }
    const title = read.clip(attr(element, "title") ?? "", MAX_NAME);
    if (title) return title;
    return read.clip(read.text(element), MAX_NAME);
  }

  function ariaStates(element) {
    const states = {};
    const booleanAttrs = ["disabled", "expanded", "selected", "pressed"];
    for (const name of booleanAttrs) {
      const value = validBooleanAria(attr(element, `aria-${name}`));
      if (value !== null) states[name] = value === "true";
    }
    const checked = validBooleanAria(attr(element, "aria-checked"), true);
    if (checked !== null) states.checked = checked === "mixed" ? "mixed" : checked === "true";
    if (element.disabled === true) states.disabled = true;
    if (typeof element.checked === "boolean" && ["checkbox", "radio"].includes(nativeRole(element))) {
      states.checked = element.checked;
    }
    return states;
  }

  function isFocusable(element, role) {
    if (element.disabled === true) return false;
    if (Number.isInteger(element.tabIndex) && element.tabIndex >= 0) return true;
    return Boolean(
      role &&
        (ACTIVATABLE_ROLES.has(role) || SETTABLE_ROLES.has(role) || role === "option"),
    );
  }

  function isScrollable(element) {
    if (!element) return false;
    if (element.scrollHeight > element.clientHeight + 1) return true;
    if (element.scrollWidth > element.clientWidth + 1) return true;
    return false;
  }

  function capabilities(element, role) {
    const actions = [];
    const disabled = element.disabled === true || validBooleanAria(attr(element, "aria-disabled")) === "true";
    if (!disabled && ACTIVATABLE_ROLES.has(role)) actions.push("activate");
    if (!disabled && isFocusable(element, role)) actions.push("focus");
    if (!disabled && SETTABLE_ROLES.has(role) && !isSensitiveElement(element)) actions.push("set_value");
    if (isScrollable(element)) actions.push("scroll");
    return actions;
  }

  function safeValue(element, role, clip = clipped) {
    if (isSensitiveElement(element) || !SETTABLE_ROLES.has(role)) return undefined;
    if (typeof element.value !== "string") return undefined;
    return clip(element.value, MAX_VALUE);
  }

  function bounds(element) {
    if (typeof element.getBoundingClientRect !== "function") return undefined;
    const rect = element.getBoundingClientRect();
    if (![rect.x, rect.y, rect.width, rect.height].every(Number.isFinite)) return undefined;
    return {
      coordinateSpace: "viewport_css",
      x: Math.round(rect.x * 10) / 10,
      y: Math.round(rect.y * 10) / 10,
      width: Math.round(rect.width * 10) / 10,
      height: Math.round(rect.height * 10) / 10,
    };
  }

  function randomToken(prefix) {
    const bytes = new Uint8Array(16);
    global.crypto.getRandomValues(bytes);
    return `${prefix}-${Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
  }

  function excludedTextTree(element) {
    if (
      !element.isConnected || element.hidden || element.hasAttribute?.("inert") ||
      validBooleanAria(attr(element, "aria-hidden")) === "true" ||
      NON_TEXT_TAGS.has(String(element.tagName ?? "").toLowerCase()) ||
      isSensitiveElement(element, false)
    ) return true;
    const style = element.ownerDocument?.defaultView?.getComputedStyle?.(element);
    return style?.display === "none" || Number.parseFloat(style?.opacity) === 0;
  }

  function visit(budget) {
    if (budget.remaining === 0) {
      budget.truncated = true;
      return false;
    }
    budget.remaining -= 1;
    return true;
  }

  function childCursor(node) {
    if (node.nodeType === 1) {
      const shadow = node.shadowRoot;
      if (shadow) return { next: shadow.firstChild };
      if (String(node.tagName).toLowerCase() === "slot" && typeof node.assignedNodes === "function") {
        // The browser materializes this result. Consume it by index without
        // flattening recursively or copying the assignments into our stack.
        const assigned = node.assignedNodes();
        if (assigned.length) return { assigned, index: 0 };
      }
    }
    return { next: node.firstChild };
  }

  function* textTree(root, budget, suppressNamedText = false) {
    // Label references can start inside a hidden/sensitive ancestor. Check
    // their context, with the same budget as the main snapshot traversal.
    let depth = 0;
    for (let child = root, parent = composedParent(root); parent; child = parent, parent = composedParent(parent)) {
      if (!visit(budget)) return;
      if (++depth > MAX_DEPTH) {
        budget.truncated = true;
        return;
      }
      if (excludedTextTree(parent)) return;
      // Referenced names must participate in the same composed tree as the
      // main walk, even when their own computed visibility says "visible".
      if (child.parentNode === parent) {
        if (parent.shadowRoot && !child.assignedSlot) return;
        if (String(parent.tagName).toLowerCase() === "slot" && parent.assignedNodes?.().length) return;
      }
    }
    const stack = root ? [{ next: root, single: true, depth: 0, suppressed: false }] : [];
    const seen = new Set();
    while (stack.length) {
      const current = stack.at(-1);
      const node = current.assigned ? current.assigned[current.index++] : current.next;
      if (!node) {
        stack.pop();
        continue;
      }
      if (!current.assigned) current.next = current.single ? null : node.nextSibling;
      if (!visit(budget)) return;
      if (seen.has(node)) continue;
      seen.add(node);
      if (node.nodeType === 1 && excludedTextTree(node)) continue;
      yield { node, suppressed: current.suppressed };
      if (budget.remaining === 0) {
        budget.truncated = true;
        return;
      }
      const children = childCursor(node);
      if (!children.next && !children.assigned) continue;
      if (current.depth >= MAX_DEPTH) {
        budget.truncated = true;
        continue;
      }
      const role = node.nodeType === 1 ? effectiveRole(node) : null;
      stack.push({
        ...children,
        depth: current.depth + 1,
        suppressed: current.suppressed || Boolean(suppressNamedText &&
          (role === "heading" || ACTIVATABLE_ROLES.has(role) || SETTABLE_ROLES.has(role)) && isVisible(node)),
      });
    }
  }

  function textVisible(node) {
    const parent = composedParent(node);
    if (!node.isConnected || !parent) return false;
    const style = node.ownerDocument?.defaultView?.getComputedStyle?.(parent);
    return style?.visibility !== "hidden" && style?.visibility !== "collapse";
  }

  function textBounds(node) {
    if (!textVisible(node)) return null;
    const range = node.ownerDocument?.createRange?.();
    if (!range) return null;
    // Limit the range too: a very long text node can have many line boxes.
    range.setStart(node, 0);
    range.setEnd(node, Math.min(node.length, MAX_TEXT_SCAN));
    const rects = range.getClientRects();
    let rendered = false;
    for (let index = 0; index < rects.length; index += 1) {
      if (rects[index].width > 0 && rects[index].height > 0) {
        rendered = true;
        break;
      }
    }
    return rendered ? bounds(range) : null;
  }

  function createSnapshot(document, { maxNodes = 500, maxChars = 100_000 } = {}) {
    const nodeLimit = Math.floor(Math.max(1, Math.min(Number(maxNodes) || 500, 1000)));
    const charLimit = Math.floor(Math.max(1024, Math.min(Number(maxChars) || 100_000, 500_000)));
    const snapshotId = randomToken("snapshot");
    const nodes = [];
    const handles = new Map();
    const result = { snapshotId, nodes, truncated: false, coverage: "top_document" };
    let characters = JSON.stringify(result).length;
    let bytes = new TextEncoder().encode(JSON.stringify(result)).byteLength;
    const budget = { remaining: MAX_VISITED, truncated: false };
    const read = {
      budget,
      clip(value, max) {
        if (typeof value !== "string") return "";
        const normalized = clipped(value.slice(0, MAX_TEXT_SCAN), Infinity)
          .replace(/[\uD800-\uDFFF]/gu, "\uFFFD");
        if (value.length > MAX_TEXT_SCAN || normalized.length > max) budget.truncated = true;
        return clipped(normalized, max);
      },
      text(element) {
        let value = "";
        for (const { node } of textTree(element, budget)) {
          if (node.nodeType !== 3 || !textVisible(node)) continue;
          const remaining = MAX_TEXT_SCAN - value.length;
          value += node.data.slice(0, remaining);
          if (node.length > remaining || value.length === MAX_TEXT_SCAN) {
            budget.truncated = true;
            break;
          }
        }
        return value;
      },
      truncate() { budget.truncated = true; },
    };
    function append(node, element) {
      const serialized = JSON.stringify(node);
      const comma = nodes.length ? 1 : 0;
      const length = serialized.length + comma;
      const byteLength = new TextEncoder().encode(serialized).byteLength + comma;
      if (nodes.length >= nodeLimit || characters + length > charLimit || bytes + byteLength > MAX_SNAPSHOT_BYTES) {
        budget.truncated = true;
        return false;
      }
      nodes.push(node);
      handles.set(node.nodeId, { element, actions: node.actions, sensitive: false });
      characters += length;
      bytes += byteLength;
      return true;
    }

    const scrollingElement = document.scrollingElement || document.documentElement;
    if (scrollingElement && !isSensitiveElement(scrollingElement) && isScrollable(scrollingElement)) {
      const node = {
        nodeId: "root",
        role: "document",
        name: read.clip(document.title || "Page", MAX_NAME),
        actions: ["scroll"],
        states: {},
      };
      append(node, scrollingElement);
    }

    const root = document.body || document.documentElement || document;
    for (const { node: element, suppressed } of textTree(root, budget, true)) {
      if (nodes.length >= nodeLimit || characters >= charLimit) {
        budget.truncated = true;
        break;
      }
      if (element.nodeType === 3) {
        if (suppressed) continue;
        const rect = textBounds(element);
        if (!rect) continue;
        const name = read.clip(element.data, MAX_NAME);
        if (!name) continue;
        if (!append({
          nodeId: `n${nodes.length + 1}`, role: "text", name,
          actions: [], states: {}, bounds: rect,
        }, element)) break;
        continue;
      }
      if (element.nodeType !== 1) continue;
      if (!isVisible(element) || isSensitiveElement(element)) continue;
      const role = effectiveRole(element);
      if (!role) continue;
      const actions = capabilities(element, role);
      const name = accessibleName(element, read);
      if (!name && actions.length === 0 && !["main", "navigation", "form", "heading"].includes(role)) {
        continue;
      }
      const nodeId = `n${nodes.length + 1}`;
      const node = { nodeId, role, name, actions, states: ariaStates(element) };
      const value = safeValue(element, role, read.clip);
      if (value !== undefined) node.value = { kind: "text", text: value };
      const description = read.clip(attr(element, "aria-description") ?? "", MAX_NAME);
      if (description) node.description = description;
      const rect = bounds(element);
      if (rect) node.bounds = rect;
      if (!append(node, element)) break;
    }

    result.truncated = budget.truncated;
    return { result, handles };
  }

  async function sha256Bytes(bytes) {
    try {
      if (typeof global.crypto?.subtle?.digest !== "function") throw new Error();
      const digest = new Uint8Array(await global.crypto.subtle.digest("SHA-256", bytes));
      if (digest.byteLength !== 32) throw new Error();
      return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
    } catch {
      throw Object.assign(new Error(
        "SHA256 value receipt unavailable; no value was written. Use HTTPS or a trusted loopback origin and read again before retrying",
      ), { code: "value_receipt_unavailable" });
    }
  }

  function setNativeValue(element, value) {
    let prototype = Object.getPrototypeOf(element);
    let setter;
    while (prototype && !setter) {
      setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set;
      prototype = Object.getPrototypeOf(prototype);
    }
    if (setter) setter.call(element, value);
    else element.value = value;
  }

  function validateNodeTarget(handle, action) {
    if (!handle?.element?.isConnected) throw Object.assign(new Error("semantic node is stale"), { code: "stale_node" });
    if (handle.sensitive || isSensitiveElement(handle.element)) {
      throw Object.assign(new Error("sensitive controls cannot be targeted"), { code: "sensitive_control" });
    }
    if (!handle.actions.includes(action)) {
      throw Object.assign(new Error(`node does not support ${action}`), { code: "unsupported_action" });
    }
  }

  async function performAction(handle, action, args = {}, assertCurrent = () => {}) {
    validateNodeTarget(handle, action);
    const element = handle.element;
    if (Object.hasOwn(args, "x") || Object.hasOwn(args, "y") || Object.hasOwn(args, "coordinates")) {
      throw Object.assign(new Error("coordinate actions are not supported"), { code: "coordinate_fallback_forbidden" });
    }
    if (action === "activate") {
      if (typeof element.click !== "function") throw Object.assign(new Error("node cannot be activated"), { code: "unsupported_action" });
      element.click();
      return { activated: true };
    }
    if (action === "focus") {
      if (typeof element.focus !== "function") throw Object.assign(new Error("node cannot be focused"), { code: "unsupported_action" });
      element.focus({ preventScroll: true });
      const treeRoot = element.getRootNode?.() ?? element.ownerDocument;
      return { focused: treeRoot?.activeElement === element };
    }
    if (action === "set_value") {
      if (typeof args.value !== "string") throw Object.assign(new Error("value must be a string"), { code: "invalid_value" });
      const value = args.value;
      const encoded = new TextEncoder().encode(value);
      if (encoded.byteLength > MAX_SET_VALUE_BYTES) {
        throw Object.assign(new Error("value is too large"), { code: "value_too_large" });
      }
      // Prepare the existing receipt before any value/event side effect. The
      // await must not let a revoked snapshot or newly sensitive node write.
      const valueSha256 = await sha256Bytes(encoded);
      assertCurrent();
      validateNodeTarget(handle, action);
      setNativeValue(element, value);
      const view = element.ownerDocument?.defaultView ?? global;
      const InputEventCtor = view.InputEvent ?? view.Event;
      element.dispatchEvent(new InputEventCtor("input", { bubbles: true, inputType: "insertText" }));
      element.dispatchEvent(new view.Event("change", { bubbles: true }));
      return {
        valueUtf8Bytes: encoded.byteLength,
        valueSha256,
      };
    }
    if (action === "scroll") {
      const direction = args.direction;
      const amount = args.amount ?? "half_page";
      if (!["up", "down", "left", "right"].includes(direction)) {
        throw Object.assign(new Error("invalid scroll direction"), { code: "invalid_scroll" });
      }
      if (!["line", "half_page", "page"].includes(amount)) {
        throw Object.assign(new Error("invalid scroll amount"), { code: "invalid_scroll" });
      }
      const verticalBase = Math.max(1, element.clientHeight || element.ownerDocument?.defaultView?.innerHeight || 800);
      const horizontalBase = Math.max(1, element.clientWidth || element.ownerDocument?.defaultView?.innerWidth || 1200);
      const multiplier = amount === "line" ? 0.1 : amount === "page" ? 0.9 : 0.5;
      const distance = (direction === "left" || direction === "right" ? horizontalBase : verticalBase) * multiplier;
      const top = direction === "up" ? -distance : direction === "down" ? distance : 0;
      const left = direction === "left" ? -distance : direction === "right" ? distance : 0;
      element.scrollBy({ top, left, behavior: "auto" });
      return { scrolled: true, direction, amount };
    }
    throw Object.assign(new Error("unsupported semantic action"), { code: "unsupported_action" });
  }

  global.NovaSemantic = Object.freeze({
    VALID_ROLES,
    SENSITIVE_AUTOCOMPLETE,
    accessibleName,
    ariaStates,
    capabilities,
    createSnapshot,
    effectiveRole,
    explicitRole,
    isSensitiveElement,
    isVisible,
    performAction,
    safeValue,
    validBooleanAria,
  });
})(globalThis);
