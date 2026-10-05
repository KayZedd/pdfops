//! One module per group of commands. Every command takes its argument struct
//! and returns a JSON value, so the CLI and the MCP server share one code path.

pub mod assemble;
pub mod create;
pub mod edit;
pub mod forms;
pub mod layout;
pub mod lossy;
pub mod ocr;
pub mod read;
pub mod redact;
pub mod render;
