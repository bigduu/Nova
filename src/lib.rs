//! Nova — Computer Use MCP Server
//!
//! A native macOS/Windows desktop control MCP server that gives LLMs the ability to:
//! - Read and activate semantic Accessibility/UIA nodes without screenshots
//! - Capture screenshots of windows and the display
//! - Control mouse and keyboard through native input APIs
//! - List and manage application windows
//! - Read/write clipboard
//!
//! The canonical grounding ladder is `ax_read` → focused OCR → focused pixel
//! capture, with screenshots reserved for genuinely visual information.

pub mod app_inspection;
pub mod app_service;
pub mod app_status;
pub mod capture;
pub mod chrome_devtools;
pub mod display;
pub mod error;
pub mod platform;
pub mod server;
pub mod tools;
pub mod types;
