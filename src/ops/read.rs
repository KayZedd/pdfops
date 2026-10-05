//! Read-only commands: info, text, search, outline.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use lopdf::Document;
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{doc, ops::forms, pagespec};

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

pub fn info(a: InfoArgs) -> Result<Value> {
    let d = doc::load(&a.input, a.password.as_deref())?;
    let ids = doc::page_ids(&d);
    let size = |id| {
        let b = doc::page_box(&d, id);
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        if doc::rotation(&d, id) % 180 == 90 {
            (h, w)
        } else {
            (w, h)
        }
    };
    let sizes: Vec<(f64, f64)> = ids.iter().map(|&id| size(id)).collect();
    let uniform = sizes.windows(2).all(|w| w[0] == w[1]);

    let mut metadata = serde_json::Map::new();
    if let Some(info) = doc::info_dict(&d) {
        for (key, value) in info.iter() {
            if let Some(text) = doc::text(doc::resolve(&d, value)) {
                metadata.insert(String::from_utf8_lossy(key).to_lowercase(), json!(text));
            }
        }
    }

    Ok(json!({
        "file": a.input,
        "size_bytes": std::fs::metadata(&a.input)?.len(),
        "pdf_version": d.version,
        "pages": ids.len(),
        "encrypted": d.was_encrypted(),
        "page_size_pt": sizes.first().map(|s| json!({"width": s.0, "height": s.1})),
        "uniform_page_size": uniform,
        "metadata": metadata,
        "outline_entries": d.get_toc().map(|t| t.toc.len()).unwrap_or(0),
        "form_fields": forms::collect(&d).len(),
    }))
}

/// Collapses runs of spaces and blank lines left over from the page layout.
///
/// They carry no meaning in extracted text and cost an agent tokens.
fn tidy(text: &str) -> String {
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
/// A page that cannot be decoded yields `Err` instead of failing the whole call,
/// because one malformed content stream should not hide the rest of the document.
pub fn page_texts(d: &Document, pages: &[u32]) -> Vec<(u32, Result<String, String>)> {
    pages
        .par_iter()
        .map(|&n| {
            // pdf-extract panics on some malformed fonts; contain it to the page.
            let res = catch_unwind(AssertUnwindSafe(|| {
                let mut s = String::new();
                pdf_extract::output_doc_page(d, &mut pdf_extract::PlainTextOutput::new(&mut s), n)
                    .map_err(|e| e.to_string())?;
                Ok(tidy(&s))
            }))
            .unwrap_or_else(|_| Err("text extraction failed on this page".to_string()));
            (n, res)
        })
        .collect()
}

pub fn text(a: TextArgs) -> Result<Value> {
    let d = doc::load(&a.input, a.password.as_deref())?;
    let total = doc::page_ids(&d).len() as u32;
    let pages = pagespec::parse_or_all(a.pages.as_deref(), total)?;

    let mut budget = a.max_chars.unwrap_or(usize::MAX);
    let mut out = Vec::new();
    let mut resume = None;
    let mut chars_total = 0usize;
    for (n, res) in page_texts(&d, &pages) {
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
                out.push(json!({"page": n, "text": shown, "truncated": cut}));
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
    let d = doc::load(&a.input, a.password.as_deref())?;
    let total = doc::page_ids(&d).len() as u32;
    let pages = pagespec::parse_or_all(a.pages.as_deref(), total)?;
    let (context, limit) = (a.context.unwrap_or(80), a.max_results.unwrap_or(50));

    let mut matches = Vec::new();
    let mut count = 0usize;
    let mut failed = Vec::new();
    for (n, res) in page_texts(&d, &pages) {
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
    let d = doc::load(&a.input, a.password.as_deref())?;
    // lopdf reports a missing outline as an error; for callers that is an empty list.
    let entries: Vec<Value> = d
        .get_toc()
        .map(|t| {
            t.toc
                .into_iter()
                .map(|e| json!({"level": e.level, "title": e.title, "page": e.page}))
                .collect()
        })
        .unwrap_or_default();
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
