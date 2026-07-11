//! Thread-local COM/UI Automation plumbing shared by every other submodule
//! here: joining the process's Multi-Threaded Apartment, activating
//! `IUIAutomation`, the `CacheRequest`/`Condition` builders discovery and the
//! query-driven actions both need, and the pattern-ladder helpers
//! `handle.rs`/`actions.rs` drive a click through.
//!
//! # COM threading model (read this before touching anything else here)
//!
//! UI Automation calls are synchronous, cross-process, and can take hundreds
//! of milliseconds — `server.rs` already runs every `UiTree` entry point on a
//! `tokio::task::spawn_blocking` thread (matching macOS's AX calls), never the
//! async executor. Each such call MUST join the process's **Multi-Threaded
//! Apartment** (`COINIT_MULTITHREADED`), NOT a Single-Threaded Apartment: an
//! STA requires a Windows message pump on that thread to receive incoming
//! calls, and nova's blocking-pool threads never run one — an STA join here
//! would deadlock the first cross-apartment callback. [`ensure_com_mta`]
//! joins the MTA (idempotent, thread-local) and MUST be the first thing any
//! entry point in this `elements` module does — mirrors
//! `platform::windows::ensure_dpi_awareness`'s "cheap to call every time"
//! contract.
//!
//! [`with_automation`] activates a FRESH `IUIAutomation` instance every call
//! rather than caching one in a `thread_local` — see its doc comment for why
//! that caching was tried first and had to be reverted: a live COM interface
//! pointer sitting in a `thread_local` is only released by a TLS destructor
//! at thread/process-exit time, whose ordering against COM's own internal
//! per-thread teardown is unguaranteed and, in testing, reproducibly crashed
//! (`STATUS_ACCESS_VIOLATION`) on exit.
//!
//! A COM interface pointer obtained on one MTA thread (e.g. the thread that
//! ran `collect_actionable`) CAN be handed directly (unmarshaled) to another
//! MTA thread of the SAME process (e.g. the thread `click_mark` later runs
//! on) — within one process there is only ever one MTA, and COM permits
//! direct use of an interface from any thread that has joined it. This is
//! what makes `WinElementHandle` (in `handle.rs`) sound as `Send`: see its
//! doc comment for the exact invariant. (This is unrelated to the
//! `thread_local`/`IUIAutomation` caching hazard above: `WinElementHandle`
//! holds an `IUIAutomationElement`, which — unlike the `automation.rs`
//! coordinator object — is NEVER cached in a `thread_local`; it is owned by
//! ordinary Rust values — `Vec`s, `Box<dyn ElementHandle>` — with normal,
//! deterministic scope-based `Drop`, which is exactly the safe pattern this
//! module's own `IUIAutomation` handling now follows too.)
use windows::core::VARIANT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationCondition,
    UIA_BoundingRectanglePropertyId, UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId,
    UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId, UIA_DataItemControlTypeId,
    UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_ExpandCollapsePatternId,
    UIA_GroupControlTypeId, UIA_HyperlinkControlTypeId, UIA_ImageControlTypeId,
    UIA_InvokePatternId, UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsInvokePatternAvailablePropertyId, UIA_IsKeyboardFocusablePropertyId,
    UIA_IsOffscreenPropertyId, UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId, UIA_IsValuePatternAvailablePropertyId,
    UIA_ListControlTypeId, UIA_ListItemControlTypeId, UIA_MenuBarControlTypeId,
    UIA_MenuControlTypeId, UIA_MenuItemControlTypeId, UIA_NamePropertyId, UIA_PaneControlTypeId,
    UIA_RadioButtonControlTypeId, UIA_SelectionItemPatternId, UIA_SliderControlTypeId,
    UIA_SplitButtonControlTypeId, UIA_TabItemControlTypeId, UIA_TableControlTypeId,
    UIA_TextControlTypeId, UIA_TogglePatternId, UIA_ToolBarControlTypeId, UIA_TreeControlTypeId,
    UIA_TreeItemControlTypeId, UIA_ValuePatternId, UIA_WindowControlTypeId, UIA_CONTROLTYPE_ID,
    UIA_PROPERTY_ID,
};

