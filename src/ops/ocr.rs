//! Optical character recognition for pages without a text layer.
//!
//! Recognition is delegated to the `tesseract` program: it covers over a
//! hundred languages, and linking an OCR engine and its models into the binary
//! would cost every user who never needs it.
//!
//! The words come back with their places on the page, so they can also be written
//! into a copy of the file as text nobody sees but every viewer can search and select.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Args;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use lopdf::{Dictionary, Document, ObjectId, Stream};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::font::TextFont;
use crate::ops::edit::{add_resource, append_content, own_resources, visual_space};
use crate::ops::render::{check_dpi, page_png};
use crate::progress::Progress;
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
    /// Tesseract language codes joined with "+", e.g. "eng" or "pol+eng" (default: eng); missing ones are fetched with ocr-install
    #[arg(short, long)]
    pub lang: Option<String>,
    /// Resolution pages are rasterised at before recognition (default: 300)
    #[arg(long)]
    pub dpi: Option<f32>,
    /// Where to write a copy of the PDF with the recognised text added as an invisible layer, so that it can be searched and selected (may be the input file)
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct OcrLangsArgs {}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct OcrInstallArgs {
    /// Language codes to download, joined with "+", e.g. "pol+eng" (tesseract codes: eng, pol, deu, fra, chi_sim, ...)
    #[arg(short, long)]
    pub lang: Option<String>,
    /// Also install the tesseract program itself if it is missing, using the system package manager
    #[arg(long)]
    #[serde(default)]
    pub engine: bool,
    /// Download the larger, more accurate models instead of the fast ones
    #[arg(long)]
    #[serde(default)]
    pub best: bool,
}

/// Where downloaded language data lives: per user, so no administrator rights are needed.
pub fn tessdata_dir() -> PathBuf {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(dir) = var("PDFOPS_TESSDATA") {
        return dir;
    }
    let home = var("HOME").unwrap_or_default();
    let base = if cfg!(windows) {
        var("LOCALAPPDATA").unwrap_or(home)
    } else if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        var("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share"))
    };
    base.join("pdfops").join("tessdata")
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|dir| dir.join(program).is_file() || dir.join(format!("{program}.exe")).is_file())
    })
}

/// The package manager command that installs tesseract, and whether it needs root.
pub fn engine_command(has: impl Fn(&str) -> bool) -> Option<(Vec<&'static str>, bool)> {
    let known: [(&[&str], bool); 7] = [
        (&["pacman", "-S", "--noconfirm", "tesseract"], true),
        (&["apt-get", "install", "-y", "tesseract-ocr"], true),
        (&["dnf", "install", "-y", "tesseract"], true),
        (
            &["zypper", "--non-interactive", "install", "tesseract-ocr"],
            true,
        ),
        (&["apk", "add", "tesseract-ocr"], true),
        (&["brew", "install", "tesseract"], false),
        (
            &[
                "winget",
                "install",
                "--id",
                "UB-Mannheim.TesseractOCR",
                "-e",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ],
            false,
        ),
    ];
    known
        .into_iter()
        .find(|(cmd, _)| has(cmd[0]))
        .map(|(cmd, root)| (cmd.to_vec(), root))
}

