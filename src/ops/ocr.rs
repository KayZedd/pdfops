//! Optical character recognition for pages without a text layer.
//!
//! Recognition is delegated to the `tesseract` program: it covers over a
//! hundred languages, and linking an OCR engine and its models into the binary
//! would cost every user who never needs it.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Result, anyhow, bail};
use clap::Args;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::ops::render::{check_dpi, page_png};
use crate::{doc, pagespec};

const PROGRAM: &str = "tesseract";

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct OcrArgs {
    /// PDF file to recognise, typically a scan
    pub input: PathBuf,
    /// Pages to recognise, e.g. "1-3" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// Tesseract language codes joined with "+", e.g. "eng" or "pol+eng" (default: eng)
    #[arg(short, long)]
    pub lang: Option<String>,
    /// Resolution pages are rasterised at before recognition (default: 300)
    #[arg(long)]
    pub dpi: Option<f32>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

/// Checks that tesseract is installed and has data for every requested language.
fn check_engine(lang: &str) -> Result<()> {
    let listed = Command::new(PROGRAM).arg("--list-langs").stdin(Stdio::null()).output().map_err(|e| {
        anyhow!("OCR needs the `{PROGRAM}` program on PATH ({e}); install tesseract and a language pack")
    })?;
    let text = [listed.stdout, listed.stderr].concat();
    // The first line is a heading; `osd` is script detection, not a language.
    let installed: Vec<&str> = std::str::from_utf8(&text)
        .unwrap_or("")
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "osd")
        .collect();
    let absent: Vec<&str> = lang.split('+').filter(|l| !installed.contains(l)).collect();
    if !absent.is_empty() {
        bail!(
            "tesseract has no language data for '{}'; installed: {}. Install the language pack or pass another lang",
            absent.join("+"),
            if installed.is_empty() {
                "none".to_string()
            } else {
                installed.join(", ")
            }
        );
    }
    Ok(())
}

fn recognise_png(png: &[u8], lang: &str, dpi: f32) -> Result<String, String> {
    let mut child = Command::new(PROGRAM)
        .args([
            "stdin",
            "stdout",
            "-l",
            lang,
            "--dpi",
            &format!("{}", dpi.round() as u32),
        ])
        // Pages already run in parallel; one thread each avoids oversubscribing the CPU.
        .env("OMP_THREAD_LIMIT", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {PROGRAM}: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("cannot reach tesseract's input")?;
    // A broken pipe here means tesseract exited early; its own message below says why.
    let _ = stdin.write_all(png);
    drop(stdin);
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{PROGRAM} failed: {e}"))?;
    if !out.status.success() {
        let reason = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{PROGRAM} failed: {}",
            reason.lines().last().unwrap_or("unknown error").trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Recognises the text of each requested page, in parallel.
pub fn recognise(
    pdf: &Pdf,
    pages: &[u32],
    lang: &str,
    dpi: f32,
) -> Result<Vec<(u32, Result<String, String>)>> {
    check_engine(lang)?;
    let all = pdf.pages();
    let settings = InterpreterSettings::default();
    Ok(pages
        .par_iter()
        .map(|&n| {
            let text = page_png(&all[n as usize - 1], &settings, dpi)
                .map_err(|e| e.to_string())
                .and_then(|(png, ..)| recognise_png(&png, lang, dpi));
            (n, text)
        })
        .collect())
}

pub fn ocr(a: OcrArgs) -> Result<Value> {
    let dpi = check_dpi(a.dpi.unwrap_or(300.0))?;
    let lang = a.lang.as_deref().unwrap_or("eng");
    let (pdf, _) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let total = pdf.pages().len() as u32;
    let pages = pagespec::parse_or_all(a.pages.as_deref(), total)?;
    let out: Vec<Value> = recognise(&pdf, &pages, lang, dpi)?
        .into_iter()
        .map(|(n, text)| match text {
            Ok(t) => json!({"page": n, "text": super::read::tidy(&t)}),
            Err(e) => json!({"page": n, "error": e}),
        })
        .collect();
    Ok(json!({"file": a.input, "total_pages": total, "lang": lang, "pages": out}))
}
