//! Command line interface. Every command prints one JSON document on stdout;
//! failures print `{"error": ...}` on stderr and exit with status 1.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::ops::{
    annotate, assemble, create, edit, forms, layout, ocr, read, redact, render, scan, sign,
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
    /// Print a line of JSON for every finished page or file while working; the result is the last line
    #[arg(long, global = true)]
    stream: bool,
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
    ///
    /// Text comes in the order the page draws it, which keeps columns and captions together. Lines that hold Hebrew or Arabic are turned back into the order they are read in. Letters stay in the form the file has them in: Arabic stored as presentation forms is not folded back to plain letters.
    ///
    /// With OCR, only pages without text of their own are recognised.
    Text(read::TextArgs),
    /// Find text or a regular expression and return matching pages with context
    Search(read::SearchArgs),
    /// Text with positions: bounding box, font and size of every line or word
    Layout(layout::LayoutArgs),
    /// Extract tables as rows of cells, Markdown or CSV
    ///
    /// Tables drawn with ruling lines are read cell by cell and are reliable.
    ///
    /// Tables without lines are inferred from column alignment and marked "detected_by": "alignment". Columns are the stretches of the page that rows fill, kept apart even where a heading lies across two of them. A label or a description that wraps is joined to its row. Rows with empty cells, a heading over a group of rows and a figure set alone under its column stay in the table. Running text between two tables splits them, and a list of contents with dot leaders is not a table. It is still inference: whether a line continues the row above is judged from indentation, spacing and capitals, so such tables deserve a look before trusting.
    Tables(layout::TablesArgs),
    /// Recognise text on scanned pages with OCR (needs the tesseract program)
    ///
    /// Quality and languages are tesseract's. When a language is missing, the error names the ocr-install call that fixes it.
    ///
    /// With an output, the recognised words are also written into a copy of the PDF as invisible text over the picture of each word, so the copy can be searched and selected in any viewer. Pages that have text of their own are left as they are.
    Ocr(ocr::OcrArgs),
    /// Show whether tesseract is installed and which OCR languages can be used
    OcrLangs(ocr::OcrLangsArgs),
    /// Download OCR language data (no administrator rights needed), optionally install tesseract itself
    ///
    /// Data comes from the tessdata_fast repository, or tessdata_best on request, and is kept in the user's data directory (~/.local/share/pdfops/tessdata on Linux), or in PDFOPS_TESSDATA if set. Installing tesseract itself uses the system package manager; where that needs a password, the command to run is returned instead.
    OcrInstall(ocr::OcrInstallArgs),
    /// List the outline (bookmarks / table of contents) with target pages
    Outline(read::OutlineArgs),
    /// List annotations: highlights, comments, links and other markup, with their positions
    Annotations(annotate::AnnotationsArgs),
    /// Inspect a PDF for scripts, automatic actions, attached files, disguised content and hidden text
    ///
    /// This inspects structure. It is not a virus scanner and never calls a file safe. The verdict is "nothing found" or "findings"; each finding has a severity (high, medium, low, info), the objects it sits in and samples, and the result lists what was checked and what was not. The raw bytes are read as well as the parsed objects, so a file that does not open, or hides a name behind #xx escapes or inside a packed object, is still judged. On request the file is also passed to clamscan when that is installed; no signature database is bundled.
    ///
    /// Hidden text is found by looking: every page is rendered, and a word that leaves no trace in the picture is reported with its page, box, reason and text, whether it is in the invisible text mode, in the colour of its background, under a flat shape, clipped away, smaller than 1.5 points or off the page. The judgement is withheld where the rendering cannot be relied on: under translucent or blended drawing, and for fonts that cannot be drawn. Rendering runs in a process of its own, so a file built to exhaust memory or time costs this one check and is reported as resource_exhaustion. Invisible text over visible content, which scanned pages with recognised text have, is reported apart as invisible_text_layer with severity info. Text under a picture that is not a flat colour is not detected, and neither is text inside annotations and form fields. The boxes can be passed to redact.
    Scan(scan::ScanArgs),
    /// Render pages to PNG files, e.g. to look at scans, charts or layout
    Render(render::RenderArgs),
    /// Extract the images drawn on pages to files (JPEG and JPEG 2000 as stored, the rest as PNG)
    ///
    /// The images are the ones a page actually draws, including images written into the page content and images inside forms. Flate, LZW, CCITT fax, JBIG2, palette images and masks are decoded to PNG.
    Images(render::ImagesArgs),
    /// Create a PDF from Markdown: headings, lists, tables, code, links and images
    ///
    /// Understood: headings, emphasis, links, nested lists, quotes, code blocks, tables, rules and local images. HTML inside the Markdown is ignored; there is no HTML or CSS engine.
    ///
    /// Text outside Latin-1 is drawn with subsets of installed fonts and shaped: Arabic is joined, Indic and Thai clusters are formed, Latin gets its ligatures and kerning. Where no one font has every character, several share the text. A line is laid out as a whole, so punctuation next to right-to-left words lands where the sentence goes on, and a paragraph that runs from the right is set against the right margin.
    Create(create::CreateArgs),
    /// Concatenate several PDFs into one
    ///
    /// Bookmarks whose target page is in the output are kept, each landing where it did on its page. Links between pages keep working, also where they go to a destination by name. Form fields of the second and later inputs are renamed doc2.<name>, doc3.<name>, so equal names do not share a value. An encrypted input keeps the output encrypted.
    Merge(assemble::MergeArgs),
    /// Keep, reorder, duplicate or delete pages
    ///
    /// Bookmarks and links that lead to pages in the output are kept, each landing where it did on its page.
    Pages(assemble::PagesArgs),
    /// Split a PDF into several files, by page count or by page ranges
    Split(assemble::SplitArgs),
    /// Rotate pages by a multiple of 90 degrees
    Rotate(edit::RotateArgs),
    /// Draw text (watermark, header, footer, page numbers), an image such as a signature, or a QR code
    ///
    /// Latin-1 text uses the built-in Helvetica and embeds nothing. Other text is drawn with a subset of the given font, or of installed fonts, and is shaped; what a given font lacks, installed ones draw.
    Stamp(edit::StampArgs),
    /// Add a highlight, underline, strike-out, box, note or link, on found text or on an area
    Annotate(annotate::AnnotateArgs),
    /// Permanently remove text, images and drawings in given areas or matching given text, then verify
    ///
    /// What is under the areas is deleted from the page content: glyphs, image pixels, drawings lying wholly inside an area, and annotations with the form values they show. Images are blanked whatever they are stored as (fax or JBIG2 scans, JPEG in any colour model, JPEG 2000, images written into the page content) and are stored without loss afterwards. Text around an area does not move.
    ///
    /// The result is read back by a second, independent interpreter, and nothing is written unless every area is empty. Where an image cannot be decoded, or text is drawn in a way that cannot be taken apart with certainty, the command fails instead of guessing.
    ///
    /// A text to redact is also taken out of the document information, the bookmark titles and the metadata stream, which goes as a whole if it holds the text; "beside_pages" in the result says what was done. Attached files that hold the text are named there and left alone: sanitize removes attachments. The accessibility structure tree, which can repeat page text, is removed.
    ///
    /// A dry run lists every area with the text that matched and the counts of glyphs, images, drawings and annotations that would go.
    Redact(redact::RedactArgs),
    /// Replace text in place, written in the document's own font where it has the glyphs
    ///
    /// The new text is written with the codes the document's own font already uses for those characters on that page, so style is kept exactly. If the font, usually a subset, lacks a needed glyph, other fonts write just those words at the same size, position and colour, and the result says so.
    ///
    /// What follows on the same line moves along by the difference in width. A line that would then run past its column, judged from the lines above and below, is drawn up to 8% closer together from the replacement on, and overflow_pt reports what is still over; without neighbouring lines the right margin is taken to equal the left one. Text does not move from one line to the next, so a much longer replacement needs a look: a dry run gives width_change_pt and overflow_pt per match beforehand. Text inside form fields and annotations is not edited.
    Replace(redact::ReplaceArgs),
    /// Remove scripts, automatic and risky actions, attachments, XFA and media, then verify by scanning
    ///
    /// Removed: JavaScript, actions that run by themselves or start programs, send form data or open other files, embedded files, XFA forms and media annotations; a group can be kept on request. Ordinary web links stay, and hidden text is not touched. The result is scanned before it is written, and nothing is written if any of it is still there. Like every rewrite, it invalidates digital signatures.
    Sanitize(scan::SanitizeArgs),
    /// Set title, author, subject, keywords or creator
    SetMeta(edit::SetMetaArgs),
    /// Shrink a PDF: lossless by default, optionally re-encoding and downscaling images
    ///
    /// Lossy compression is opt-in, by giving an image quality or a longest edge. It re-encodes 8-bit grey and RGB images as JPEG. Masks, palette images, CMYK and 1-bit scans are left untouched, and the output is never larger than the input.
    Compress(edit::CompressArgs),
    /// Protect a PDF with AES-256 passwords and permissions
    Encrypt(edit::EncryptArgs),
    /// Remove password protection, given the password
    Decrypt(edit::DecryptArgs),
    /// Sign a PDF digitally with a certificate and its private key
    ///
    /// Signing appends to the file, so signatures already present stay valid. Keys are RSA or ECDSA P-256, from PEM files or a PKCS #12 file. The signature is not shown unless a place for it is given; then a box with the signer's name, the date and the reason is drawn there. With the address of a timestamp authority, its signed statement of the time is embedded; signing fails if the authority does not answer. No long-term validation data is added.
    ///
    /// A damaged file is rebuilt before it is signed, so signatures it carried do not survive.
    Sign(sign::SignArgs),
    /// Check the digital signatures of a PDF: who signed, and whether it changed since
    ///
    /// Two things are established: the signed bytes are unchanged, and the signature was made by the embedded certificate's key. covers_whole_document is false when content was appended after signing, as a later signature legitimately does.
    ///
    /// Whether the certificate deserves trust is checked only against certificates you give: the signer's chain must lead to one of them, with every certificate in date when the document was signed. On request the revocation list each certificate names is downloaded and checked; a certificate that names none leaves that question open, and the result says so. A timestamp is reported with its time and whether it is about this signature; the authority's own signature on it is not checked.
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
            if raw && crate::progress::enabled() {
                anyhow::bail!("raw text cannot be streamed: every streamed line is JSON");
            }
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
        Command::Scan(a) => scan::scan(a)?,
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
        Command::Sanitize(a) => scan::sanitize(a)?,
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
            // Not a tool: the part of `scan` that it runs away from itself.
            if name == scan::HIDDEN_TEXT_CALL {
                return Ok(Some(scan::hidden_text_call(args)?));
            }
            tools::call(&name, args)?
        }
        Command::Tools => Value::Array(tools::definitions()),
    }))
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    // Parser panics are reported as JSON errors below, not as backtraces.
    std::panic::set_hook(Box::new(|_| {}));
    // Streamed output is one JSON document per line, so the result cannot be indented.
    let pretty = cli.pretty && !cli.stream;
    if cli.stream {
        crate::progress::enable();
    }
    let limits = (cli.max_memory, cli.timeout);
    // The server itself is long-lived and small; its limits apply to each call it starts.
    if !matches!(cli.command, Command::Mcp { .. }) {
        crate::limits::set_max_memory(limits.0);
        crate::limits::set_timeout(limits.1);
        scan::isolate_rendering();
    }
    let outcome = std::panic::catch_unwind(|| run(cli.command, limits)).unwrap_or_else(|_| {
        Err(anyhow::anyhow!(
            "internal error: the PDF could not be processed"
        ))
    });
    match outcome {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(mut value)) => {
            // One command per process, so every rebuilt file was an input of this one.
            tools::note_repairs(&mut value, |_| true);
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