fn is_root() -> bool {
    cfg!(unix)
        && Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

/// The command a person would type to install tesseract here.
fn engine_hint() -> Option<String> {
    let (cmd, needs_root) = engine_command(on_path)?;
    let sudo = if needs_root && !is_root() {
        "sudo "
    } else {
        ""
    };
    Some(format!("{sudo}{}", cmd.join(" ")))
}

/// What OCR can use on this machine.
struct Engine {
    /// First line of `tesseract --version`, or `None` when the program is missing.
    version: Option<String>,
    /// Languages in tesseract's own data directory.
    system: Vec<String>,
    /// Languages downloaded by `ocr-install`.
    downloaded: Vec<String>,
    dir: PathBuf,
}

impl Engine {
    fn probe() -> Engine {
        let run = |arg: &str| {
            Command::new(PROGRAM)
                .arg(arg)
                .stdin(Stdio::null())
                .output()
                .ok()
        };
        let text = |o: std::process::Output| {
            String::from_utf8_lossy(&[o.stdout, o.stderr].concat()).into_owned()
        };
        let version = run("--version")
            .map(text)
            .and_then(|t| t.lines().next().map(str::to_string));
        // The first line is a heading; `osd` is script detection, not a language.
        let system = run("--list-langs")
            .map(text)
            .map(|t| {
                t.lines()
                    .skip(1)
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && *l != "osd")
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let dir = tessdata_dir();
        let mut downloaded: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                e.ok()?
                    .file_name()
                    .to_str()?
                    .strip_suffix(".traineddata")
                    .map(str::to_string)
            })
            .filter(|l| l != "osd")
            .collect();
        downloaded.sort();
        Engine {
            version,
            system,
            downloaded,
            dir,
        }
    }

    /// The data directory to pass to tesseract for `lang`; `None` means its own.
    fn data_dir(&self, lang: &str) -> Result<Option<&Path>> {
        if self.version.is_none() {
            bail!(
                "OCR needs the `{PROGRAM}` program, which is not installed. {}",
                match engine_hint() {
                    Some(cmd) => format!(
                        "Install it with: {cmd} (or `pdfops ocr-install --engine` where that needs no password)"
                    ),
                    None => "Install tesseract with your package manager".to_string(),
                }
            );
        }
        let wanted: Vec<&str> = lang.split('+').filter(|l| !l.is_empty()).collect();
        let all_in = |have: &[String]| {
            !wanted.is_empty() && wanted.iter().all(|l| have.iter().any(|h| h == l))
        };
        // Tesseract reads one data directory per run, so all languages must come from the same one.
        if all_in(&self.downloaded) {
            return Ok(Some(&self.dir));
        }
        if all_in(&self.system) {
            return Ok(None);
        }
        let absent: Vec<&str> = wanted
            .iter()
            .copied()
            .filter(|l| !self.downloaded.iter().any(|h| h == l))
            .collect();
        let have: Vec<&str> = self
            .system
            .iter()
            .chain(&self.downloaded)
            .map(String::as_str)
            .collect();
        let fix = format!(
            "`pdfops ocr-install --lang {}` (MCP tool pdf_ocr_install)",
            absent.join("+")
        );
        if absent.iter().all(|l| self.system.iter().any(|h| h == l)) {
            bail!(
                "'{}' is installed system-wide and cannot be combined with downloaded languages in one run. Add it next to them with {fix}",
                absent.join("+")
            );
        }
        bail!(
            "no OCR language data for '{}'. Download it with {fix}. Usable now: {}",
            absent.join("+"),
            if have.is_empty() {
                "none".to_string()
            } else {
                have.join(", ")
            }
        )
    }
}

/// Downloads one language model into `dir`. Returns the file, its size and whether it was fetched now.
pub fn install_language(lang: &str, dir: &Path, base_url: &str) -> Result<(PathBuf, u64, bool)> {
    if lang.is_empty() || !lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("invalid language code '{lang}'; codes look like eng, pol, deu, chi_sim");
    }
    let path = dir.join(format!("{lang}.traineddata"));
    if let Ok(meta) = std::fs::metadata(&path) {
        return Ok((path, meta.len(), false));
    }
    let url = format!("{}/{lang}.traineddata", base_url.trim_end_matches('/'));
    let mut response = ureq::get(&url).call().map_err(|e| match e {
        ureq::Error::StatusCode(404) => {
            anyhow!("tesseract has no language '{lang}'; codes look like eng, pol, deu, chi_sim")
        }
        other => anyhow!("cannot download {url}: {other}"),
    })?;
    let data = response
        .body_mut()
        .with_config()
        .limit(200 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| anyhow!("cannot download {url}: {e}"))?;
    // A real model is megabytes; anything tiny is an error page, not data.
    if data.len() < 1024 {
        bail!(
            "download of {url} returned {} bytes, which is not a language model",
            data.len()
        );
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    // Written under another name first, so an interrupted download never looks installed.
    let partial = dir.join(format!("{lang}.traineddata.part{}", std::process::id()));
    std::fs::write(&partial, &data)
        .and_then(|_| std::fs::rename(&partial, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&partial);
            anyhow!("cannot write {}: {e}", path.display())
        })?;
    Ok((path, data.len() as u64, true))
}

pub fn ocr_langs(_: OcrLangsArgs) -> Result<Value> {
    let engine = Engine::probe();
    Ok(json!({
        "tesseract": engine.version,
        "install_tesseract_with": if engine.version.is_none() { engine_hint() } else { None },
        "system_languages": engine.system,
        "downloaded_languages": engine.downloaded,
        "tessdata_dir": engine.dir,
    }))
}