thread_local! {
    /// Forces [`ensure_com_mta`]'s `CoInitializeEx` to run exactly once per
    /// thread (the `thread_local!` initializer itself is the "once" — reading
    /// the cell is what triggers it, and subsequent reads are a plain
    /// thread-local load).
    static COM_MTA_JOINED: () = {
        // SAFETY: `None`/`COINIT_MULTITHREADED` are documented, pointer-free
        // arguments; safe to call from any thread, any number of times (each
        // call past the first just bumps a per-thread refcount and returns
        // S_FALSE, which `is_err()` treats as Ok — see below).
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK (first join) and S_FALSE (already joined, refcounted) both
        // indicate this thread is now in the MTA — `HRESULT::is_err()` is
        // false for both. We deliberately never pair this with
        // `CoUninitialize`: these are tokio blocking-pool threads that, in
        // practice, live for the process's duration; leaking the COM join
        // until thread exit (cleaned up by the OS/CRT) is simpler than
        // threading a matching teardown through a thread pool we don't own.
        if hr.is_err() {
            // RPC_E_CHANGED_MODE would mean this thread already joined an STA
            // elsewhere — not expected for tokio's blocking pool (nothing else
            // in nova calls CoInitializeEx), but logged rather than panicking;
            // subsequent UIA calls on this thread then fail loudly (wrong
            // apartment) instead of silently deadlocking.
            tracing::warn!(
                "CoInitializeEx(COINIT_MULTITHREADED) returned {hr:?} on this thread — \
                 UI Automation calls here may fail (see platform::windows::elements::automation's \
                 module doc)"
            );
        }
    };
}

/// Join this thread to the process's Multi-Threaded Apartment. MUST run
/// before any other call in this module — see the module doc. Cheap after the
/// first call (a thread-local read).
pub(super) fn ensure_com_mta() {
    COM_MTA_JOINED.with(|_| {});
}

/// Run `f` with a fresh `IUIAutomation` instance, activated via
/// `CoCreateInstance(CUIAutomation)` for THIS call and dropped (released)
/// again before returning — deliberately NOT cached in a `thread_local!`
/// despite `ensure_com_mta`'s join being cached that way. This was an earlier
/// design (one instance per thread, reused across calls) that turned out to
/// be a real, reproducible crash: a `thread_local`-cached `IUIAutomation`
/// (or any live COM interface pointer) can outlive ordinary Rust scoping and
/// only gets `Release`d by a TLS destructor running at thread-exit or
/// process-exit time — and that destructor's ordering relative to COM's OWN
/// internal per-thread/per-process teardown (`combase.dll`'s
/// `DLL_THREAD_DETACH`/`DLL_PROCESS_DETACH` handling) is NOT guaranteed by
/// either Rust or Win32. In practice this manifested as a real
/// `STATUS_ACCESS_VIOLATION` (0xC0000005) on process exit, reproduced via
/// `--marks` against a live Calculator window in the Windows VM (COM's own
/// teardown had already invalidated the apartment/proxy state by the time
/// Rust's thread-local destructor tried to `Release()` the cached instance) —
/// see the PR body for the exact repro. `CoCreateInstance(CUIAutomation)` is
/// a cheap, purely in-process activation (not the cross-process RPC that
/// actually costs time — that's the `FindAllBuildCache` call itself), so
/// creating one per call and letting ordinary Rust `Drop` release it at the
/// end of THIS synchronous call (always well before any thread/process
/// teardown could race it) costs nothing meaningful while removing the whole
/// hazard. `ensure_com_mta`'s apartment join stays cached in a `thread_local`
/// because its value is `()` — no `Drop` glue, hence nothing for a teardown
/// race to corrupt.
pub(super) fn with_automation<T>(
    f: impl FnOnce(&IUIAutomation) -> Result<T, String>,
) -> Result<T, String> {
    ensure_com_mta();
    // SAFETY: standard in-process COM activation of a well-known Microsoft
    // coclass (`CUIAutomation`); no pointers of ours involved beyond the
    // out-param `windows-rs` fills in. `automation` is dropped (released) at
    // the end of this function, on this same thread, synchronously.
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("CoCreateInstance(CUIAutomation) failed: {e}"))?;
    f(&automation)
}

// ── CacheRequest: batch every property/pattern into ONE round trip ──────

