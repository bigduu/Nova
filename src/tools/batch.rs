/// Batch execution — execute a sequence of input actions in one MCP call.
///
/// Reduces round-trips for deterministic multi-step interactions (e.g. click a
/// field, type, press return). Screenshots are intentionally *not* part of a
/// batch: they return image content rather than a status string, so an agent
/// takes a screenshot with the dedicated `screenshot` tool after a batch runs.
use crate::display::view::ViewFrame;
use crate::error::Result;
use crate::tools::input::InputTarget;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const MAX_BATCH_ACTIONS: usize = 64;
pub const MAX_BATCH_DETAIL_CHARS: usize = 512;

/// Output-only progress: never include the submitted actions or typed text.
#[derive(Debug, Serialize)]
pub struct CompletedAction {
    pub index: usize,
    pub result: String,
}

/// Half-open range of actions that were never attempted.
#[derive(Debug, Serialize)]
pub struct NotExecuted {
    pub start: usize,
    pub end_exclusive: usize,
}

#[derive(Debug, Serialize)]
pub struct BatchFailure {
    pub completed: Vec<CompletedAction>,
    /// None means preflight rejection: no action was attempted.
    pub failed_index: Option<usize>,
    pub reason: String,
    pub not_executed: NotExecuted,
}

/// A single action in a batch sequence. Coordinates are in screenshot space,
/// matching the individual tools.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action")]
pub enum BatchAction {
    #[serde(rename = "mouse_move")]
    MouseMove { x: f64, y: f64 },
    #[serde(rename = "left_click")]
    LeftClick { x: f64, y: f64 },
    #[serde(rename = "right_click")]
    RightClick { x: f64, y: f64 },
    #[serde(rename = "double_click")]
    DoubleClick { x: f64, y: f64 },
    #[serde(rename = "scroll")]
    Scroll { lines: i32 },
    #[serde(rename = "key_combo")]
    KeyCombo { key: String },
    #[serde(rename = "type_text")]
    TypeText { text: String },
    #[serde(rename = "wait")]
    Wait { ms: u64 },
}

/// Execute a sequence of actions in order, stopping at the first failure.
/// Returns a status line for each completed action, or bounded failure progress.
/// The failed action may already have caused side effects. Coordinates are
/// mapped to logical points through the active screenshot's `view` frame.
pub async fn execute_batch(
    actions: Vec<BatchAction>,
    view: ViewFrame,
    target: InputTarget,
) -> std::result::Result<Vec<String>, BatchFailure> {
    execute_batch_with(actions, view, target, crate::platform::input()).await
}

pub(crate) async fn execute_batch_with(
    actions: Vec<BatchAction>,
    view: ViewFrame,
    target: InputTarget,
    input: &dyn crate::platform::InputInjector,
) -> std::result::Result<Vec<String>, BatchFailure> {
    let count = actions.len();
    if count > MAX_BATCH_ACTIONS {
        return Err(BatchFailure {
            completed: Vec::new(),
            failed_index: None,
            reason: format!("batch exceeds {MAX_BATCH_ACTIONS} actions; no actions ran"),
            not_executed: NotExecuted {
                start: 0,
                end_exclusive: count,
            },
        });
    }

    let mut results = Vec::with_capacity(count);
    for (index, action) in actions.into_iter().enumerate() {
        match execute_action(action, view, target, input).await {
            Ok(result) => results.push(bounded_detail(&result)),
            Err(error) => {
                return Err(BatchFailure {
                    completed: results
                        .into_iter()
                        .enumerate()
                        .map(|(index, result)| CompletedAction { index, result })
                        .collect(),
                    failed_index: Some(index),
                    reason: bounded_detail(&error.to_string()),
                    not_executed: NotExecuted {
                        start: index + 1,
                        end_exclusive: count,
                    },
                });
            }
        }
    }
    Ok(results)
}

/// Keep both the native route prefix and the trailing typed chars/bytes metadata.
fn bounded_detail(detail: &str) -> String {
    let count = detail.chars().count();
    if count <= MAX_BATCH_DETAIL_CHARS {
        return detail.to_owned();
    }
    let head = MAX_BATCH_DETAIL_CHARS / 2;
    let tail = MAX_BATCH_DETAIL_CHARS - head - 1;
    let mut result: String = detail.chars().take(head).collect();
    result.push('…');
    result.extend(detail.chars().skip(count - tail));
    result
}

