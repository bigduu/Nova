(function installNovaSemantic(global) {
  "use strict";

  const MAX_NAME = 512;
  const MAX_VALUE = 1024;
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
    return value
      .replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/gu, " ")
      .replace(/\s+/gu, " ")
      .trim()
      .slice(0, max);
  }

  function attr(element, name) {
    const value = element?.getAttribute?.(name);
    return typeof value === "string" ? value : null;
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

  function isSensitiveElement(element) {
    if (!element || typeof element !== "object") return true;
    const tag = String(element.tagName ?? "").toLowerCase();
    if (tag === "input") {
      const type = String(element.type || attr(element, "type") || "text").toLowerCase();
      if (["password", "file", "hidden"].includes(type)) return true;
    }
    if (autocompleteTokens(element).some((token) => SENSITIVE_AUTOCOMPLETE.has(token))) {
      return true;
    }
    if (element.closest?.("[data-nova-sensitive], [data-private], [data-sensitive]")) {
      return true;
    }
    return false;
  }

  function isAriaHidden(element) {
    let current = element;
    while (current?.getAttribute) {
      const value = validBooleanAria(attr(current, "aria-hidden"));
      if (value === "true") return true;
      current = current.parentElement;
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

  function labelledByName(element) {
    const ids = String(attr(element, "aria-labelledby") ?? "")
      .trim()
      .split(/\s+/u)
      .filter(Boolean)
      .slice(0, 16);
    if (ids.length === 0) return "";
    const document = element.ownerDocument;
    return clipped(
      ids
        .map((id) => document?.getElementById?.(id))
        .filter((node) => node && !isAriaHidden(node))
        .map((node) => node.textContent ?? "")
        .join(" "),
      MAX_NAME,
    );
  }

  function associatedLabelName(element) {
    if (element.labels && typeof element.labels[Symbol.iterator] === "function") {
      return clipped(
        Array.from(element.labels, (label) => label.textContent ?? "").join(" "),
        MAX_NAME,
      );
    }
    return "";
  }

  function accessibleName(element) {
    const ariaLabel = clipped(attr(element, "aria-label") ?? "", MAX_NAME);
    if (ariaLabel) return ariaLabel;
    const labelledBy = labelledByName(element);
    if (labelledBy) return labelledBy;
    const label = associatedLabelName(element);
    if (label) return label;
    const alt = clipped(attr(element, "alt") ?? "", MAX_NAME);
    if (alt) return alt;
    const tag = String(element.tagName ?? "").toLowerCase();
    if (tag === "input") {
      const type = String(element.type || "text").toLowerCase();
      if (["button", "submit", "reset"].includes(type)) {
        const value = clipped(String(element.value ?? ""), MAX_NAME);
        if (value) return value;
      }
      const placeholder = clipped(attr(element, "placeholder") ?? "", MAX_NAME);
      if (placeholder) return placeholder;
    }
    const title = clipped(attr(element, "title") ?? "", MAX_NAME);
    if (title) return title;
    return clipped(element.textContent ?? "", MAX_NAME);
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

  function safeValue(element, role) {
    if (isSensitiveElement(element) || !SETTABLE_ROLES.has(role)) return undefined;
    if (typeof element.value !== "string") return undefined;
    return clipped(element.value, MAX_VALUE);
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

  function createSnapshot(document, { maxNodes = 500, maxChars = 100_000 } = {}) {
    const nodeLimit = Math.max(1, Math.min(Number(maxNodes) || 500, 1000));
    const charLimit = Math.max(1024, Math.min(Number(maxChars) || 100_000, 500_000));
    const snapshotId = randomToken("snapshot");
    const nodes = [];
    const handles = new Map();
    let characters = 0;
    let truncated = false;

    const scrollingElement = document.scrollingElement || document.documentElement;
    if (scrollingElement && isScrollable(scrollingElement)) {
      const node = {
        nodeId: "root",
        role: "document",
        name: clipped(document.title || "Page", MAX_NAME),
        actions: ["scroll"],
        states: {},
      };
      nodes.push(node);
      handles.set("root", { element: scrollingElement, actions: node.actions, sensitive: false });
      characters += JSON.stringify(node).length;
    }

    const all = document.querySelectorAll?.("*") ?? [];
    for (const element of all) {
      if (nodes.length >= nodeLimit || characters >= charLimit) {
        truncated = true;
        break;
      }
      if (!isVisible(element) || isSensitiveElement(element)) continue;
      const role = effectiveRole(element);
      if (!role) continue;
      const actions = capabilities(element, role);
      const name = accessibleName(element);
      if (!name && actions.length === 0 && !["main", "navigation", "form", "heading"].includes(role)) {
        continue;
      }
      const nodeId = `n${nodes.length + 1}`;
      const node = { nodeId, role, name, actions, states: ariaStates(element) };
      const value = safeValue(element, role);
      if (value !== undefined) node.value = { kind: "text", text: value };
      const description = clipped(attr(element, "aria-description") ?? "", MAX_NAME);
      if (description) node.description = description;
      const rect = bounds(element);
      if (rect) node.bounds = rect;
      const serializedLength = JSON.stringify(node).length;
      if (characters + serializedLength > charLimit) {
        truncated = true;
        break;
      }
      nodes.push(node);
      handles.set(nodeId, { element, actions, sensitive: false });
      characters += serializedLength;
    }

    return {
      result: {
        snapshotId,
        nodes,
        truncated,
        coverage: "top_document",
      },
      handles,
    };
  }

  async function sha256Text(value) {
    const bytes = new TextEncoder().encode(value);
    const digest = await global.crypto.subtle.digest("SHA-256", bytes);
    return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
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

  async function performAction(handle, action, args = {}) {
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
      return { focused: element.ownerDocument?.activeElement === element };
    }
    if (action === "set_value") {
      if (typeof args.value !== "string") throw Object.assign(new Error("value must be a string"), { code: "invalid_value" });
      const encoded = new TextEncoder().encode(args.value);
      if (encoded.byteLength > MAX_SET_VALUE_BYTES) {
        throw Object.assign(new Error("value is too large"), { code: "value_too_large" });
      }
      setNativeValue(element, args.value);
      const view = element.ownerDocument?.defaultView ?? global;
      const InputEventCtor = view.InputEvent ?? view.Event;
      element.dispatchEvent(new InputEventCtor("input", { bubbles: true, inputType: "insertText" }));
      element.dispatchEvent(new view.Event("change", { bubbles: true }));
      return {
        valueUtf8Bytes: encoded.byteLength,
        valueSha256: await sha256Text(args.value),
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
