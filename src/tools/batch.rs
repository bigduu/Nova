/// Batch execution — execute a sequence of actions in one MCP call.
///
/// Reduces round-trips for complex multi-step operations.
use crate::error::Result;
use serde::{Deserialize, Serialize};

/// A single action in a batch sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum BatchAction {
    #[serde(rename = "screenshot")]
    Screenshot,
    #[serde(rename = "mouse_move")]
    MouseMove { x: f64, y: f64 },
    #[serde(rename = "left_click")]
    LeftClick,
    #[serde(rename = "right_click")]
    RightClick,
    #[serde(rename = "double_click")]
    DoubleClick,
    #[serde(rename = "scroll")]
    Scroll { lines: i32 },
    #[serde(rename = "key_combo")]
    KeyCombo { key: String },
    #[serde(rename = "type_text")]
    TypeText { text: String },
    #[serde(rename = "wait")]
    Wait { ms: u64 },
}

/// Execute a sequence of actions in order.
/// Returns results for each action.
pub async fn execute_batch(actions: Vec<BatchAction>) -> Result<Vec<String>> {
    let mut results = Vec::with_capacity(actions.len());
    for action in actions {
        let result = execute_action(action).await?;
        results.push(result);
    }
    Ok(results)
}

async fn execute_action(_action: BatchAction) -> Result<String> {
    // TODO: dispatch to individual tool implementations
    Err(crate::error::NovaError::Internal(
        "batch not yet implemented".into(),
    ))
}