pub fn ocr_install(a: OcrInstallArgs) -> Result<Value> {
    let langs: Vec<&str> = a
        .lang
        .as_deref()
        .unwrap_or("")
        .split('+')
        .filter(|l| !l.is_empty())
        .collect();
    if langs.is_empty() && !a.engine {
        bail!("nothing to install: give languages, e.g. lang \"pol+eng\", or ask for the engine");
    }
    if a.engine && crate::sandbox::active() {
        bail!("installing programs is not available to a server confined to a directory");
    }
    let mut engine_installed = false;
    if a.engine && Engine::probe().version.is_none() {
        let (cmd, needs_root) = engine_command(on_path).ok_or_else(|| {
            anyhow!("no supported package manager found; install the tesseract program manually")
        })?;
        if needs_root && !is_root() {
            bail!(
                "installing tesseract needs administrator rights; ask the user to run: sudo {}",
                cmd.join(" ")
            );
        }
        let out = Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| anyhow!("cannot run {}: {e}", cmd[0]))?;
        if !out.status.success() {
            let reason = String::from_utf8_lossy(&out.stderr);
            bail!(
                "`{}` failed: {}",
                cmd.join(" "),
                reason.lines().last().unwrap_or("unknown error").trim()
            );
        }
        engine_installed = true;
    }

    let dir = tessdata_dir();
    let quality = if a.best { "best" } else { "fast" };
    let base = std::env::var("PDFOPS_TESSDATA_URL").unwrap_or_else(|_| {
        format!("https://raw.githubusercontent.com/tesseract-ocr/tessdata_{quality}/main")
    });
    let languages: Vec<Value> = langs
        .par_iter()
        .map(|lang| {
            let (file, size, fetched) = install_language(lang, &dir, &base)?;
            Ok(json!({"lang": lang, "file": file, "size_bytes": size, "downloaded": fetched}))
        })
        .collect::<Result<_>>()?;
    let engine = Engine::probe();
    Ok(json!({
        "languages": languages,
        "tessdata_dir": dir,
        "tesseract": engine.version,
        "tesseract_installed_now": engine_installed,
        "install_tesseract_with": if engine.version.is_none() { engine_hint() } else { None },
    }))
}

/// A recognised line of text: where it is on the raster, in pixels from the top-left
/// corner, and its words with their places.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Left, top, right, bottom.
    pub bbox: [f64; 4],
    pub words: Vec<(String, [f64; 4])>,
    /// Lines of one paragraph share this.
    pub paragraph: (u32, u32),
}

/// Reads tesseract's table of what it found: one row per block, paragraph, line and word.
pub fn parse_tsv(tsv: &str) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    for row in tsv.lines().skip(1) {
        let cells: Vec<&str> = row.splitn(12, '\t').collect();
        let number = |at: usize| cells.get(at).and_then(|c| c.trim().parse::<f64>().ok());
        let (Some(level), Some(block), Some(paragraph)) = (number(0), number(2), number(3)) else {
            continue;
        };
        let (Some(left), Some(top), Some(width), Some(height)) =
            (number(6), number(7), number(8), number(9))
        else {
            continue;
        };
        let bbox = [left, top, left + width, top + height];
        let text = cells.get(11).map_or("", |t| t.trim());
        if level == 4.0 {
            lines.push(Line {
                bbox,
                words: Vec::new(),
                paragraph: (block as u32, paragraph as u32),
            });
        } else if level == 5.0
            && !text.is_empty()
            && let Some(line) = lines.last_mut()
        {
            line.words.push((text.to_string(), bbox));
        }
    }
    lines.retain(|line| !line.words.is_empty());
    lines
}

/// The text of recognised lines: a line each, and an empty one between paragraphs.
fn plain(lines: &[Line]) -> String {
    let mut out = String::new();
    for (at, line) in lines.iter().enumerate() {
        if at > 0 {
            out.push('\n');
            if lines[at - 1].paragraph != line.paragraph {
                out.push('\n');
            }
        }
        let words: Vec<&str> = line.words.iter().map(|w| w.0.as_str()).collect();
        out += &words.join(" ");
    }
    out
}

/// Writes recognised lines onto a page as text that is not drawn. Returns how many words.
///
/// `dpi` is the resolution of the raster the lines were found on. Every word is
/// stretched to the width of what it was read from, so a selection covers the picture
/// of the word. `open` is a stream holding just `q`, as `append_content` wants it.
pub fn add_text_layer(
    d: &mut Document,
    page: ObjectId,
    lines: &[Line],
    dpi: f32,
    font: &TextFont,
    open: ObjectId,
) -> Result<usize> {
    let (m, _, height) = visual_space(doc::page_box(d, page), doc::rotation(d, page));
    let mut resources = own_resources(d, page);
    let names: Vec<String> = font
        .ids()
        .into_iter()
        .map(|id| add_resource(d, &mut resources, "Font", "PdfopsOcr", id))
        .collect();
    let point = 72.0 / dpi as f64;
    let mut ops = format!(
        "q\n{} {} {} {} {} {} cm\nBT\n3 Tr\n",
        m[0], m[1], m[2], m[3], m[4], m[5]
    );
    let mut words = 0;
    for line in lines {
        // A line's box runs from the top of its tall letters to the bottom of its
        // descenders; the baseline lies about a fifth of the way up.
        let size = ((line.bbox[3] - line.bbox[1]) * point).max(1.0);
        let baseline = height - line.bbox[3] * point + 0.2 * size;
        for (text, bbox) in &line.words {
            let natural = font.width(text, size);
            if natural <= 0.0 {
                continue;
            }
            let stretch = ((bbox[2] - bbox[0]) * point / natural * 100.0).clamp(1.0, 1000.0);
            ops += &format!(
                "{stretch:.1} Tz\n1 0 0 1 {:.2} {baseline:.2} Tm\n{}\n",
                bbox[0] * point,
                font.show_named(&names, text, size)
            );
            words += 1;
        }
    }
    append_content(d, page, open, ops + "ET\nQ\n", resources)?;
    Ok(words)
}

