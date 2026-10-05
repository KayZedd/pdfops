//! Optical character recognition for pages without a text layer.
//!
//! Recognition is delegated to the `tesseract` program: it covers over a
//! hundred languages, and linking an OCR engine and its models into the binary
//! would cost every user who never needs it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, anyhow, bail};
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
    /// Tesseract language codes joined with "+", e.g. "eng" or "pol+eng" (default: eng); missing ones are fetched with ocr-install
    #[arg(short, long)]
    pub lang: Option<String>,
    /// Resolution pages are rasterised at before recognition (default: 300)
    #[arg(long)]
    pub dpi: Option<f32>,
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

fn recognise_png(png: &[u8], lang: &str, dpi: f32, data: Option<&Path>) -> Result<String, String> {
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
    let engine = Engine::probe();
    let data = engine.data_dir(lang)?;
    let all = pdf.pages();
    let settings = InterpreterSettings::default();
    Ok(pages
        .par_iter()
        .map(|&n| {
            let text = page_png(&all[n as usize - 1], &settings, dpi)
                .map_err(|e| e.to_string())
                .and_then(|(png, ..)| recognise_png(&png, lang, dpi, data));
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
