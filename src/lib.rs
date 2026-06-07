//! Nova — Computer Use MCP Server
//!
//! A macOS desktop control MCP server that gives LLMs the ability to:
//! - Capture screenshots of windows and the display
//! - Control mouse and keyboard via CGEvent
//! - List and manage application windows
//! - Read/write clipboard
//!
//! Built on Apple's ScreenCaptureKit, CoreGraphics, and Accessibility APIs.

pub mod error;
pub mod server;
pub mod tools;
pub mod types;
