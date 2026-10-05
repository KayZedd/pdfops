//! Read-only commands: info, text, search, outline.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::object::{Array, Dict};
use lopdf::{Object, StringFormat};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::ops::{layout, ocr};
use crate::{doc, pagespec};

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct InfoArgs {
    /// PDF file to inspect
    pub input: PathBuf,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct TextArgs {
    /// PDF file to read
    pub input: PathBuf,
    /// Pages to read, e.g. "1-3,7,10-" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// Stop after this many characters in total; the result says where to resume
    #[arg(long)]
    pub max_chars: Option<usize>,
    /// Run OCR on pages that have no extractable text, such as scans (needs tesseract)
    #[arg(long)]
    #[serde(default)]
    pub ocr: bool,
    /// Tesseract language codes for OCR, e.g. "pol+eng" (default: eng)
    #[arg(long)]
    pub ocr_lang: Option<String>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
    /// Print plain text instead of JSON, pages separated by form feeds
    #[arg(long)]
    #[serde(skip)]
    #[schemars(skip)]
    pub raw: bool,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    /// PDF file to search
    pub input: PathBuf,
    /// Text to find, or a regular expression when regex is enabled
    pub query: String,
    /// Treat the query as a regular expression
    #[arg(long)]
    #[serde(default)]
    pub regex: bool,
    /// Match case exactly (default: case-insensitive)
    #[arg(long)]
    #[serde(default)]
    pub case_sensitive: bool,
    /// Pages to search, e.g. "1-20" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// Characters of context on each side of a match (default: 80)
    #[arg(long)]
    pub context: Option<usize>,
    /// Maximum matches to return (default: 50)
    #[arg(long)]
    pub max_results: Option<usize>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct OutlineArgs {
    /// PDF file to inspect
    pub input: PathBuf,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

/// Counts outline items by walking the First/Next links.
fn count_outline(catalog: &Dict<'_>) -> usize {
    let mut seen = HashSet::new();
    let mut open: Vec<Dict<'_>> = Vec::new();
    open.extend(
        catalog
            .get::<Dict<'_>>(b"Outlines")
            .and_then(|o| o.get::<Dict<'_>>(b"First")),
    );
    let mut count = 0;
    while let Some(item) = open.pop() {
        // `seen` stops a cyclic chain; the cap covers items stored without an object id.
        if item.obj_id().is_some_and(|id| !seen.insert(id)) || count >= 1_000_000 {
            continue;
        }
        count += 1;
        open.extend(item.get::<Dict<'_>>(b"Next"));
        open.extend(item.get::<Dict<'_>>(b"First"));
    }
    count
}

/// Counts terminal form fields below `field`, the same way `forms` lists them.
fn count_fields(field: &Dict<'_>, depth: u32) -> usize {
    let kids: Vec<Dict<'_>> = field
        .get::<Array<'_>>(b"Kids")
        .map(|k| k.iter::<Dict<'_>>().collect())
        .unwrap_or_default();
    if depth >= 32 || !kids.iter().any(|k| k.contains_key(b"T")) {
        return 1;
    }
    kids.iter().map(|k| count_fields(k, depth + 1)).sum()
}

pub fn info(a: InfoArgs) -> Result<Value> {
    let (pdf, encrypted) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let sizes: Vec<(f32, f32)> = pdf.pages().iter().map(|p| p.render_dimensions()).collect();
    let uniform = sizes.windows(2).all(|w| w[0] == w[1]);

    let meta = pdf.metadata();
    let mut metadata = serde_json::Map::new();
    let texts = [
        ("title", &meta.title),
        ("author", &meta.author),
        ("subject", &meta.subject),
        ("keywords", &meta.keywords),
        ("creator", &meta.creator),
        ("producer", &meta.producer),
    ];
    for (key, value) in texts {
        let text = value
            .as_ref()
            .and_then(|b| doc::text(&Object::String(b.clone(), StringFormat::Literal)));
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            metadata.insert(key.to_string(), json!(text));
        }
    }
    for (key, date) in [
        ("created", meta.creation_date),
        ("modified", meta.modification_date),
    ] {
        if let Some(d) = date {
            let sign = if d.utc_offset_hour < 0 { '-' } else { '+' };
            let iso = format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
                d.year,
                d.month,
                d.day,
                d.hour,
                d.minute,
                d.second,
                d.utc_offset_hour.unsigned_abs(),
                d.utc_offset_minute
            );
            metadata.insert(key.to_string(), json!(iso));
        }
    }

    let xref = pdf.xref();
    let catalog: Option<Dict<'_>> = xref.get(xref.root_id());
    let fields = catalog
        .as_ref()
        .and_then(|c| c.get::<Dict<'_>>(b"AcroForm"))
        .and_then(|f| f.get::<Array<'_>>(b"Fields"))
        .map(|f| {
            f.iter::<Dict<'_>>()
                .map(|d| count_fields(&d, 0))
                .sum::<usize>()
        });
    let digits: String = format!("{:?}", pdf.version())
        .chars()
        .filter(char::is_ascii_digit)
        .collect();

    Ok(json!({
        "file": a.input,
        "size_bytes": pdf.data().as_ref().len(),
        "pdf_version": format!("{}.{}", &digits[..1.min(digits.len())], &digits[1.min(digits.len())..]),
        "pages": sizes.len(),
        "encrypted": encrypted,
        "page_size_pt": sizes.first().map(|s| json!({"width": s.0, "height": s.1})),
        "uniform_page_size": uniform,
        "metadata": metadata,
        "outline_entries": catalog.as_ref().map_or(0, count_outline),
        "form_fields": fields.unwrap_or(0),
    }))
}

/// Collapses runs of spaces and blank lines left over from the page layout.
///
/// They carry no meaning in extracted text and cost an agent tokens.
pub fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        blank = if line.is_empty() { blank + 1 } else { 0 };
        // Keep one blank line as a paragraph break; drop leading and repeated ones.
        if blank > 1 || (blank == 1 && out.is_empty()) {
            continue;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out
}

/// Extracts text for each requested page, in parallel.
///
/// A page that cannot be read yields `Err` instead of failing the whole call,
/// because one malformed content stream should not hide the rest of the document.
pub fn page_texts(pdf: &Pdf, pages: &[u32]) -> Vec<(u32, Result<String, String>)> {
    let all = pdf.pages();
    pages
        .par_iter()
        .map(|&n| {
            // Malformed fonts and streams are the renderer's to survive; a panic stays on its page.
            let res = catch_unwind(AssertUnwindSafe(|| {
                tidy(&layout::plain_text(
                    &layout::scan(&all[n as usize - 1]).words,
                ))
            }))
            .map_err(|_| "text extraction failed on this page".to_string());
            (n, res)
        })
        .collect()
}

pub fn text(a: TextArgs) -> Result<Value> {
    let (pdf, _) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let total = pdf.pages().len() as u32;
    let pages = pagespec::parse_or_all(a.pages.as_deref(), total)?;

    let mut budget = a.max_chars.unwrap_or(usize::MAX);
    let mut out = Vec::new();
    let mut resume = None;
    let mut chars_total = 0usize;
    let mut texts = page_texts(&pdf, &pages);
    let mut recognised = HashSet::new();
    let blank: Vec<u32> = texts
        .iter()
        .filter(|(_, t)| t.as_ref().is_ok_and(|t| t.is_empty()))
        .map(|p| p.0)
        .collect();
    if a.ocr && !blank.is_empty() {
        let read = ocr::recognise(&pdf, &blank, a.ocr_lang.as_deref().unwrap_or("eng"), 300.0)?;
        for (n, text) in read {
            recognised.insert(n);
            for slot in texts.iter_mut().filter(|t| t.0 == n) {
                slot.1 = text.clone().map(|t| tidy(&t));
            }
        }
    }
    for (n, res) in texts {
        if budget == 0 {
            resume = Some(n);
            break;
        }
        match res {
            Ok(t) => {
                let t = t.trim();
                let chars = t.chars().count();
                let cut = chars > budget;
                let shown: String = if cut {
                    t.chars().take(budget).collect()
                } else {
                    t.to_string()
                };
                chars_total += chars.min(budget);
                budget -= chars.min(budget);
                let mut page = json!({"page": n, "text": shown, "truncated": cut});
                if recognised.contains(&n) {
                    page["ocr"] = json!(true);
                }
                out.push(page);
                if cut {
                    // The rest of this page was dropped, so it is where reading resumes.
                    resume = Some(n);
                    break;
                }
            }
            Err(e) => out.push(json!({"page": n, "error": e})),
        }
    }
    Ok(json!({
        "file": a.input,
        "total_pages": total,
        "chars": chars_total,
        "pages": out,
        "resume_at_page": resume,
    }))
}

/// Plain-text rendering of a `text` result, for `--raw`.
pub fn raw_text(v: &Value) -> String {
    let pages = v["pages"].as_array().map(Vec::as_slice).unwrap_or_default();
    pages
        .iter()
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\u{c}\n")
}

pub fn search(a: SearchArgs) -> Result<Value> {
    if a.query.is_empty() {
        bail!("query is empty");
    }
    let pattern = if a.regex {
        a.query.clone()
    } else {
        regex::escape(&a.query)
    };
    let re = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!a.case_sensitive)
        .build()?;
    let (pdf, _) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let total = pdf.pages().len() as u32;
    let pages = pagespec::parse_or_all(a.pages.as_deref(), total)?;
    let (context, limit) = (a.context.unwrap_or(80), a.max_results.unwrap_or(50));

    let mut matches = Vec::new();
    let mut count = 0usize;
    let mut failed = Vec::new();
    for (n, res) in page_texts(&pdf, &pages) {
        let Ok(raw) = res else {
            failed.push(n);
            continue;
        };
        // Line breaks in extracted text are layout artefacts, so a phrase must match across them.
        let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        for m in re.find_iter(&flat) {
            count += 1;
            if matches.len() < limit {
                let start = floor_boundary(&flat, m.start().saturating_sub(context));
                let end = ceil_boundary(&flat, (m.end() + context).min(flat.len()));
                matches.push(json!({"page": n, "match": m.as_str(), "snippet": &flat[start..end]}));
            }
        }
    }
    Ok(json!({
        "file": a.input,
        "query": a.query,
        "total_matches": count,
        "matches": matches,
        "unreadable_pages": failed,
    }))
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

pub fn outline(a: OutlineArgs) -> Result<Value> {
    // Listing does not write, so a file too damaged to rewrite is still worth reading.
    let (d, _) = doc::read(&a.input, a.password.as_deref())?;
    let numbers: HashMap<lopdf::ObjectId, u32> =
        d.get_pages().into_iter().map(|(n, id)| (id, n)).collect();
    let entries: Vec<Value> = doc::outline(&d)
        .into_iter()
        .map(|e| json!({"level": e.level, "title": e.title, "page": e.page.and_then(|p| numbers.get(&p))}))
        .collect();
    Ok(json!({"file": a.input, "entries": entries}))
}

#[cfg(test)]
mod tests {
    use super::tidy;

    #[test]
    fn tidy_collapses_layout_whitespace() {
        assert_eq!(
            tidy("\n\n a  b\t c \n\n\n\nnext   line\n\n"),
            "a b c\n\nnext line"
        );
        assert_eq!(tidy("   \n "), "");
        assert_eq!(tidy("one"), "one");
    }
}
