//! Bounded, screenshot-free semantic AX snapshot collection.
//!
//! This is intentionally independent from Set-of-Mark discovery. In
//! particular it never asks ScreenCaptureKit to identify a window: the
//! `UiTarget` was resolved through AX/NSWorkspace and the tree is walked
//! directly from the application root.

use super::attrs::{
    ax_actions, ax_bool, ax_description, ax_help, ax_role, ax_string, ax_subrole, ax_title,
    ax_value_string, ax_window_id, element_rect, is_secure_field, process_is_trusted,
    rects_intersect, CLICK_ACTIONS,
};
use super::model::{is_actionable, AxHandle};
use super::walk::{child_elements, CoordLift, MAX_DEPTH};
use super::warmth::enable_web_accessibility;
use crate::platform::{
    UiBounds, UiNode, UiNodeStates, UiNodeValue, UiPartialReason, UiReadCoverage, UiReadError,
    UiReadErrorKind, UiReadMode, UiSnapshot, UiSnapshotOptions, UiTarget,
};
use accessibility::AXUIElement;
use core_foundation::base::TCFType;
use std::collections::HashSet;
use std::time::{Duration, Instant};

const MAX_VISITED_NODES: usize = 6_000;
const MAX_FIELD_CHARS: usize = 4_096;
const WEB_MATERIALIZE_BUDGET: Duration = Duration::from_secs(5);
const WEB_MATERIALIZE_STEP: Duration = Duration::from_millis(300);
const WEB_STABLE_PASSES: usize = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct WebRichness {
    saw_area: bool,
    descendants: usize,
    emitted: usize,
}

impl WebRichness {
    fn is_materialized(self) -> bool {
        self.saw_area && self.descendants > 0
    }
}

fn observe_web_pass(
    best: &mut WebRichness,
    stable_passes: &mut usize,
    candidate: WebRichness,
) -> bool {
    if candidate > *best {
        *best = candidate;
        *stable_passes = 0;
        true
    } else if candidate == *best && candidate.is_materialized() {
        *stable_passes += 1;
        true
    } else {
        *stable_passes = 0;
        false
    }
}

#[derive(Debug, Clone)]
pub(super) struct SemanticAnchor {
    pub window: AxHandle,
    pub window_id: u32,
}

pub(super) struct SemanticElement {
    pub node: UiNode,
    pub handle: Option<SemanticHandle>,
}

pub(super) type SemanticHandle = (AxHandle, Option<SemanticAnchor>);

fn deadline_error() -> UiReadError {
    UiReadError::new(
        UiReadErrorKind::TimedOut,
        "Accessibility tree read exceeded its deadline",
    )
}

fn clean_field(value: String) -> String {
    let value = value.trim();
    if value.chars().count() <= MAX_FIELD_CHARS {
        return value.to_string();
    }
    value.chars().take(MAX_FIELD_CHARS).collect()
}

fn bounds(rect: (f64, f64, f64, f64)) -> UiBounds {
    UiBounds {
        x: rect.0,
        y: rect.1,
        width: rect.2,
        height: rect.3,
    }
}

fn node_chars(node: &UiNode) -> usize {
    node.role.chars().count()
        + node.name.chars().count()
        + node.description.chars().count()
        + node.value.as_filter_text().chars().count()
        + node
            .actions
            .iter()
            .map(|action| action.chars().count())
            .sum::<usize>()
}

fn readable_role(role: &str) -> bool {
    matches!(
        role,
        "AXStaticText"
            | "AXHeading"
            | "AXLabel"
            | "AXTextField"
            | "AXSecureTextField"
            | "AXTextArea"
            | "AXComboBox"
            | "AXLink"
            | "AXButton"
            | "AXCheckBox"
            | "AXRadioButton"
            | "AXMenuItem"
            | "AXTab"
            | "AXRow"
            | "AXCell"
            | "AXListItem"
    )
}

fn should_emit(mode: UiReadMode, actionable: bool, content: bool) -> bool {
    (mode.includes_interactive() && actionable) || (mode.includes_content() && content)
}