/// Build the [`IUIAutomationCacheRequest`] every discovery call uses: the
/// properties and patterns `discover.rs`'s `to_ui_element` and `handle.rs`'s
/// pattern ladder need, all fetched in the SAME cross-process `FindAllBuildCache`
/// call that finds the elements — this is the perf-critical piece (see the
/// crate/PR doc): reading any of these back afterward via `Cached*`/
/// `GetCachedPropertyValue` is then a pure LOCAL read of the already-fetched
/// blob, not another RPC. Doing this per-property in a loop instead (N nodes ×
/// M properties = N×M round trips) is what would make a real page's worth of
/// marks unusably slow.
pub(super) fn build_cache_request(
    automation: &IUIAutomation,
) -> windows::core::Result<IUIAutomationCacheRequest> {
    unsafe {
        let cr = automation.CreateCacheRequest()?;
        cr.AddProperty(UIA_NamePropertyId)?;
        cr.AddProperty(UIA_ControlTypePropertyId)?;
        cr.AddProperty(UIA_BoundingRectanglePropertyId)?;
        cr.AddProperty(UIA_IsOffscreenPropertyId)?;
        cr.AddProperty(UIA_IsInvokePatternAvailablePropertyId)?;
        cr.AddProperty(UIA_IsTogglePatternAvailablePropertyId)?;
        cr.AddProperty(UIA_IsExpandCollapsePatternAvailablePropertyId)?;
        cr.AddProperty(UIA_IsSelectionItemPatternAvailablePropertyId)?;
        cr.AddPattern(UIA_InvokePatternId)?;
        cr.AddPattern(UIA_TogglePatternId)?;
        cr.AddPattern(UIA_ExpandCollapsePatternId)?;
        cr.AddPattern(UIA_SelectionItemPatternId)?;
        Ok(cr)
    }
}

/// Control types treated as actionable BY ROLE ALONE (mirrors macOS
/// `mac::elements::model::is_actionable`'s AX-role allowlist) — an element
/// matching one of these OR exposing a click-ish pattern (see
/// [`build_actionable_condition`]) is offered as a mark.
const ACTIONABLE_CONTROL_TYPES: &[UIA_CONTROLTYPE_ID] = &[
    UIA_ButtonControlTypeId,
    UIA_HyperlinkControlTypeId,
    UIA_EditControlTypeId,
    UIA_CheckBoxControlTypeId,
    UIA_RadioButtonControlTypeId,
    UIA_ComboBoxControlTypeId,
    UIA_MenuItemControlTypeId,
    UIA_TabItemControlTypeId,
    UIA_ListItemControlTypeId,
    UIA_SliderControlTypeId,
    UIA_SplitButtonControlTypeId,
    UIA_TreeItemControlTypeId,
    UIA_DataItemControlTypeId,
];

/// The four click-ish pattern-availability properties, in the SAME preference
/// order `handle.rs::pattern_for` tries them.
const PATTERN_AVAILABLE_PROPS: &[UIA_PROPERTY_ID] = &[
    UIA_IsInvokePatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId,
    UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsSelectionItemPatternAvailablePropertyId,
];

/// The `FindAll` condition for Set-of-Mark discovery: a click-ish pattern
/// available (Invoke/Toggle/ExpandCollapse/SelectionItem — covers web/custom
/// controls that report a generic `ControlType` but still respond to a
/// pattern, e.g. Chromium/WebView2-hosted DOM content) OR a control type in
/// the allowlist (covers native controls that for whatever reason don't
/// expose one of those four, e.g. some plain `Edit` fields only expose
/// `ValuePattern`). Mirrors macOS `model::is_target`'s
/// `is_actionable(role) || click_action_for(el).is_some()`. Evaluated
/// server-side by `FindAllBuildCache` — one cross-process call returns
/// exactly the matching set, rather than a client-side walk that would visit
/// every node in the tree.
pub(super) fn build_actionable_condition(
    automation: &IUIAutomation,
) -> windows::core::Result<IUIAutomationCondition> {
    unsafe {
        let mut conds: Vec<Option<IUIAutomationCondition>> = Vec::new();
        for &prop in PATTERN_AVAILABLE_PROPS {
            conds.push(Some(
                automation.CreatePropertyCondition(prop, &VARIANT::from(true))?,
            ));
        }
        for &ct in ACTIONABLE_CONTROL_TYPES {
            conds.push(Some(automation.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(ct.0),
            )?));
        }
        automation.CreateOrConditionFromNativeArray(&conds)
    }
}

