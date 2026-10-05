//! Command line interface. Every command prints one JSON document on stdout;
//! failures print `{"error": ...}` on stderr and exit with status 1.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::ops::{assemble, edit, forms, read, render};
use crate::{mcp, tools};

#[derive(Parser)]
#[command(
    name = "pdfops",
    version,
    about = "Fast PDF operations with JSON output, built for AI agents"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Indent the JSON output
    #[arg(long, global = true)]
    pretty: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Page count, page size, metadata, encryption, outline and form summary of a PDF
    Info(read::InfoArgs),
    /// Extract text page by page, with an optional character budget
    Text(read::TextArgs),
    /// Find text or a regular expression and return matching pages with context
    Search(read::SearchArgs),
    /// List the outline (bookmarks / table of contents) with target pages
    Outline(read::OutlineArgs),
    /// Render pages to PNG files, e.g. to look at scans, charts or layout
    Render(render::RenderArgs),
    /// Extract embedded images to files
    Images(render::ImagesArgs),
    /// Concatenate several PDFs into one
    Merge(assemble::MergeArgs),
    /// Keep, reorder, duplicate or delete pages
    Pages(assemble::PagesArgs),
    /// Split a PDF into several files, by page count or by page ranges
    Split(assemble::SplitArgs),
    /// Rotate pages by a multiple of 90 degrees
    Rotate(edit::RotateArgs),
    /// Draw a text watermark, header or footer (supports page numbers)
    Stamp(edit::StampArgs),
    /// Set title, author, subject, keywords or creator
    SetMeta(edit::SetMetaArgs),
    /// Shrink a PDF losslessly by recompressing streams and dropping unused objects
    Compress(edit::CompressArgs),
    /// Protect a PDF with AES-256 passwords and permissions
    Encrypt(edit::EncryptArgs),
    /// Remove password protection, given the password
    Decrypt(edit::DecryptArgs),
    /// List form fields with their types, current values and options
    Forms(forms::FormsArgs),
    /// Fill form fields by name
    Fill(forms::FillArgs),
    /// Run a Model Context Protocol server on stdio exposing every command as a tool
    Mcp,
    /// Print JSON tool definitions (name, description, input schema) for function calling
    Tools,
}

fn run(command: Command) -> Result<Option<Value>> {
    Ok(Some(match command {
        Command::Info(a) => read::info(a)?,
        Command::Text(a) => {
            let raw = a.raw;
            let value = read::text(a)?;
            if raw {
                println!("{}", read::raw_text(&value));
                return Ok(None);
            }
            value
        }
        Command::Search(a) => read::search(a)?,
        Command::Outline(a) => read::outline(a)?,
        Command::Render(a) => render::render(a)?,
        Command::Images(a) => render::images(a)?,
        Command::Merge(a) => assemble::merge(a)?,
        Command::Pages(a) => assemble::pages(a)?,
        Command::Split(a) => assemble::split(a)?,
        Command::Rotate(a) => edit::rotate(a)?,
        Command::Stamp(a) => edit::stamp(a)?,
        Command::SetMeta(a) => edit::set_meta(a)?,
        Command::Compress(a) => edit::compress(a)?,
        Command::Encrypt(a) => edit::encrypt(a)?,
        Command::Decrypt(a) => edit::decrypt(a)?,
        Command::Forms(a) => forms::forms(a)?,
        Command::Fill(a) => forms::fill(a)?,
        Command::Mcp => {
            mcp::serve()?;
            return Ok(None);
        }
        Command::Tools => Value::Array(tools::definitions()),
    }))
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    // Parser panics are reported as JSON errors below, not as backtraces.
    std::panic::set_hook(Box::new(|_| {}));
    let pretty = cli.pretty;
    let outcome = std::panic::catch_unwind(|| run(cli.command)).unwrap_or_else(|_| {
        Err(anyhow::anyhow!(
            "internal error: the PDF could not be processed"
        ))
    });
    match outcome {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(value)) => {
            let text = if pretty {
                serde_json::to_string_pretty(&value)
            } else {
                serde_json::to_string(&value)
            };
            println!("{}", text.expect("JSON value serialises"));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", json!({"error": format!("{e:#}")}));
            ExitCode::FAILURE
        }
    }
}