struct Walk<'a> {
    target: &'a UiTarget,
    options: UiSnapshotOptions,
    out: Vec<SemanticElement>,
    path: HashSet<usize>,
    emitted: HashSet<usize>,
    visited: usize,
    chars: usize,
    partial_reason: Option<UiPartialReason>,
    web: WebRichness,
}

impl<'a> Walk<'a> {
    fn run(root: &AXUIElement, target: &'a UiTarget, options: UiSnapshotOptions) -> Self {
        let mut walk = Self {
            target,
            options,
            out: Vec::new(),
            path: HashSet::new(),
            emitted: HashSet::new(),
            visited: 0,
            chars: 0,
            partial_reason: None,
            web: WebRichness::default(),
        };
        walk.recurse(root, 0, false, None, None);
        walk
    }

    fn stop(&mut self, reason: UiPartialReason) {
        if self.partial_reason.is_none() {
            self.partial_reason = Some(reason);
        }
    }

    fn recurse(
        &mut self,
        element: &AXUIElement,
        depth: usize,
        in_web: bool,
        lift: Option<CoordLift>,
        anchor: Option<SemanticAnchor>,
    ) {
        if self.partial_reason.is_some() {
            return;
        }
        if Instant::now() >= self.options.deadline {
            self.stop(UiPartialReason::Deadline);
            return;
        }
        if self.visited >= MAX_VISITED_NODES {
            self.stop(UiPartialReason::ProviderPartial);
            return;
        }
        if depth >= MAX_DEPTH {
            self.stop(UiPartialReason::ProviderPartial);
            return;
        }

        let identity = element.as_concrete_TypeRef() as usize;
        if !self.path.insert(identity) {
            return;
        }
        self.visited += 1;

        let role = ax_role(element);
        if ax_bool(element, "AXHidden") == Some(true)
            || ax_bool(element, "AXMinimized") == Some(true)
        {
            self.path.remove(&identity);
            return;
        }

        // A concrete non-target sibling window can be pruned without losing
        // browser content exposed directly under the application root.
        if role == "AXWindow" {
            if let (Some(expected), Some(actual)) =
                (self.target.window_id, ax_window_id(element).map(u64::from))
            {
                if expected != actual {
                    self.path.remove(&identity);
                    return;
                }
            }
        }

        let clip = self.target.bounds.map(UiBounds::as_tuple);
        let target_window_id = self.target.window_id.and_then(|id| u32::try_from(id).ok());
        let is_target_window = role == "AXWindow"
            && match (target_window_id, ax_window_id(element)) {
                (Some(expected), Some(actual)) => expected == actual,
                _ => false,
            };
        let lift = match (lift, clip, is_target_window) {
            (None, Some(clip), true) => CoordLift::derive(element, clip, target_window_id),
            _ => lift,
        };
        let anchor = if is_target_window {
            target_window_id.map(|window_id| SemanticAnchor {
                window: AxHandle(element.clone()),
                window_id,
            })
        } else {
            anchor
        };
        // AX bounds are not assumed global unless they can be reconciled with
        // the independent CoreGraphics metadata anchor selected above. When
        // that anchor is unavailable, semantic actions remain usable but
        // coordinate output/fallback is deliberately omitted.
        let rect = match (element_rect(element), anchor.as_ref(), clip) {
            (Some(rect), Some(_), Some(_)) => Some(CoordLift::lift(lift, rect)),
            _ => None,
        };
        if matches!(
            (clip, rect),
            (Some(clip), Some(rect))
                if rect.2 >= 1.0 && rect.3 >= 1.0 && !rects_intersect(rect, clip)
        ) {
            self.path.remove(&identity);
            return;
        }
        let in_web = in_web || role == "AXWebArea";
        let web_descendant = in_web && role != "AXWebArea";
        if role == "AXWebArea" {
            self.web.saw_area = true;
        } else if web_descendant {
            self.web.descendants += 1;
        }

        let actions = ax_actions(element);
        let actionable = is_actionable(&role)
            || CLICK_ACTIONS
                .iter()
                .any(|expected| actions.iter().any(|actual| actual == expected));

        // Classify secure controls before reading AXValue. This ordering is a
        // security boundary: even a value that would later be redacted must
        // never enter a temporary String.
        let subrole = ax_subrole(element);
        let secure = is_secure_field(element, &role, &subrole);
        let title = clean_field(ax_title(element));
        let explicit_label = clean_field(ax_string(element, "AXLabel"));
        let ax_description = clean_field(ax_description(element));
        let help = clean_field(ax_help(element));
        let name = if !title.is_empty() {
            title
        } else if !explicit_label.is_empty() {
            explicit_label
        } else {
            ax_description.clone()
        };
        let description = match (ax_description.as_str(), help.as_str()) {
            ("", "") => String::new(),
            ("", help) => help.to_string(),
            (description, "") => description.to_string(),
            (description, help) if description == help => description.to_string(),
            (description, help) => format!("{description}; {help}"),
        };
        let value = if secure {
            UiNodeValue::Redacted
        } else if readable_role(&role) || role == "AXDocument" || role == "AXWebArea" {
            let value = clean_field(ax_value_string(element));
            if value.is_empty() {
                UiNodeValue::Absent
            } else {
                UiNodeValue::Text(value)
            }
        } else {
            UiNodeValue::Absent
        };
        let content = readable_role(&role)
            || !name.is_empty()
            || !description.is_empty()
            || !matches!(&value, UiNodeValue::Absent);

        if should_emit(self.options.mode, actionable, content) && self.emitted.insert(identity) {
            let checked = if matches!(role.as_str(), "AXCheckBox" | "AXRadioButton" | "AXSwitch") {
                ax_bool(element, "AXValue")
            } else {
                None
            };
            let node = UiNode {
                role,
                name,
                description,
                value,
                actions,
                states: UiNodeStates {
                    enabled: ax_bool(element, "AXEnabled"),
                    focused: ax_bool(element, "AXFocused"),
                    selected: ax_bool(element, "AXSelected"),
                    checked,
                    expanded: ax_bool(element, "AXExpanded"),
                },
                bounds: rect.map(bounds),
                depth,
                actionable,
            };
            let chars = node_chars(&node);
            if self.out.len() >= self.options.max_nodes {
                self.stop(UiPartialReason::NodeLimit);
            } else if self.chars.saturating_add(chars) > self.options.max_chars {
                self.stop(UiPartialReason::CharacterLimit);
            } else {
                self.chars += chars;
                self.out.push(SemanticElement {
                    handle: actionable.then(|| (AxHandle(element.clone()), anchor.clone())),
                    node,
                });
                if web_descendant {
                    self.web.emitted += 1;
                }
            }
        }

        if self.partial_reason.is_none() {
            for child in child_elements(element, &ax_role(element)) {
                self.recurse(&child, depth + 1, in_web, lift, anchor.clone());
                if self.partial_reason.is_some() {
                    break;
                }
            }
        }
        self.path.remove(&identity);
    }
}

