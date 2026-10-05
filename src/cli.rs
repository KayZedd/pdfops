//! Command line interface. Every command prints one JSON document on stdout;
//! failures print `{"error": ...}` on stderr and exit with status 1.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::ops::{
    annotate, assemble, create, edit, forms, layout, ocr, read, redact, render, sign,
};
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
    /// Memory a command may hold, in MiB; 0 for no limit
    #[arg(long, global = true, env = "PDFOPS_MAX_MEMORY", default_value_t = 4096)]
    max_memory: usize,
    /// Seconds a command may run; 0 for no limit
    #[arg(long, global = true, env = "PDFOPS_TIMEOUT", default_value_t = 300)]
    timeout: u64,
}

#[derive(Subcommand)]
enum Command {
    /// Page count, page size, metadata, encryption, outline and form summary of a PDF
    Info(read::InfoArgs),
    /// Extract text page by page, with an optional character budget
    Text(read::TextArgs),
    /// Find text or a regular expression and return matching pages with context
    Search(read::SearchArgs),
    /// Text with positions: bounding box, font and size of every line or word
    Layout(layout::LayoutArgs),
    /// Extract tables as rows of cells, Markdown or CSV
    Tables(layout::TablesArgs),
    /// Recognise text on scanned pages with OCR (needs the tesseract program)
    Ocr(ocr::OcrArgs),
    /// Show whether tesseract is installed and which OCR languages can be used
    OcrLangs(ocr::OcrLangsArgs),
    /// Download OCR language data (no administrator rights needed), optionally install tesseract itself
    OcrInstall(ocr::OcrInstallArgs),
    /// List the outline (bookmarks / table of contents) with target pages
    Outline(read::OutlineArgs),
    /// List annotations: highlights, comments, links and other markup, with their positions
    Annotations(annotate::AnnotationsArgs),
    /// Render pages to PNG files, e.g. to look at scans, charts or layout
    Render(render::RenderArgs),
    /// Extract the images drawn on pages to files (JPEG and JPEG 2000 as stored, the rest as PNG)
    Images(render::ImagesArgs),
    /// Create a PDF from Markdown: headings, lists, tables, code, links and images
    Create(create::CreateArgs),
    /// Concatenate several PDFs into one
    Merge(assemble::MergeArgs),
    /// Keep, reorder, duplicate or delete pages
    Pages(assemble::PagesArgs),
    /// Split a PDF into several files, by page count or by page ranges
    Split(assemble::SplitArgs),
    /// Rotate pages by a multiple of 90 degrees
    Rotate(edit::RotateArgs),
    /// Draw text (watermark, header, footer, page numbers), an image such as a signature, or a QR code
    Stamp(edit::StampArgs),
    /// Add a highlight, underline, strike-out, box, note or link, on found text or on an area
    Annotate(annotate::AnnotateArgs),
    /// Permanently remove text, images and drawings in given areas or matching given text, then verify
    Redact(redact::RedactArgs),
    /// Replace text in place, written in the document's own font where it has the glyphs
    Replace(redact::ReplaceArgs),
    /// Set title, author, subject, keywords or creator
    SetMeta(edit::SetMetaArgs),
    /// Shrink a PDF: lossless by default, optionally re-encoding and downscaling images
    Compress(edit::CompressArgs),
    /// Protect a PDF with AES-256 passwords and permissions
    Encrypt(edit::EncryptArgs),
    /// Remove password protection, given the password
    Decrypt(edit::DecryptArgs),
    /// Sign a PDF digitally with a certificate and its private key
    Sign(sign::SignArgs),
    /// Check the digital signatures of a PDF: who signed, and whether it changed since
    Signatures(sign::SignaturesArgs),
    /// List form fields with their types, current values and options
    Forms(forms::FormsArgs),
    /// Fill form fields by name
    Fill(forms::FillArgs),
    /// Run a Model Context Protocol server on stdio exposing every command as a tool
    Mcp {
        /// Confine all file access to this directory; relative paths resolve against it
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// Run one tool with JSON arguments from stdin; the MCP server runs each call this way
    #[command(hide = true)]
    Call {
        /// Tool name, e.g. pdf_text
        name: String,
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// Print JSON tool definitions (name, description, input schema) for function calling
    Tools,
}

fn run(command: Command, limits: (usize, u64)) -> Result<Option<Value>> {
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
        Command::Layout(a) => layout::layout(a)?,
        Command::Tables(a) => layout::tables(a)?,
        Command::Ocr(a) => ocr::ocr(a)?,
        Command::OcrLangs(a) => ocr::ocr_langs(a)?,
        Command::OcrInstall(a) => ocr::ocr_install(a)?,
        Command::Outline(a) => read::outline(a)?,
        Command::Annotations(a) => annotate::annotations(a)?,
        Command::Render(a) => render::render(a)?,
        Command::Images(a) => render::images(a)?,
        Command::Create(a) => create::create(a)?,
        Command::Merge(a) => assemble::merge(a)?,
        Command::Pages(a) => assemble::pages(a)?,
        Command::Split(a) => assemble::split(a)?,
        Command::Rotate(a) => edit::rotate(a)?,
        Command::Stamp(a) => edit::stamp(a)?,
        Command::Annotate(a) => annotate::annotate(a)?,
        Command::Redact(a) => redact::redact(a)?,
        Command::Replace(a) => redact::replace(a)?,
        Command::SetMeta(a) => edit::set_meta(a)?,
        Command::Compress(a) => edit::compress(a)?,
        Command::Encrypt(a) => edit::encrypt(a)?,
        Command::Decrypt(a) => edit::decrypt(a)?,
        Command::Sign(a) => sign::sign(a)?,
        Command::Signatures(a) => sign::signatures(a)?,
        Command::Forms(a) => forms::forms(a)?,
        Command::Fill(a) => forms::fill(a)?,
        Command::Mcp { root } => {
            if let Some(root) = &root {
                crate::sandbox::set_root(root)?;
            }
            mcp::serve(root.as_deref(), limits.0, limits.1)?;
            return Ok(None);
        }
        Command::Call { name, root } => {
            if let Some(root) = &root {
                crate::sandbox::set_root(root)?;
            }
            let args = serde_json::from_reader(std::io::stdin().lock())?;
            tools::call(&name, args)?
        }
        Command::Tools => Value::Array(tools::definitions()),
    }))
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    // Parser panics are reported as JSON errors below, not as backtraces.
    std::panic::set_hook(Box::new(|_| {}));
    let pretty = cli.pretty;
    let limits = (cli.max_memory, cli.timeout);
    // The server itself is long-lived and small; its limits apply to each call it starts.
    if !matches!(cli.command, Command::Mcp { .. }) {
        crate::limits::set_max_memory(limits.0);
        crate::limits::set_timeout(limits.1);
    }
    let outcome = std::panic::catch_unwind(|| run(cli.command, limits)).unwrap_or_else(|_| {
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