async fn execute_action(
    action: BatchAction,
    view: ViewFrame,
    target: InputTarget,
    input: &dyn crate::platform::InputInjector,
) -> Result<String> {
    match action {
        BatchAction::MouseMove { x, y } => {
            let (lx, ly) = view.to_logical(x, y);
            input.mouse_move(lx, ly)?;
            Ok(format!("moved to ({x}, {y})"))
        }
        BatchAction::LeftClick { x, y } => {
            let (lx, ly) = view.to_logical(x, y);
            input.left_click_at(lx, ly, target)?;
            Ok(format!("left clicked at ({x}, {y})"))
        }
        BatchAction::RightClick { x, y } => {
            let (lx, ly) = view.to_logical(x, y);
            input.right_click_at(lx, ly, target)?;
            Ok(format!("right clicked at ({x}, {y})"))
        }
        BatchAction::DoubleClick { x, y } => {
            let (lx, ly) = view.to_logical(x, y);
            input.double_click_at(lx, ly, target)?;
            Ok(format!("double clicked at ({x}, {y})"))
        }
        BatchAction::Scroll { lines } => {
            // Scroll at the view's center when no explicit position is given.
            let (cx, cy) = view.to_logical(view.screenshot.0 / 2.0, view.screenshot.1 / 2.0);
            input.scroll_at(cx, cy, lines, target)?;
            Ok(format!("scrolled {lines} lines"))
        }
        BatchAction::KeyCombo { key } => {
            input.key_combo(&key, target)?;
            Ok(format!("pressed {key}"))
        }
        BatchAction::TypeText { text } => crate::tools::input::type_text_with(input, &text, target),
        BatchAction::Wait { ms } => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(format!("waited {ms}ms"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NovaError;
    use crate::platform::InputInjector;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeInput {
        calls: Mutex<Vec<&'static str>>,
        fail_at: Option<usize>,
        reason: String,
    }

    impl FakeInput {
        fn record(&self, action: &'static str) -> Result<()> {
            let mut calls = self.calls.lock().unwrap();
            calls.push(action);
            if self.fail_at == Some(calls.len() - 1) {
                Err(NovaError::Input(self.reason.clone()))
            } else {
                Ok(())
            }
        }
    }

    impl InputInjector for FakeInput {
        fn mouse_move(&self, _: f64, _: f64) -> Result<()> {
            self.record("mouse_move")
        }
        fn cursor_position(&self) -> Result<(f64, f64)> {
            unreachable!("batch does not read the cursor")
        }
        fn left_click_at(&self, _: f64, _: f64, _: InputTarget) -> Result<()> {
            self.record("left_click")
        }
        fn right_click_at(&self, _: f64, _: f64, _: InputTarget) -> Result<()> {
            self.record("right_click")
        }
        fn double_click_at(&self, _: f64, _: f64, _: InputTarget) -> Result<()> {
            self.record("double_click")
        }
        fn scroll_at(&self, _: f64, _: f64, _: i32, _: InputTarget) -> Result<()> {
            self.record("scroll")
        }
        fn key_combo(&self, _: &str, _: InputTarget) -> Result<()> {
            self.record("key_combo")
        }
        fn type_text(&self, _: &str, _: InputTarget) -> Result<()> {
            self.record("type_text")
        }
    }

    fn view() -> ViewFrame {
        ViewFrame {
            origin: (0.0, 0.0),
            region: (10.0, 10.0),
            screenshot: (10.0, 10.0),
        }
    }

    fn three_actions() -> Vec<BatchAction> {
        vec![
            BatchAction::LeftClick { x: 1.0, y: 2.0 },
            BatchAction::KeyCombo {
                key: "return".into(),
            },
            BatchAction::TypeText {
                text: "pāss🔐word".into(),
            },
        ]
    }

    #[test]
    fn deserializes_tagged_actions() {
        let json = r#"[
            {"action":"mouse_move","x":1.0,"y":2.0},
            {"action":"left_click","x":3.0,"y":4.0},
            {"action":"scroll","lines":-5},
            {"action":"key_combo","key":"cmd+c"},
            {"action":"type_text","text":"hi"},
            {"action":"wait","ms":100}
        ]"#;
        let actions: Vec<BatchAction> = serde_json::from_str(json).unwrap();
        assert_eq!(actions.len(), 6);
        assert!(matches!(actions[0], BatchAction::MouseMove { x, y } if x == 1.0 && y == 2.0));
        assert!(matches!(actions[2], BatchAction::Scroll { lines: -5 }));
        assert!(matches!(actions[5], BatchAction::Wait { ms: 100 }));
    }

    #[test]
    fn unknown_action_tag_is_rejected() {
        let json = r#"[{"action":"frobnicate"}]"#;
        assert!(serde_json::from_str::<Vec<BatchAction>>(json).is_err());
    }

    #[tokio::test]
    async fn first_failure_reports_no_completed_actions_and_unattempted_range() {
        let input = FakeInput {
            fail_at: Some(0),
            reason: "route=fake click failed".into(),
            ..FakeInput::default()
        };
        let failure = execute_batch_with(three_actions(), view(), InputTarget::Global, &input)
            .await
            .unwrap_err();
        assert_eq!(
            serde_json::to_value(failure).unwrap(),
            serde_json::json!({
                "completed": [],
                "failed_index": 0,
                "reason": "input event failed: route=fake click failed",
                "not_executed": {"start": 1, "end_exclusive": 3}
            })
        );
        assert_eq!(*input.calls.lock().unwrap(), ["left_click"]);
    }

    #[tokio::test]
    async fn middle_failure_keeps_completed_irreversible_click_and_stops_dispatch() {
        let input = FakeInput {
            fail_at: Some(1),
            reason: "route=fake key failed".into(),
            ..FakeInput::default()
        };
        let failure = execute_batch_with(three_actions(), view(), InputTarget::Global, &input)
            .await
            .unwrap_err();
        assert_eq!(
            serde_json::to_value(failure).unwrap(),
            serde_json::json!({
                "completed": [{"index": 0, "result": "left clicked at (1, 2)"}],
                "failed_index": 1,
                "reason": "input event failed: route=fake key failed",
                "not_executed": {"start": 2, "end_exclusive": 3}
            })
        );
        // Each recorded call models an irreversible event, including the failed
        // attempt. The click is neither repeated nor undone; text is not sent.
        assert_eq!(*input.calls.lock().unwrap(), ["left_click", "key_combo"]);
    }

    #[tokio::test]
    async fn successful_and_empty_batches_preserve_status_lines() {
        let input = FakeInput::default();
        let results = execute_batch_with(three_actions(), view(), InputTarget::Global, &input)
            .await
            .unwrap();
        assert_eq!(
            results,
            [
                "left clicked at (1, 2)",
                "pressed return",
                "typed input (chars=9, bytes=13)"
            ]
        );
        assert_eq!(
            *input.calls.lock().unwrap(),
            ["left_click", "key_combo", "type_text"]
        );
        assert!(
            execute_batch_with(Vec::new(), view(), InputTarget::Global, &input)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(input.calls.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn oversized_batch_is_rejected_before_any_input_and_has_no_failed_step() {
        let input = FakeInput::default();
        let actions = vec![BatchAction::LeftClick { x: 1.0, y: 2.0 }; MAX_BATCH_ACTIONS + 1];
        let failure = execute_batch_with(actions, view(), InputTarget::Global, &input)
            .await
            .unwrap_err();
        assert!(failure.completed.is_empty());
        assert_eq!(failure.failed_index, None);
        assert!(failure.reason.contains("no actions ran"));
        assert_eq!(failure.not_executed.start, 0);
        assert_eq!(failure.not_executed.end_exclusive, MAX_BATCH_ACTIONS + 1);
        assert!(input.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn maximum_progress_bounds_unicode_details_and_preserves_typed_metadata() {
        let secret = "pāss🔐word";
        let long = "\0界🔐".repeat(MAX_BATCH_DETAIL_CHARS);
        let input = FakeInput {
            fail_at: Some(MAX_BATCH_ACTIONS - 1),
            reason: format!("route=fake {long} value={secret:?}"),
            ..FakeInput::default()
        };
        let mut actions = vec![BatchAction::KeyCombo { key: long }; MAX_BATCH_ACTIONS - 1];
        actions.push(BatchAction::TypeText {
            text: secret.into(),
        });
        let failure = execute_batch_with(actions, view(), InputTarget::Global, &input)
            .await
            .unwrap_err();
        assert_eq!(failure.completed.len(), MAX_BATCH_ACTIONS - 1);
        for (index, completed) in failure.completed.iter().enumerate() {
            assert_eq!(completed.index, index);
            assert_eq!(completed.result.chars().count(), MAX_BATCH_DETAIL_CHARS);
            assert!(completed.result.starts_with("pressed "));
            assert!(completed.result.contains('…'));
        }
        assert_eq!(failure.failed_index, Some(MAX_BATCH_ACTIONS - 1));
        assert_eq!(failure.not_executed.start, MAX_BATCH_ACTIONS);
        assert_eq!(failure.not_executed.end_exclusive, MAX_BATCH_ACTIONS);
        assert_eq!(failure.reason.chars().count(), MAX_BATCH_DETAIL_CHARS);
        assert!(failure.reason.contains("route=fake"));
        assert!(failure.reason.ends_with("(chars=9, bytes=13)"));
        let json = serde_json::to_string(&failure).unwrap();
        assert!(!json.contains(secret));
        // A JSON-escaped Unicode scalar uses at most six bytes. Account for
        // each bounded detail and its small index/field-name overhead.
        assert!(json.len() <= MAX_BATCH_ACTIONS * (MAX_BATCH_DETAIL_CHARS * 6 + 128) + 512);
        assert_eq!(input.calls.lock().unwrap().len(), MAX_BATCH_ACTIONS);
    }

    #[tokio::test]
    async fn wait_only_batch_executes_without_touching_input_apis() {
        // Hermetic: `wait` posts no system events, so this exercises the
        // dispatch/aggregation path without moving the real mouse/keyboard.
        let view = ViewFrame {
            origin: (0.0, 0.0),
            region: (1280.0, 720.0),
            screenshot: (1280.0, 720.0),
        };
        let out = execute_batch(
            vec![BatchAction::Wait { ms: 1 }, BatchAction::Wait { ms: 1 }],
            view,
            InputTarget::Global,
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            vec!["waited 1ms".to_string(), "waited 1ms".to_string()]
        );
    }
}