pub(super) fn read_snapshot(
    target: &UiTarget,
    options: UiSnapshotOptions,
) -> Result<(UiSnapshot, Vec<Option<SemanticHandle>>), UiReadError> {
    if !process_is_trusted() {
        return Err(UiReadError::new(
            UiReadErrorKind::PermissionDenied,
            "macOS Accessibility permission is not granted to the process hosting Nova",
        ));
    }
    if Instant::now() >= options.deadline {
        return Err(deadline_error());
    }

    let app = AXUIElement::application(target.pid);
    app.set_messaging_timeout(
        options
            .deadline
            .saturating_duration_since(Instant::now())
            .as_secs_f32()
            .clamp(0.05, 0.5),
    )
    .map_err(|error| {
        UiReadError::new(
            UiReadErrorKind::BackendFailure,
            format!("failed to set AX provider timeout: {error:?}"),
        )
    })?;
    if ax_role(&app).is_empty() {
        return Err(UiReadError::new(
            UiReadErrorKind::NoSemanticTree,
            format!("{} does not expose an Accessibility tree", target.app_name),
        ));
    }

    // Best effort: ask Chromium/Electron to materialize its semantic web tree.
    // A cold provider gets a bounded wait for non-empty descendants and a
    // stable richness plateau; this path never invokes a screenshot fallback.
    let web_capable = enable_web_accessibility(&app);
    let mut walk = Walk::run(&app, target, options);
    if web_capable && !walk.web.is_materialized() && walk.partial_reason.is_none() {
        let materialize_deadline =
            std::cmp::min(options.deadline, Instant::now() + WEB_MATERIALIZE_BUDGET);
        let mut best = walk.web;
        let mut stable_passes = 0;
        while Instant::now() < materialize_deadline && stable_passes < WEB_STABLE_PASSES {
            let remaining = materialize_deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(std::cmp::min(WEB_MATERIALIZE_STEP, remaining));
            if Instant::now() >= materialize_deadline {
                break;
            }
            let _ = enable_web_accessibility(&app);
            let retry = Walk::run(&app, target, options);
            if retry.partial_reason.is_some() {
                walk = retry;
                break;
            }
            if observe_web_pass(&mut best, &mut stable_passes, retry.web) {
                walk = retry;
            }
        }
        if (stable_passes < WEB_STABLE_PASSES || !walk.web.is_materialized())
            && walk.partial_reason.is_none()
        {
            walk.stop(if Instant::now() >= options.deadline {
                UiPartialReason::Deadline
            } else {
                UiPartialReason::ProviderPartial
            });
        }
    }
    if walk.out.is_empty() && walk.partial_reason == Some(UiPartialReason::Deadline) {
        return Err(deadline_error());
    }

    let coverage = if walk.out.is_empty() {
        UiReadCoverage::Empty
    } else if walk.partial_reason.is_some() {
        UiReadCoverage::Partial
    } else {
        UiReadCoverage::Complete
    };
    let truncated = walk.partial_reason.is_some();
    let (nodes, handles): (Vec<_>, Vec<_>) = walk
        .out
        .into_iter()
        .map(|element| (element.node, element.handle))
        .unzip();
    let snapshot = UiSnapshot {
        target: target.clone(),
        nodes: nodes
            .into_iter()
            .map(|node| crate::platform::CollectedUiNode { node, handle: None })
            .collect(),
        coverage,
        truncated,
        partial_reason: walk.partial_reason,
    };
    Ok((snapshot, handles))
}