/// A broader condition for the query-driven `ax_click`/`ax_set_value`/
/// `ax_focus` actions: [`build_actionable_condition`]'s set, OR'd with
/// keyboard-focusable elements and `ValuePattern`-bearing elements — so a
/// plain (non-actionable-by-marks-standards) focusable label or a text field
/// that only exposes `ValuePattern` is still reachable by a query, matching
/// macOS's `actions::find_matching` walking the WHOLE tree rather than just
/// the mark-worthy subset.
pub(super) fn build_queryable_condition(
    automation: &IUIAutomation,
) -> windows::core::Result<IUIAutomationCondition> {
    unsafe {
        let actionable = build_actionable_condition(automation)?;
        let focusable = automation
            .CreatePropertyCondition(UIA_IsKeyboardFocusablePropertyId, &VARIANT::from(true))?;
        let has_value = automation
            .CreatePropertyCondition(UIA_IsValuePatternAvailablePropertyId, &VARIANT::from(true))?;
        automation.CreateOrConditionFromNativeArray(&[
            Some(actionable),
            Some(focusable),
            Some(has_value),
        ])
    }
}

/// `(id, name)` pairs backing [`control_type_name`]. A lookup TABLE rather
/// than a `match` on principle: the `windows` crate's generated `UIA_*Id`
/// constants are PascalCase (mirroring the COM API's own naming), which trips
/// rustc's `non_upper_case_globals` lint when used as MATCH PATTERNS (it
/// can't tell "a constant path" from "a new binding" apart from the casing
/// convention) — comparing `.0` values in a table lookup sidesteps that
/// entirely instead of sprinkling `#[allow]` over two dozen arms.
const CONTROL_TYPE_NAMES: &[(UIA_CONTROLTYPE_ID, &str)] = &[
    (UIA_ButtonControlTypeId, "Button"),
    (UIA_HyperlinkControlTypeId, "Hyperlink"),
    (UIA_EditControlTypeId, "Edit"),
    (UIA_CheckBoxControlTypeId, "CheckBox"),
    (UIA_RadioButtonControlTypeId, "RadioButton"),
    (UIA_ComboBoxControlTypeId, "ComboBox"),
    (UIA_MenuItemControlTypeId, "MenuItem"),
    (UIA_TabItemControlTypeId, "TabItem"),
    (UIA_ListItemControlTypeId, "ListItem"),
    (UIA_SliderControlTypeId, "Slider"),
    (UIA_SplitButtonControlTypeId, "SplitButton"),
    (UIA_TreeItemControlTypeId, "TreeItem"),
    (UIA_DataItemControlTypeId, "DataItem"),
    (UIA_MenuControlTypeId, "Menu"),
    (UIA_MenuBarControlTypeId, "MenuBar"),
    (UIA_ToolBarControlTypeId, "ToolBar"),
    (UIA_TextControlTypeId, "Text"),
    (UIA_ImageControlTypeId, "Image"),
    (UIA_GroupControlTypeId, "Group"),
    (UIA_PaneControlTypeId, "Pane"),
    (UIA_WindowControlTypeId, "Window"),
    (UIA_ListControlTypeId, "List"),
    (UIA_TreeControlTypeId, "Tree"),
    (UIA_TableControlTypeId, "Table"),
    (UIA_DocumentControlTypeId, "Document"),
];

/// A short, human-readable name for a `ControlType` id (e.g. `Button`,
/// `Hyperlink`) — the Windows analog of macOS's `AXButton`/`AXLink` role
/// strings, used as [`crate::tools::elements::UiElement::role`]. Falls back to
/// `ControlType(<id>)` for anything not in [`CONTROL_TYPE_NAMES`] (still
/// useful to a model deciding whether to click a mark, just less pretty).
pub(super) fn control_type_name(ct: UIA_CONTROLTYPE_ID) -> String {
    CONTROL_TYPE_NAMES
        .iter()
        .find(|(id, _)| *id == ct)
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| format!("ControlType({})", ct.0))
}

// ── Pattern ladder (shared by handle.rs's click and actions.rs's ax_click) ──

/// Read a boolean property LIVE (`GetCurrentPropertyValue`, not cached) — used
/// for pattern-availability checks at click time, when the element may have
/// been found just now (via `FindFirst`/`GetParentElement`, neither of which
/// goes through our `CacheRequest`) rather than at the original marks
/// discovery. Returns `false` (never panics) on any read failure — a torn-down
/// or disconnected element just looks "no pattern available", which correctly
/// falls through to the next rung of the ladder / a final clean error.
fn bool_prop(
    el: &windows::Win32::UI::Accessibility::IUIAutomationElement,
    prop: UIA_PROPERTY_ID,
) -> bool {
    // SAFETY: reading a property is a documented, side-effect-free COM call.
    unsafe { el.GetCurrentPropertyValue(prop) }
        .ok()
        .and_then(|v| bool::try_from(&v).ok())
        .unwrap_or(false)
}

