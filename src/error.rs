use thiserror::Error;

/// Nova error types — 3-tier hierarchy for precise error reporting.
#[derive(Error, Debug)]
pub enum NovaError {
    #[error("screenshot failed: {0}")]
    Screenshot(String),

    #[error("input event failed: {0}")]
    Input(String),

    #[error("window operation failed: {0}")]
    Window(String),

    #[error("clipboard operation failed: {0}")]
    Clipboard(String),

    #[error("application operation failed: {0}")]
    Application(String),

    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, NovaError>;