#[cfg(test)]
mod tests {
    use super::{observe_web_pass, WebRichness, WEB_STABLE_PASSES};

    #[test]
    fn web_area_without_descendants_is_not_materialized() {
        assert!(!WebRichness {
            saw_area: true,
            descendants: 0,
            emitted: 1,
        }
        .is_materialized());
    }

    #[test]
    fn web_area_with_descendants_is_materialized() {
        assert!(WebRichness {
            saw_area: true,
            descendants: 1,
            emitted: 0,
        }
        .is_materialized());
    }

    #[test]
    fn web_materialization_waits_for_a_stable_richness_plateau() {
        let empty = WebRichness {
            saw_area: true,
            descendants: 0,
            emitted: 0,
        };
        let rich = WebRichness {
            saw_area: true,
            descendants: 3,
            emitted: 2,
        };
        let mut best = WebRichness::default();
        let mut stable_passes = 0;

        assert!(observe_web_pass(&mut best, &mut stable_passes, empty));
        assert_eq!(stable_passes, 0);
        assert!(observe_web_pass(&mut best, &mut stable_passes, rich));
        assert_eq!(stable_passes, 0);
        for expected in 1..=WEB_STABLE_PASSES {
            assert!(observe_web_pass(&mut best, &mut stable_passes, rich));
            assert_eq!(stable_passes, expected);
        }
    }
}