/// The click-ish pattern `el` supports right now, in the same preference
/// order macOS's `attrs::CLICK_ACTIONS` tries AX actions: `Invoke` (buttons/
/// links/menu items — most cases, and the one that actually fires a
/// Chromium/WebView2 DOM element's own click handler — see the module/PR
/// doc), then `Toggle` (checkboxes), then `SelectionItem` (radios/list rows/
/// tabs), then `ExpandCollapse` (combos/trees/disclosure triangles). `None` if
/// the element exposes none of the four.
pub(super) fn pattern_for(
    el: &windows::Win32::UI::Accessibility::IUIAutomationElement,
) -> Option<&'static str> {
    if bool_prop(el, UIA_IsInvokePatternAvailablePropertyId) {
        Some("Invoke")
    } else if bool_prop(el, UIA_IsTogglePatternAvailablePropertyId) {
        Some("Toggle")
    } else if bool_prop(el, UIA_IsSelectionItemPatternAvailablePropertyId) {
        Some("SelectionItem")
    } else if bool_prop(el, UIA_IsExpandCollapsePatternAvailablePropertyId) {
        Some("ExpandCollapse")
    } else {
        None
    }
}

/// Perform `action` (as returned by [`pattern_for`], which guarantees it's one
/// of these four) on `el` by fetching the LIVE pattern object and invoking it.
pub(super) fn invoke_pattern(
    el: &windows::Win32::UI::Accessibility::IUIAutomationElement,
    action: &'static str,
) -> Result<(), String> {
    use windows::Win32::UI::Accessibility::{
        IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern,
        IUIAutomationSelectionItemPattern, IUIAutomationTogglePattern,
    };
    // SAFETY: every call below is a synchronous, documented COM call on a
    // pattern object just retrieved from `el`; `el` is a live element
    // reference (this module never uses `AutomationElementMode_None` — see
    // `discover.rs`'s doc for why that mode isn't used here).
    unsafe {
        match action {
            "Invoke" => {
                let p: IUIAutomationInvokePattern = el
                    .GetCurrentPatternAs(UIA_InvokePatternId)
                    .map_err(|e| format!("GetCurrentPatternAs(Invoke) failed: {e}"))?;
                p.Invoke().map_err(|e| format!("Invoke() failed: {e}"))
            }
            "Toggle" => {
                let p: IUIAutomationTogglePattern = el
                    .GetCurrentPatternAs(UIA_TogglePatternId)
                    .map_err(|e| format!("GetCurrentPatternAs(Toggle) failed: {e}"))?;
                p.Toggle().map_err(|e| format!("Toggle() failed: {e}"))
            }
            "SelectionItem" => {
                let p: IUIAutomationSelectionItemPattern = el
                    .GetCurrentPatternAs(UIA_SelectionItemPatternId)
                    .map_err(|e| format!("GetCurrentPatternAs(SelectionItem) failed: {e}"))?;
                p.Select().map_err(|e| format!("Select() failed: {e}"))
            }
            "ExpandCollapse" => {
                let p: IUIAutomationExpandCollapsePattern = el
                    .GetCurrentPatternAs(UIA_ExpandCollapsePatternId)
                    .map_err(|e| format!("GetCurrentPatternAs(ExpandCollapse) failed: {e}"))?;
                p.Expand().map_err(|e| format!("Expand() failed: {e}"))
            }
            _ => unreachable!("pattern_for only ever returns one of the four arms above"),
        }
    }
}

/// The `ValuePattern` on `el`, if it exposes one — used by `actions::ax_set_value`.
pub(super) fn value_pattern(
    el: &windows::Win32::UI::Accessibility::IUIAutomationElement,
) -> windows::core::Result<windows::Win32::UI::Accessibility::IUIAutomationValuePattern> {
    // SAFETY: documented COM call; the returned pattern is used immediately
    // by the caller (not stored), on the same live element reference.
    unsafe { el.GetCurrentPatternAs(UIA_ValuePatternId) }
}
