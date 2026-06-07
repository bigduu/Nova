/// Batch execution — execute a sequence of input actions in one MCP call.
///
/// Reduces round-trips for deterministic multi-step interactions (e.g. click a
/// field, type, press return). Screenshots are intentionally *not* part of a
/// batch: they return image content rather than a status string, so an agent
/// takes a screenshot with the dedicated `screenshot` tool after a batch runs.
use crate::error::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

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
/// Returns a status line for each action that ran.
pub async fn execute_batch(actions: Vec<BatchAction>) -> Result<Vec<String>> {
    let mut results = Vec::with_capacity(actions.len());
    for action in actions {
        results.push(execute_action(action).await?);
    }
    Ok(results)
}

async fn execute_action(action: BatchAction) -> Result<String> {
    use crate::display::geometry::screen_to_logical_coords;
    use crate::tools::input;

    match action {
        BatchAction::MouseMove { x, y } => {
            let (lx, ly) = screen_to_logical_coords(x, y);
            input::mouse_move(lx, ly)?;
            Ok(format!("moved to ({x}, {y})"))
        }
        BatchAction::LeftClick { x, y } => {
            let (lx, ly) = screen_to_logical_coords(x, y);
            input::left_click_at(lx, ly)?;
            Ok(format!("left clicked at ({x}, {y})"))
        }
        BatchAction::RightClick { x, y } => {
            let (lx, ly) = screen_to_logical_coords(x, y);
            input::right_click_at(lx, ly)?;
            Ok(format!("right clicked at ({x}, {y})"))
        }
        BatchAction::DoubleClick { x, y } => {
            let (lx, ly) = screen_to_logical_coords(x, y);
            input::mouse_move(lx, ly)?;
            std::thread::sleep(Duration::from_millis(10));
            input::double_click()?;
            Ok(format!("double clicked at ({x}, {y})"))
        }
        BatchAction::Scroll { lines } => {
            input::scroll(lines)?;
            Ok(format!("scrolled {lines} lines"))
        }
        BatchAction::KeyCombo { key } => {
            input::key_combo(&key)?;
            Ok(format!("pressed {key}"))
        }
        BatchAction::TypeText { text } => {
            input::type_text(&text)?;
            Ok(format!("typed {text:?}"))
        }
        BatchAction::Wait { ms } => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(format!("waited {ms}ms"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn wait_only_batch_executes_without_touching_input_apis() {
        // Hermetic: `wait` posts no system events, so this exercises the
        // dispatch/aggregation path without moving the real mouse/keyboard.
        let out = execute_batch(vec![
            BatchAction::Wait { ms: 1 },
            BatchAction::Wait { ms: 1 },
        ])
        .await
        .unwrap();
        assert_eq!(
            out,
            vec!["waited 1ms".to_string(), "waited 1ms".to_string()]
        );
    }
}