fn recognise_png(
    png: &[u8],
    lang: &str,
    dpi: f32,
    data: Option<&Path>,
    places: bool,
) -> Result<String, String> {
    let mut command = Command::new(PROGRAM);
    if let Some(dir) = data {
        command.arg("--tessdata-dir").arg(dir);
    }
    let mut child = command
        .args([
            "stdin",
            "stdout",
            "-l",
            lang,
            "--dpi",
            &format!("{}", dpi.round() as u32),
        ])
        // The table of words with their places, in place of the plain text.
        .args(places.then_some("tsv"))
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

/// Recognises the text of each requested page.
pub fn recognise(
    pdf: &Pdf,
    pages: &[u32],
    lang: &str,
    dpi: f32,
) -> Result<Vec<(u32, Result<String, String>)>> {
    run(pdf, pages, lang, dpi, false)
}

/// Runs tesseract over each requested page, in parallel, for its text or for its
/// table of words with their places.
fn run(
    pdf: &Pdf,
    pages: &[u32],
    lang: &str,
    dpi: f32,
    places: bool,
) -> Result<Vec<(u32, Result<String, String>)>> {
    let engine = Engine::probe();
    let data = engine.data_dir(lang)?;
    let all = pdf.pages();
    let settings = InterpreterSettings::default();
    let progress = Progress::new("ocr", pages.len());
    Ok(pages
        .par_iter()
        .map(|&n| {
            let text = page_png(&all[n as usize - 1], &settings, dpi)
                .map_err(|e| e.to_string())
                .and_then(|(png, ..)| recognise_png(&png, lang, dpi, data, places));
            progress.tick(json!({"page": n, "recognised": text.is_ok()}));
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
    let Some(output) = &a.output else {
        let out: Vec<Value> = recognise(&pdf, &pages, lang, dpi)?
            .into_iter()
            .map(|(n, text)| match text {
                Ok(t) => json!({"page": n, "text": super::read::tidy(&t)}),
                Err(e) => json!({"page": n, "error": e}),
            })
            .collect();
        return Ok(json!({"file": a.input, "total_pages": total, "lang": lang, "pages": out}));
    };

    let read: Vec<(u32, Result<Vec<Line>, String>)> = run(&pdf, &pages, lang, dpi, true)?
        .into_iter()
        .map(|(n, table)| (n, table.map(|table| parse_tsv(&table))))
        .collect();
    // A page that has text of its own keeps it; a second copy would be found twice.
    let own: std::collections::HashSet<u32> = super::read::page_texts(&pdf, &pages)
        .into_iter()
        .filter(|(_, text)| text.as_ref().is_ok_and(|t| !t.trim().is_empty()))
        .map(|(n, _)| n)
        .collect();
    let mut d = doc::load(&a.input, a.password.as_deref())?;
    let ids = doc::page_ids(&d);
    let wanted: String = read
        .iter()
        .filter(|(n, _)| !own.contains(n))
        .filter_map(|(_, lines)| lines.as_ref().ok())
        .flatten()
        .flat_map(|line| line.words.iter().map(|w| w.0.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    let font = TextFont::new(&mut d, &wanted, None).context("cannot write the text layer")?;
    let open = d.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let mut out = Vec::new();
    let mut done: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (n, lines) in read {
        out.push(match lines {
            Ok(lines) => {
                let mut page = json!({"page": n, "text": super::read::tidy(&plain(&lines))});
                if own.contains(&n) {
                    page["text_layer"] = json!("kept: the page has text of its own");
                } else if done.insert(n) {
                    let words =
                        add_text_layer(&mut d, ids[n as usize - 1], &lines, dpi, &font, open)?;
                    page["text_layer"] = json!("added");
                    page["words"] = json!(words);
                }
                page
            }
            Err(e) => json!({"page": n, "error": e}),
        });
    }
    let size = doc::save(&mut d, output)?;
    Ok(json!({
        "file": a.input,
        "total_pages": total,
        "lang": lang,
        "pages": out,
        "output": output,
        "size_bytes": size,
    }))
}
