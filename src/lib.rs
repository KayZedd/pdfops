//! Fast PDF operations with JSON results, built for AI agents.
//!
//! The same commands are exposed three ways: the `pdfops` CLI, an MCP server
//! (`pdfops mcp`) and this library. See [`tools::TOOLS`] for the full list.

pub mod cli;
pub mod doc;
pub mod mcp;
pub mod ops;
pub mod pagespec;
pub mod tools;
