//! Inspection of a document for content that does something rather than says
//! something: scripts, actions that fire on their own, programs to launch,
//! attached files, text a person cannot see. `sanitize` removes what `scan`
//! found and checks the result with `scan` before writing it.
//!
//! This is structural inspection, not virus detection. A file is judged from
//! its bytes and through the repairing parser, so a damaged or hostile file is
//! reported on instead of refused. The result never says "safe": it says what
//! was found, what was looked for, and what was not.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::ops::Not;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use clap::{Args, ValueEnum};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::object::{
    Array, Dict, MaybeRef, Name, Object as LazyObject, Stream as LazyStream, String as LazyString,
};
use hayro::hayro_syntax::{Filter, LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use lopdf::{Dictionary, Object, ObjectId};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::ops::edit::prune;
use crate::ops::layout;
use crate::ops::redact::round_box;
use crate::ops::render::raster_scale;
use crate::{doc, limits, pagespec};

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct ScanArgs {
    /// PDF file to inspect
    pub input: PathBuf,
    /// Pages searched for hidden text, e.g. "1-20" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// Leave out the search for hidden text, which renders every page
    #[arg(long)]
    #[serde(default)]
    pub skip_hidden_text: bool,
    /// Also run the file through the clamscan program (ClamAV signatures), if it is installed
    #[arg(long)]
    #[serde(default)]
    pub clamav: bool,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

/// A group of things `sanitize` removes.
#[derive(ValueEnum, Deserialize, JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// Scripts, wherever they are attached
    Javascript,
    /// Actions that fire on their own, start programs, send form data or open other files
    Actions,
    /// Embedded files and file attachment annotations
    Attachments,
    /// XFA form definitions
    Xfa,
    /// Rich media, 3D, movie, sound and screen annotations
    Media,
}

const RISKS: [Risk; 5] = [
    Risk::Javascript,
    Risk::Actions,
    Risk::Attachments,
    Risk::Xfa,
    Risk::Media,
];

impl Risk {
    fn name(self) -> &'static str {
        match self {
            Risk::Javascript => "javascript",
            Risk::Actions => "actions",
            Risk::Attachments => "attachments",
            Risk::Xfa => "xfa",
            Risk::Media => "media",
        }
    }

    /// The kinds of finding that must be gone once this group has been removed.
    fn kinds(self) -> &'static [&'static str] {
        match self {
            Risk::Javascript => &["javascript"],
            Risk::Actions => &[
                "auto_action",
                "launch",
                "submit_form",
                "import_data",
                "remote_goto",
            ],
            Risk::Attachments => &["embedded_file"],
            Risk::Xfa => &["xfa"],
            Risk::Media => &["rich_media"],
        }
    }
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct SanitizeArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Where to write the cleaned PDF (may be the input file)
    #[arg(short, long)]
    pub output: PathBuf,
    /// Groups to leave in place; everything else is removed (default: remove all)
    #[arg(long = "keep", value_enum)]
    #[serde(default)]
    pub keep: Vec<Risk>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
    /// Report what would be removed without writing the output file
    #[arg(long)]
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Severity {
    Info,
    Low,
    Medium,
    High,
}

impl Severity {
    fn name(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
        }
    }
}

/// Everything found of one kind.
struct Finding {
    severity: Severity,
    count: usize,
    /// Object numbers it was found in, as far as they are known.
    objects: Vec<i32>,
    samples: Vec<Value>,
    more: Map<String, Value>,
}

const MAX_SAMPLES: usize = 20;

#[derive(Default)]
pub(crate) struct Findings {
    by_kind: BTreeMap<&'static str, Finding>,
    /// Kinds whose name the object walk came across, flagged or not.
    named: HashSet<&'static str>,
}

/// What each kind of finding means, in one sentence.
fn describe(kind: &str) -> &'static str {
    match kind {
        "javascript" => "JavaScript that a viewer may run",
        "auto_action" => {
            "an action that runs without being clicked: on opening, on a page turn or on a form event"
        }
        "launch" => "an action that starts a program or opens a file with the system's handler",
        "submit_form" => "an action that sends form data to an address",
        "import_data" => "an action that loads form data from a file",
        "remote_goto" => {
            "an action that opens another file; a network path makes the viewer contact that host"
        }
        "external_link" => "links to addresses outside the document",
        "embedded_file" => "a file carried inside the document",
        "xfa" => "an XFA form, a second document format with its own scripting",
        "rich_media" => {
            "embedded media that a viewer plays: rich media, 3D, movie, sound or screen"
        }
        "obfuscated_name" => {
            "names written with needless #xx escapes, which hides them from simple scanners"
        }
        "filter_chain" => {
            "streams packed several times over, a way to hide content or to expand hugely"
        }
        "decompression_bomb" => "a stream that expands to an enormous size",
        "misplaced_image_filter" => {
            "image compression on a stream that is not an image, so a reader decodes a picture where it expects something else"
        }
        "resource_exhaustion" => {
            "processing the file ran out of memory or time, as files built for that do; a very heavy honest file does the same"
        }
        "huge_image" => "an image whose declared size is beyond anything displayable",
        "implausible_fax_parameters" => "fax-coded image data with impossible dimensions",
        "cyclic_structure" => "a page tree that refers back to itself",
        "header_not_at_start" => {
            "the PDF header is not at the start of the file, as in files built to be two formats at once"
        }
        "trailing_data" => "data after the end of the PDF",
        "shadowed_objects" => {
            "object numbers defined more than once in a single revision, so readers may disagree on the content"
        }
        "hidden_text" => {
            "text that is extracted but that a person looking at the page does not see"
        }
        "invisible_text_layer" => {
            "invisible text lying over visible content, as scanned pages with recognised text have; it may differ from what is shown"
        }
        "unparseable" => "the file could not be opened as a PDF; only its bytes were inspected",
        "encrypted_unread" => {
            "the file is encrypted and was not opened; only names outside its encrypted and packed parts were inspected"
        }
        "known_malware" => "ClamAV recognises the file",
        _ => "",
    }
}

impl Findings {
    fn add(
        &mut self,
        kind: &'static str,
        severity: Severity,
        object: Option<i32>,
        sample: Option<Value>,
    ) -> &mut Finding {
        self.named.insert(kind);
        let finding = self.by_kind.entry(kind).or_insert_with(|| Finding {
            severity,
            count: 0,
            objects: Vec::new(),
            samples: Vec::new(),
            more: Map::new(),
        });
        finding.severity = finding.severity.max(severity);
        finding.count += 1;
        if let Some(id) = object
            && finding.objects.len() < MAX_SAMPLES
            && !finding.objects.contains(&id)
        {
            finding.objects.push(id);
        }
        if let Some(sample) = sample
            && finding.samples.len() < MAX_SAMPLES
            && !finding.samples.contains(&sample)
        {
            finding.samples.push(sample);
        }
        finding
    }

    /// Whether a readable object gave rise to a finding of this kind.
    pub(crate) fn has(&self, kind: &str) -> bool {
        self.by_kind
            .get(kind)
            .is_some_and(|f| !f.more.contains_key("note"))
    }

    fn highest(&self) -> Option<Severity> {
        self.by_kind.values().map(|f| f.severity).max()
    }

    /// The findings as JSON, the most severe first.
    fn report(&self) -> Vec<Value> {
        let mut all: Vec<(&&str, &Finding)> = self.by_kind.iter().collect();
        all.sort_by(|a, b| b.1.severity.cmp(&a.1.severity).then(a.0.cmp(b.0)));
        all.into_iter()
            .map(|(kind, f)| {
                let mut entry = json!({
                    "kind": kind,
                    "severity": f.severity.name(),
                    "count": f.count,
                    "description": describe(kind),
                });
                if !f.objects.is_empty() {
                    entry["objects"] = json!(f.objects);
                }
                if !f.samples.is_empty() {
                    entry["samples"] = json!(f.samples);
                }
                if let Some(map) = entry.as_object_mut() {
                    map.extend(f.more.clone());
                }
                entry
            })
            .collect()
    }
}

/// Decodes a PDF text string: UTF-16 with a byte order mark, otherwise one byte per character.
fn text_of(bytes: &[u8]) -> String {
    match bytes {
        [0xFE, 0xFF, rest @ ..] => {
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| u16::from_be_bytes(*p))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => bytes.iter().map(|&b| b as char).collect(),
    }
}

fn excerpt(text: &str, limit: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out: String = clean.chars().take(limit).collect();
    if clean.chars().count() > limit {
        out.push('…');
    }
    out
}

/// Inflates zlib data without keeping it, up to `limit` bytes. Returns how much came out
/// and its first bytes.
fn inflate(data: &[u8], limit: u64, keep: usize) -> (u64, Vec<u8>) {
    let mut decoder = flate2::read::ZlibDecoder::new(data);
    let mut buffer = vec![0u8; 64 * 1024];
    let mut head = Vec::new();
    let mut total = 0u64;
    while total <= limit {
        match decoder.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if head.len() < keep {
                    head.extend_from_slice(&buffer[..n.min(keep - head.len())]);
                }
                total += n as u64;
            }
        }
    }
    (total, head)
}

/// The first `keep` bytes of a stream's content, when it is stored plainly or deflated once.
fn head(stream: &LazyStream<'_>, keep: usize) -> Option<Vec<u8>> {
    let raw = stream.raw_data();
    match stream.filters().as_slice() {
        [] => Some(raw[..raw.len().min(keep)].to_vec()),
        [Filter::FlateDecode] => Some(inflate(&raw, keep as u64, keep).1),
        _ => None,
    }
}

/// What a file is, from its first bytes and its name. The severity says how much
/// an attachment of that type deserves attention.
fn file_type(head: &[u8], name: &str) -> (&'static str, Severity) {
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    let by_magic = match head {
        [b'M', b'Z', ..] => Some(("executable", Severity::High)),
        [0x7f, b'E', b'L', b'F', ..] => Some(("executable", Severity::High)),
        [0xfe, 0xed, 0xfa, 0xce | 0xcf, ..] | [0xce | 0xcf, 0xfa, 0xed, 0xfe, ..] => {
            Some(("executable", Severity::High))
        }
        [0xca, 0xfe, 0xba, 0xbe, ..] => Some(("executable", Severity::High)),
        [b'#', b'!', ..] => Some(("script", Severity::High)),
        [b'%', b'P', b'D', b'F', ..] => Some(("pdf", Severity::Medium)),
        [0xd0, 0xcf, 0x11, 0xe0, ..] => Some(("office document (old format)", Severity::Medium)),
        [b'P', b'K', 3, 4, ..] => Some(match ext {
            "docm" | "xlsm" | "pptm" | "dotm" | "xltm" => {
                ("office document with macros", Severity::High)
            }
            "docx" | "xlsx" | "pptx" => ("office document", Severity::Low),
            "jar" | "apk" => ("executable", Severity::High),
            _ => ("archive", Severity::Medium),
        }),
        [b'R', b'a', b'r', b'!', ..] | [b'7', b'z', 0xbc, 0xaf, ..] | [0x1f, 0x8b, ..] => {
            Some(("archive", Severity::Medium))
        }
        _ => None,
    };
    by_magic.unwrap_or(match ext {
        "exe" | "dll" | "scr" | "com" | "msi" | "jar" | "lnk" | "hta" => {
            ("executable", Severity::High)
        }
        "js" | "jse" | "vbs" | "vbe" | "wsf" | "ps1" | "bat" | "cmd" | "sh" | "py" => {
            ("script", Severity::High)
        }
        "html" | "htm" | "svg" => ("web page", Severity::Medium),
        _ => ("data", Severity::Low),
    })
}

/// Whether a file target names another machine.
fn remote(target: &str) -> bool {
    target.starts_with("\\\\") || target.starts_with("//") || target.contains("://")
}

/// The file an action or file specification points at.
fn file_target(dict: &Dict<'_>) -> Option<String> {
    let name = |d: &Dict<'_>| {
        ["UF", "F"]
            .iter()
            .find_map(|key| d.get::<LazyString<'_>>(key))
            .map(|s| text_of(&s))
    };
    match dict.get::<LazyObject<'_>>("F")? {
        LazyObject::String(s) => Some(text_of(&s)),
        LazyObject::Dict(spec) => name(&spec),
        _ => None,
    }
}

/// Looks at every object the parser can reach.
struct Walker<'f> {
    found: &'f mut Findings,
}

impl Walker<'_> {
    fn walk(&mut self, object: &LazyObject<'_>, id: Option<i32>, depth: u32) {
        if depth > 48 {
            return;
        }
        match object {
            LazyObject::Dict(dict) => self.dict(dict, id, depth),
            LazyObject::Stream(stream) => {
                self.stream(stream, id);
                self.dict(stream.dict(), id, depth);
            }
            LazyObject::Array(array) => self.array(array, id, depth),
            _ => {}
        }
    }

    fn array(&mut self, array: &Array<'_>, id: Option<i32>, depth: u32) {
        for item in array.raw_iter() {
            if let MaybeRef::NotRef(object) = item {
                self.walk(&object, id, depth + 1);
            }
        }
    }

    fn dict(&mut self, dict: &Dict<'_>, id: Option<i32>, depth: u32) {
        let id = dict.obj_id().map(|i| i.obj_number).or(id);
        self.action(dict, id);
        for (key, value) in dict.entries() {
            self.note(&key);
            if let MaybeRef::NotRef(LazyObject::Name(name)) = &value {
                self.note(name);
            }
            match &*key {
                b"JS" => {
                    // An action dictionary with a script is counted by its type; this
                    // catches the ones that carry a script and no type.
                    if dict
                        .get::<Name<'_>>("S")
                        .is_none_or(|s| &*s != b"JavaScript")
                    {
                        self.javascript(dict, id);
                    }
                }
                b"OpenAction" => {
                    // A plain destination just says where to open; an action does something.
                    if let Some(action) = dict.get::<Dict<'_>>("OpenAction") {
                        let kind = action.get::<Name<'_>>("S");
                        let kind = kind.as_deref().unwrap_or(b"");
                        if kind != b"GoTo" {
                            let severity = match kind {
                                b"JavaScript" | b"Launch" => Severity::High,
                                _ => Severity::Medium,
                            };
                            let what = String::from_utf8_lossy(kind).into_owned();
                            self.found.add(
                                "auto_action",
                                severity,
                                id,
                                Some(json!({"when": "document opens", "action": what})),
                            );
                        }
                    }
                }
                b"AA" => {
                    let events: Vec<String> = dict
                        .get::<Dict<'_>>("AA")
                        .map(|aa| aa.keys().map(|k| k.as_str().to_string()).collect())
                        .unwrap_or_default();
                    self.found.add(
                        "auto_action",
                        Severity::Medium,
                        id,
                        Some(json!({"when": "events", "events": events})),
                    );
                }
                b"XFA" => {
                    self.found.add("xfa", Severity::Medium, id, None);
                }
                b"EF" => self.embedded(dict, id),
                b"Subtype" => {
                    if let MaybeRef::NotRef(LazyObject::Name(subtype)) = &value {
                        let severity = match &**subtype {
                            b"RichMedia" => Some(Severity::High),
                            b"3D" | b"Movie" | b"Sound" | b"Screen" => Some(Severity::Medium),
                            _ => None,
                        };
                        // Only annotations: an image or font may use the same word otherwise.
                        if let Some(severity) = severity
                            && dict.contains_key("Rect")
                        {
                            self.found.add(
                                "rich_media",
                                severity,
                                id,
                                Some(json!({"annotation": subtype.as_str()})),
                            );
                        }
                    }
                }
                _ => {}
            }
            if let MaybeRef::NotRef(object) = value {
                self.walk(&object, id, depth + 1);
            }
        }
    }

    /// Records that a name the raw pass counts was met in a readable object.
    fn note(&mut self, name: &[u8]) {
        for (known, kind, _) in RAW_NAMES {
            if name == known.as_bytes() {
                self.found.named.insert(kind);
            }
        }
    }

    fn javascript(&mut self, dict: &Dict<'_>, id: Option<i32>) {
        let script = match dict.get::<LazyObject<'_>>("JS") {
            Some(LazyObject::String(s)) => Some(text_of(&s)),
            Some(LazyObject::Stream(s)) => head(&s, 4096).map(|b| text_of(&b)),
            _ => None,
        };
        let sample = script.map(|s| json!({"script": excerpt(&s, 200)}));
        self.found.add("javascript", Severity::High, id, sample);
    }

    /// Flags the dictionary if it is an action of a kind worth knowing about.
    fn action(&mut self, dict: &Dict<'_>, id: Option<i32>) {
        let Some(kind) = dict.get::<Name<'_>>("S") else {
            return;
        };
        let target = || file_target(dict).map(|t| excerpt(&t, 200));
        match &*kind {
            b"JavaScript" => self.javascript(dict, id),
            b"Launch" => {
                let mut sample = json!({});
                if let Some(target) = target() {
                    sample["target"] = json!(target);
                }
                if let Some(win) = dict.get::<Dict<'_>>("Win") {
                    for (name, key) in [("program", "F"), ("parameters", "P")] {
                        if let Some(value) = win.get::<LazyString<'_>>(key) {
                            sample[name] = json!(excerpt(&text_of(&value), 200));
                        }
                    }
                }
                self.found.add("launch", Severity::High, id, Some(sample));
            }
            b"URI" => {
                let uri = dict
                    .get::<LazyString<'_>>("URI")
                    .map(|u| text_of(&u))
                    .unwrap_or_default();
                let lower = uri.trim_start().to_lowercase();
                if lower.starts_with("javascript:") {
                    self.found.add(
                        "javascript",
                        Severity::High,
                        id,
                        Some(json!({"script": excerpt(&uri, 200)})),
                    );
                } else if lower.starts_with("file:") || remote(&uri) && !lower.contains("://") {
                    self.found.add(
                        "remote_goto",
                        Severity::High,
                        id,
                        Some(json!({"target": excerpt(&uri, 200)})),
                    );
                } else {
                    // The host is enough to judge by, and keeps the list short.
                    let host = lower
                        .split_once("://")
                        .map_or(lower.as_str(), |(_, rest)| rest)
                        .split(['/', '?', '#'])
                        .next()
                        .unwrap_or("")
                        .to_string();
                    self.found.add(
                        "external_link",
                        Severity::Info,
                        id,
                        Some(json!(excerpt(&host, 100))),
                    );
                }
            }
            b"SubmitForm" | b"ImportData" => {
                let kind = if &*kind == b"SubmitForm" {
                    "submit_form"
                } else {
                    "import_data"
                };
                let sample = target().map(|t| json!({"target": t}));
                self.found.add(kind, Severity::Medium, id, sample);
            }
            b"GoToR" | b"GoToE" => {
                let target = target();
                let severity = if target.as_deref().is_some_and(remote) {
                    Severity::High
                } else {
                    Severity::Low
                };
                self.found.add(
                    "remote_goto",
                    severity,
                    id,
                    target.map(|t| json!({"target": t})),
                );
            }
            b"Rendition" | b"Movie" | b"Sound" | b"RichMediaExecute" | b"GoTo3DView" => {
                self.found.add(
                    "rich_media",
                    Severity::Medium,
                    id,
                    Some(json!({"action": kind.as_str()})),
                );
            }
            _ => {}
        }
    }

    /// A file specification with an embedded file.
    fn embedded(&mut self, spec: &Dict<'_>, id: Option<i32>) {
        let name = ["UF", "F"]
            .iter()
            .find_map(|key| spec.get::<LazyString<'_>>(key))
            .map(|s| text_of(&s))
            .unwrap_or_default();
        let stream = spec.get::<Dict<'_>>("EF").and_then(|ef| {
            ["UF", "F", "DOS", "Mac", "Unix"]
                .iter()
                .find_map(|key| ef.get::<LazyStream<'_>>(key))
        });
        let (kind, severity) = file_type(
            &stream
                .as_ref()
                .and_then(|s| head(s, 16))
                .unwrap_or_default(),
            &name,
        );
        let mut sample = json!({"name": excerpt(&name, 200), "type": kind});
        if let Some(stream) = &stream {
            let stated = stream
                .dict()
                .get::<Dict<'_>>("Params")
                .and_then(|p| p.get::<i32>("Size"));
            sample["size_bytes"] = match stated {
                Some(size) => json!(size),
                None => json!(stream.raw_data().len()),
            };
        }
        self.found.add("embedded_file", severity, id, Some(sample));
    }

    fn stream(&mut self, stream: &LazyStream<'_>, id: Option<i32>) {
        let id = Some(stream.obj_id().obj_number).filter(|n| *n != 0).or(id);
        let dict = stream.dict();
        let filters = stream.filters();
        let packing = filters
            .iter()
            .filter(|f| {
                matches!(
                    f,
                    Filter::FlateDecode | Filter::LzwDecode | Filter::RunLengthDecode
                )
            })
            .count();
        if filters.len() > 2 || packing > 1 {
            self.found.add(
                "filter_chain",
                Severity::Medium,
                id,
                Some(json!({"filters": filters.len()})),
            );
        }
        let pictorial = |f: &Filter| {
            matches!(
                f,
                Filter::Jbig2Decode
                    | Filter::CcittFaxDecode
                    | Filter::DctDecode
                    | Filter::JpxDecode
            )
        };
        // Thumbnails and masks state a size without calling themselves images.
        let image = dict
            .get::<Name<'_>>("Subtype")
            .is_some_and(|s| &*s == b"Image")
            || (dict.contains_key("Width") && dict.contains_key("Height"));
        if !image && filters.iter().any(pictorial) {
            self.found
                .add("misplaced_image_filter", Severity::Medium, id, None);
        }
        // Deflate tops out near 1032 to 1, so only a large stream can expand enormously.
        const BOMB: u64 = 256 * 1024 * 1024;
        if filters.first() == Some(&Filter::FlateDecode) {
            let raw = stream.raw_data();
            if raw.len() as u64 > BOMB / 1100 {
                let (size, _) = inflate(&raw, BOMB, 0);
                if size > BOMB && size / raw.len() as u64 > 100 {
                    self.found.add(
                        "decompression_bomb",
                        Severity::High,
                        id,
                        Some(json!({"stored_bytes": raw.len(), "expands_beyond_bytes": BOMB})),
                    );
                }
            }
        }
        let number = |d: &Dict<'_>, key: &str| d.get::<f32>(key).map(|v| v as f64);
        if dict
            .get::<Name<'_>>("Subtype")
            .is_some_and(|s| &*s == b"Image")
        {
            let (w, h) = (
                number(dict, "Width").unwrap_or(0.0),
                number(dict, "Height").unwrap_or(0.0),
            );
            if w > 30000.0 || h > 30000.0 || w * h > 4.0e8 {
                self.found.add(
                    "huge_image",
                    Severity::Medium,
                    id,
                    Some(json!({"width": w, "height": h})),
                );
            }
        }
        if filters.contains(&Filter::CcittFaxDecode) {
            let parms = dict.get::<Dict<'_>>("DecodeParms").or_else(|| {
                dict.get::<Array<'_>>("DecodeParms")?
                    .iter::<Dict<'_>>()
                    .next()
            });
            let columns = parms.as_ref().and_then(|p| number(p, "Columns"));
            let rows = parms.as_ref().and_then(|p| number(p, "Rows"));
            if columns.is_some_and(|c| !(1.0..=200_000.0).contains(&c))
                || rows.is_some_and(|r| !(0.0..=2_000_000.0).contains(&r))
            {
                self.found.add(
                    "implausible_fax_parameters",
                    Severity::Medium,
                    id,
                    Some(json!({"columns": columns, "rows": rows})),
                );
            }
        }
    }
}

/// Whether the page tree reaches a node twice.
fn cyclic_pages(pdf: &Pdf) -> bool {
    let xref = pdf.xref();
    let Some(root) = xref.get::<Dict<'_>>(xref.root_id()) else {
        return false;
    };
    let mut open: Vec<i32> = root
        .get_ref("Pages")
        .map(|r| r.obj_number)
        .into_iter()
        .collect();
    let mut seen = HashSet::new();
    while let Some(number) = open.pop() {
        if !seen.insert(number) {
            return true;
        }
        if seen.len() > 1_000_000 {
            return false;
        }
        let node = xref.get::<Dict<'_>>(hayro::hayro_syntax::object::ObjectIdentifier::new(
            number, 0,
        ));
        let kids = node.and_then(|n| n.get::<Array<'_>>("Kids"));
        for kid in kids.iter().flat_map(|k| k.raw_iter()) {
            open.extend(kid.as_obj_ref().map(|r| r.obj_number));
        }
    }
    false
}

/// Names in the raw bytes that stand for something `scan` reports.
const RAW_NAMES: [(&str, &str, Severity); 10] = [
    ("JavaScript", "javascript", Severity::High),
    ("Launch", "launch", Severity::High),
    ("OpenAction", "auto_action", Severity::Medium),
    ("XFA", "xfa", Severity::Medium),
    ("EmbeddedFile", "embedded_file", Severity::Medium),
    ("EmbeddedFiles", "embedded_file", Severity::Medium),
    ("RichMedia", "rich_media", Severity::High),
    ("SubmitForm", "submit_form", Severity::Medium),
    ("ImportData", "import_data", Severity::Medium),
    ("GoToR", "remote_goto", Severity::Low),
];

/// What the bytes say without any parsing.
#[derive(Default)]
struct Raw {
    header_offset: Option<usize>,
    revisions: usize,
    bytes_after_end: usize,
    /// How often each name of `RAW_NAMES` occurs, by position in that list.
    names: [usize; RAW_NAMES.len()],
    /// Names written with escapes they did not need: as written, and decoded.
    obfuscated: Vec<(String, String)>,
    obfuscated_total: usize,
    /// Object numbers defined more than once.
    redefined: usize,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn raw_facts(bytes: &[u8]) -> Raw {
    let mut raw = Raw {
        header_offset: find(&bytes[..bytes.len().min(1 << 20)], b"%PDF-"),
        ..Raw::default()
    };
    let mut ends = 0;
    let mut at = 0;
    while let Some(found) = find(&bytes[at..], b"%%EOF") {
        at += found + 5;
        ends = at;
        raw.revisions += 1;
    }
    if raw.revisions > 0 {
        raw.bytes_after_end = bytes[ends..]
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |last| last + 1);
    }

    // Stream data is not structure: a page of text about PDF may well say /JavaScript.
    let mut streams: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    while let Some(found) = find(&bytes[at..], b"stream") {
        let start = at + found + 6;
        at = start;
        let opens = bytes[..start - 6].ends_with(b"end").not()
            && matches!(bytes.get(start), Some(b'\r' | b'\n'));
        if opens && let Some(end) = find(&bytes[start..], b"endstream") {
            streams.push((start, start + end));
            at = start + end + 9;
        }
    }
    let mut stream = streams.iter().peekable();
    let regular = |b: u8| !b.is_ascii_whitespace() && !b"()<>[]{}/%".contains(&b) && b != 0;
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut i = 0;
    while i < bytes.len() {
        if let Some(&&(from, to)) = stream.peek()
            && i >= from
        {
            i = i.max(to);
            stream.next();
            continue;
        }
        if bytes[i] != b'/' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && end - start < 127 && regular(bytes[end]) {
            end += 1;
        }
        let written = &bytes[start..end];
        i = end.max(start);
        if written.is_empty() {
            continue;
        }
        let mut decoded = Vec::with_capacity(written.len());
        let mut needless = false;
        let mut k = 0;
        while k < written.len() {
            match (written[k], written.get(k + 1), written.get(k + 2)) {
                (b'#', Some(&a), Some(&b)) if hex(a).is_some() && hex(b).is_some() => {
                    let byte = hex(a).unwrap_or(0) * 16 + hex(b).unwrap_or(0);
                    needless |= byte.is_ascii_alphanumeric();
                    decoded.push(byte);
                    k += 3;
                }
                (byte, ..) => {
                    decoded.push(byte);
                    k += 1;
                }
            }
        }
        for (slot, (name, ..)) in RAW_NAMES.iter().enumerate() {
            if decoded == name.as_bytes() {
                raw.names[slot] += 1;
            }
        }
        // Binary data throws up stray escapes; a real name is plain text once decoded.
        if needless && decoded.iter().all(|b| b.is_ascii_graphic()) {
            raw.obfuscated_total += 1;
            if raw.obfuscated.len() < MAX_SAMPLES {
                let pair = (
                    String::from_utf8_lossy(written).into_owned(),
                    String::from_utf8_lossy(&decoded).into_owned(),
                );
                if !raw.obfuscated.contains(&pair) {
                    raw.obfuscated.push(pair);
                }
            }
        }
    }

    let definition =
        regex::bytes::Regex::new(r"(?-u)(?:^|[\r\n])[ \t]*(\d{1,10})[ \t]+(\d{1,5})[ \t]+obj\b")
            .expect("the pattern is valid");
    let mut defined = HashSet::new();
    for found in definition.captures_iter(bytes) {
        if !defined.insert((found[1].to_vec(), found[2].to_vec())) {
            raw.redefined += 1;
        }
    }
    raw
}

/// A stretch of text on a page that is extracted but not seen.
#[derive(Serialize, Deserialize)]
struct Hidden {
    page: u32,
    reason: String,
    bbox: [f64; 4],
    text: String,
}

/// The reason given to invisible text that lies over something visible.
const LAYER: &str = "invisible, over visible content";

/// Why each word of a page cannot be seen, judged from the rendered page.
///
/// A word that leaves no trace in the picture is hidden, whatever hides it: the
/// invisible text mode, the background's own colour, a covering shape, a clip.
fn hidden_on(page: &hayro::hayro_syntax::page::Page<'_>, number: u32) -> Vec<Hidden> {
    // The page's own content only, in the words and in the picture alike. Annotations
    // are left out of both: a highlight is drawn as a solid band here, and would make
    // every highlighted word look covered.
    let settings = InterpreterSettings {
        render_annotations: false,
        ..Default::default()
    };
    let scanned = layout::scan_with(page, false, true);
    if scanned.words.is_empty() {
        return Vec::new();
    }
    let (w, h) = page.render_dimensions();
    let scale = raster_scale(w, h, 108.0);
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &settings,
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
    let pixels = pixmap.data_as_u8_slice();
    // Whether anything with contrast shows in the middle band of the box, where letters have ink.
    let shows = |b: &[f64; 4]| {
        let band = (b[3] - b[1]) * 0.2;
        let px = |v: f64, max: usize| ((v * scale as f64).max(0.0) as usize).min(max);
        let x0 = px(b[0], width);
        let x1 = px(b[2], width).max(x0 + 1).min(width);
        let y0 = px(b[1] + band, height);
        let y1 = px(b[3] - band, height).max(y0 + 1).min(height);
        let (mut low, mut high) = ([255u8; 3], [0u8; 3]);
        for y in y0..y1 {
            for x in x0..x1 {
                let at = (y * width + x) * 4;
                for c in 0..3 {
                    low[c] = low[c].min(pixels[at + c]);
                    high[c] = high[c].max(pixels[at + c]);
                }
            }
        }
        (0..3).any(|c| high[c].saturating_sub(low[c]) >= 12)
    };

    let veiled = |b: &[f64; 4]| {
        scanned
            .veils
            .iter()
            .any(|v| v[0] < b[2] && b[0] < v[2] && v[1] < b[3] && b[1] < v[3])
    };
    let mut by_reason: BTreeMap<&'static str, Vec<layout::Word>> = BTreeMap::new();
    for word in &scanned.words {
        let b = &word.bbox;
        let letters = word.text.chars().any(char::is_alphanumeric);
        let reason = if b[2] <= 0.0 || b[3] <= 0.0 || b[0] >= w as f64 || b[1] >= h as f64 {
            "outside the page"
        } else if word.size < 1.5 {
            "too small to read"
        } else if word.invisible {
            if shows(b) { LAYER } else { "invisible" }
        // Punctuation alone has too little ink to judge by. Neither can a word be judged
        // whose font this renderer cannot draw, or one under something translucent, which
        // a viewer shows through whatever the picture here says.
        } else if letters && !word.blank && !veiled(b) && !shows(b) {
            "no contrast with what is around it"
        } else {
            continue;
        };
        by_reason.entry(reason).or_default().push(word.clone());
    }
    let mut hidden = Vec::new();
    for (reason, words) in by_reason {
        for line in layout::lines(&words) {
            let text: Vec<&str> = line.iter().map(|w| w.text.as_str()).collect();
            let bbox = line.iter().fold(line[0].bbox, |b, w| {
                [
                    b[0].min(w.bbox[0]),
                    b[1].min(w.bbox[1]),
                    b[2].max(w.bbox[2]),
                    b[3].max(w.bbox[3]),
                ]
            });
            hidden.push(Hidden {
                page: number,
                reason: reason.to_string(),
                bbox,
                text: text.join(" "),
            });
        }
    }
    hidden
}

/// What the search for hidden text came back with.
#[derive(Serialize, Deserialize, Default)]
struct HiddenReport {
    hidden: Vec<Hidden>,
    /// Pages the renderer could not get through.
    unreadable_pages: Vec<u32>,
}

/// Searches pages for hidden text, in this process.
fn hidden_text(
    bytes: Arc<Vec<u8>>,
    password: Option<&str>,
    pages: Option<&str>,
) -> Result<HiddenReport> {
    let pdf = Pdf::new_with_password(bytes, password.unwrap_or(""))
        .map_err(|_| anyhow!("the file does not open"))?;
    let all = pdf.pages();
    let mut report = HiddenReport::default();
    if all.is_empty() {
        return Ok(report);
    }
    let wanted = pagespec::parse_or_all(pages, all.len() as u32)?;
    let results: Vec<(u32, Option<Vec<Hidden>>)> = wanted
        .par_iter()
        .map(|&n| {
            // A page the renderer cannot survive is reported, not fatal.
            let page = catch_unwind(AssertUnwindSafe(|| hidden_on(&all[n as usize - 1], n)));
            (n, page.ok())
        })
        .collect();
    for (n, hidden) in results {
        match hidden {
            Some(hidden) => report.hidden.extend(hidden),
            None => report.unreadable_pages.push(n),
        }
    }
    Ok(report)
}

/// The search for hidden text as the one thing a process does: what `scan` starts
/// when it keeps the rendering away from itself.
pub fn hidden_text_call(args: Value) -> Result<Value> {
    let input = args["input"]
        .as_str()
        .ok_or_else(|| anyhow!("no input given"))?;
    let bytes = std::fs::read(input).map_err(|e| anyhow!("cannot read {input}: {e}"))?;
    let report = hidden_text(
        Arc::new(bytes),
        args["password"].as_str(),
        args["pages"].as_str(),
    )?;
    Ok(serde_json::to_value(report)?)
}

static ISOLATED: AtomicBool = AtomicBool::new(false);

/// Makes `scan` render pages in a process of its own from now on.
///
/// Rendering is where a file built to exhaust memory or time succeeds, and both
/// limits end the process. With the rendering elsewhere, such a file costs the
/// search for hidden text and is reported, instead of costing the whole answer.
/// Only the `pdfops` program can do this: it starts itself again.
pub fn isolate_rendering() {
    ISOLATED.store(true, Ordering::Relaxed);
}

/// Runs the search for hidden text in a child process. On failure, says what ran out.
fn hidden_text_isolated(
    input: &Path,
    password: Option<&str>,
    pages: Option<&str>,
) -> Result<HiddenReport, &'static str> {
    // The child must be done before this process's own time is up.
    let timeout = match limits::seconds_left() {
        Some(left) if left < 4 => return Err("time"),
        Some(left) => left - 2,
        None => 0,
    };
    let run = || -> Result<std::process::Output> {
        let mut child = Command::new(std::env::current_exe()?)
            .args([
                "--max-memory",
                &limits::max_memory().to_string(),
                "--timeout",
                &timeout.to_string(),
                "call",
                HIDDEN_TEXT_CALL,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let args = json!({"input": input, "password": password, "pages": pages});
        serde_json::to_writer(child.stdin.take().expect("stdin was piped"), &args)?;
        Ok(child.wait_with_output()?)
    };
    let out = run().map_err(|_| "a process to render in")?;
    if out.status.success() {
        return serde_json::from_slice(&out.stdout).map_err(|_| "a readable answer");
    }
    let said = String::from_utf8_lossy(&out.stderr);
    Err(if said.contains("memory limit") {
        "memory"
    } else if said.contains("time limit") {
        "time"
    } else {
        "a renderer that survives the file"
    })
}

/// The name under which the hidden `call` command runs `hidden_text_call`.
pub const HIDDEN_TEXT_CALL: &str = "scan_hidden_text";

/// Files what the search for hidden text found.
fn note_hidden(report: HiddenReport, found: &mut Findings) {
    for item in report.hidden {
        let characters = item.text.chars().count();
        let layer = item.reason == LAYER;
        let (kind, severity) = if layer {
            ("invisible_text_layer", Severity::Info)
        } else {
            ("hidden_text", Severity::Medium)
        };
        let sample = json!({
            "page": item.page,
            "reason": item.reason,
            "bbox": round_box(&item.bbox),
            "text": excerpt(&item.text, 300),
        });
        // A scanned book has a layer on every line; three lines show what it is like.
        let room = !layer || found.by_kind.get(kind).is_none_or(|f| f.samples.len() < 3);
        let finding = found.add(kind, severity, None, room.then_some(sample));
        let total = finding
            .more
            .get("characters")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        finding
            .more
            .insert("characters".into(), json!(total + characters as u64));
        let pages = finding.more.entry("pages").or_insert_with(|| json!([]));
        if let Some(list) = pages.as_array_mut()
            && list.last() != Some(&json!(item.page))
        {
            list.push(json!(item.page));
        }
    }
}

/// What an inspection established.
pub(crate) struct Inspection {
    pub(crate) found: Findings,
    structure: Value,
    /// Page count, when the file opened.
    pages: Option<u32>,
    /// False when the time allowed ran out before every object had been read.
    pub(crate) complete: bool,
}

/// Inspects a file's bytes and the objects that can be read from them.
pub(crate) fn inspect(bytes: Arc<Vec<u8>>, password: Option<&str>) -> Inspection {
    let mut found = Findings::default();
    let raw = raw_facts(&bytes);
    let mut structure = json!({
        "header_offset": raw.header_offset,
        "revisions": raw.revisions,
        "bytes_after_end": raw.bytes_after_end,
        "objects_defined_more_than_once": raw.redefined,
        "encrypted": doc::trailer_has_encrypt(&bytes),
    });
    match raw.header_offset {
        Some(0) => {}
        Some(offset) => {
            let severity = if offset < 1024 {
                Severity::Low
            } else {
                Severity::Medium
            };
            found.add(
                "header_not_at_start",
                severity,
                None,
                Some(json!({"offset": offset})),
            );
        }
        None => {
            found.add("header_not_at_start", Severity::Medium, None, None);
        }
    }
    if raw.bytes_after_end > 1024 {
        found.add(
            "trailing_data",
            Severity::Low,
            None,
            Some(json!({"bytes": raw.bytes_after_end})),
        );
    }
    if raw.redefined > 0 && raw.revisions <= 1 {
        found.add(
            "shadowed_objects",
            Severity::Low,
            None,
            Some(json!({"objects": raw.redefined})),
        );
    }
    for (written, decoded) in &raw.obfuscated {
        // Hiding a name that means something is deliberate; elsewhere it may be a quirk.
        let telling = RAW_NAMES.iter().any(|(name, ..)| name == decoded)
            || ["JS", "AA", "S", "URI", "Action", "Filter", "ObjStm"].contains(&decoded.as_str());
        let severity = if telling {
            Severity::High
        } else {
            Severity::Low
        };
        found.add(
            "obfuscated_name",
            severity,
            None,
            Some(json!({"written": format!("/{written}"), "means": format!("/{decoded}")})),
        );
    }
    if let Some(finding) = found.by_kind.get_mut("obfuscated_name") {
        finding.count = raw.obfuscated_total;
    }

    let mut pages = None;
    let mut complete = true;
    let opened = Pdf::new_with_password(bytes.clone(), password.unwrap_or(""));
    match &opened {
        Ok(pdf) => {
            let total = pdf.len();
            structure["objects"] = json!(total);
            structure["pages"] = json!(pdf.pages().len());
            pages = Some(pdf.pages().len() as u32);
            // Reading an object out of a packed group can cost as much as the whole group,
            // so a file can make this slow on purpose. A third of the time allowed is spent
            // here at most, and the result says how far that got.
            let deadline = limits::seconds_left()
                .map(|left| Instant::now() + Duration::from_secs((left / 3).max(1)));
            let mut read = 0usize;
            // Hostile files are exactly what this is pointed at; a parser panic is a finding.
            let walked = catch_unwind(AssertUnwindSafe(|| {
                let mut walker = Walker { found: &mut found };
                for object in pdf.objects() {
                    walker.walk(&object, None, 0);
                    read += 1;
                    if deadline.is_some_and(|d| Instant::now() > d) {
                        return None;
                    }
                }
                Some(cyclic_pages(pdf))
            }));
            match walked {
                Ok(Some(true)) => {
                    found.add("cyclic_structure", Severity::Medium, None, None);
                }
                Ok(Some(false)) => {}
                Ok(None) => {
                    complete = false;
                    structure["objects_read"] = json!(read);
                    found.add(
                        "resource_exhaustion",
                        Severity::Medium,
                        None,
                        Some(json!({
                            "while": "reading the objects",
                            "ran_out_of": "time",
                            "objects_read": read,
                            "objects": total,
                        })),
                    );
                }
                Err(_) => {
                    found.add("unparseable", Severity::Medium, None, None);
                }
            }
        }
        Err(LoadPdfError::Decryption(_)) => {
            found.add("encrypted_unread", Severity::Info, None, None);
        }
        Err(LoadPdfError::Invalid) => {
            found.add("unparseable", Severity::Medium, None, None);
        }
    }
    // A name the bytes carry but no readable object does: in an object the
    // cross-reference table leaves out, or in a file that did not open at all.
    for (slot, (name, kind, severity)) in RAW_NAMES.iter().enumerate() {
        if raw.names[slot] > 0 && !found.named.contains(kind) {
            let finding = found.add(
                kind,
                (*severity).min(Severity::Medium),
                None,
                Some(json!({"name_in_bytes": format!("/{name}"), "times": raw.names[slot]})),
            );
            finding.more.insert(
                "note".into(),
                json!("named in the file's bytes, not found among the objects that could be read"),
            );
        }
    }
    structure["parsed"] = json!(opened.is_ok());
    Inspection {
        found,
        structure,
        pages,
        complete,
    }
}

/// The signature name in a line of clamscan output, `path: Name FOUND`.
fn clam_signature(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (_, verdict) = line.trim().strip_suffix(" FOUND")?.rsplit_once(": ")?;
        Some(verdict.to_string())
    })
}

fn clamscan(path: &Path, found: &mut Findings) -> Value {
    let run = Command::new("clamscan")
        .args(["--no-summary", "--stdout"])
        .arg(path)
        .output();
    let Ok(out) = run else {
        return json!({"ran": false, "reason": "the clamscan program is not installed"});
    };
    let text = String::from_utf8_lossy(&out.stdout);
    match out.status.code() {
        Some(0) => json!({"ran": true, "recognised": false}),
        Some(1) => {
            let signature = clam_signature(&text);
            found.add(
                "known_malware",
                Severity::High,
                None,
                signature.clone().map(|s| json!({"signature": s})),
            );
            json!({"ran": true, "recognised": true, "signature": signature})
        }
        _ => {
            let reason = String::from_utf8_lossy(&out.stderr);
            json!({"ran": false, "reason": reason.lines().last().unwrap_or("clamscan failed").trim()})
        }
    }
}

pub fn scan(a: ScanArgs) -> Result<Value> {
    let bytes = Arc::new(
        std::fs::read(&a.input).map_err(|e| anyhow!("cannot read {}: {e}", a.input.display()))?,
    );
    let size = bytes.len();
    let password = a.password.as_deref();
    let mut inspection = inspect(bytes.clone(), password);

    let mut unreadable_pages = Vec::new();
    let mut hidden_text_checked = false;
    let total = inspection.pages.unwrap_or(0);
    if !a.skip_hidden_text && total > 0 {
        // A mistake in the page list is the caller's to hear about, whoever renders.
        pagespec::parse_or_all(a.pages.as_deref(), total)?;
        let outcome = if ISOLATED.load(Ordering::Relaxed) {
            hidden_text_isolated(&a.input, password, a.pages.as_deref())
        } else {
            catch_unwind(AssertUnwindSafe(|| {
                hidden_text(bytes, password, a.pages.as_deref())
            }))
            .map_err(|_| "a renderer that survives the file")
            .and_then(|report| report.map_err(|_| "a file that opens"))
        };
        match outcome {
            Ok(mut report) => {
                unreadable_pages = std::mem::take(&mut report.unreadable_pages);
                note_hidden(report, &mut inspection.found);
                hidden_text_checked = true;
            }
            Err(ran_out_of) => {
                inspection.found.add(
                    "resource_exhaustion",
                    Severity::Medium,
                    None,
                    Some(json!({
                        "while": "rendering the pages to look for hidden text",
                        "ran_out_of": ran_out_of,
                    })),
                );
            }
        }
    } else if !a.skip_hidden_text && inspection.pages == Some(0) {
        // No pages, no text to hide.
        hidden_text_checked = true;
    }
    let clam = a.clamav.then(|| clamscan(&a.input, &mut inspection.found));

    let mut checked = vec![
        "scripts and the actions that run them",
        "actions that fire on opening, page turns and form events",
        "actions that start programs, send or load form data, or open other files",
        "embedded files, with their type read from their first bytes",
        "XFA forms and rich media, 3D, movie, sound and screen annotations",
        "names disguised with escapes, streams packed repeatedly or with a filter that does not belong, streams that expand enormously",
        "oversized images, impossible fax parameters, a page tree that loops",
        "the header's position, data after the end, objects defined twice",
    ];
    let mut not_checked = vec![
        "whether embedded files, fonts or images exploit a flaw in some decoder",
        "what a script does when run",
        "where links lead beyond their address",
    ];
    if hidden_text_checked {
        checked.push(
            "text that leaves no trace in the rendered page: invisible, without contrast, covered by a flat shape, clipped, tiny or off the page",
        );
        not_checked.push("text hidden under a picture or pattern that is not a flat colour");
        not_checked.push("text inside annotations and form fields, and text they cover");
        not_checked.push("text under translucent or blended drawing, and text in fonts that cannot be drawn here");
    } else {
        not_checked.push("hidden text");
    }
    if !inspection.structure["parsed"].as_bool().unwrap_or(false) {
        not_checked.push("anything inside packed or encrypted parts, since the file did not open");
    }
    if !inspection.complete {
        not_checked.push("the objects that were not reached in the time allowed");
    }
    if clam.as_ref().is_none_or(|c| c["ran"] == false) {
        not_checked.push("known malware signatures");
    } else {
        checked.push("ClamAV signatures");
    }

    let found = &inspection.found;
    let mut result = json!({
        "file": a.input,
        "size_bytes": size,
        "verdict": if found.by_kind.is_empty() { "nothing found" } else { "findings" },
        "highest_severity": found.highest().map(Severity::name),
        "findings": found.report(),
        "structure": inspection.structure,
        "checked": checked,
        "not_checked": not_checked,
    });
    if !unreadable_pages.is_empty() {
        result["unreadable_pages"] = json!(unreadable_pages);
    }
    if let Some(clam) = clam {
        result["clamav"] = clam;
    }
    Ok(result)
}

/// The action type of a dictionary, if it is an action `remove` covers.
fn risk_of(dict: &Dictionary) -> Option<Risk> {
    let kind = dict.get(b"S").ok().and_then(|s| s.as_name().ok());
    match kind {
        Some(b"JavaScript") => Some(Risk::Javascript),
        Some(b"Launch" | b"SubmitForm" | b"ImportData" | b"GoToR" | b"GoToE") => {
            Some(Risk::Actions)
        }
        Some(b"Rendition" | b"Movie" | b"Sound" | b"RichMediaExecute" | b"GoTo3DView") => {
            Some(Risk::Media)
        }
        Some(b"URI") => {
            // A link that is a script or a file path in disguise.
            let uri = dict.get(b"URI").ok().and_then(|u| u.as_str().ok());
            let uri = String::from_utf8_lossy(uri.unwrap_or(b""))
                .trim_start()
                .to_lowercase();
            if uri.starts_with("javascript:") {
                Some(Risk::Javascript)
            } else if uri.starts_with("file:") || remote(&uri) && !uri.contains("://") {
                Some(Risk::Actions)
            } else {
                None
            }
        }
        _ if dict.has(b"JS") => Some(Risk::Javascript),
        _ => None,
    }
}

/// The group an annotation falls into, if `sanitize` removes annotations of its type.
fn annotation_risk(dict: &Dictionary) -> Option<Risk> {
    if !dict.has(b"Rect") {
        return None;
    }
    match dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) {
        Some(b"RichMedia" | b"3D" | b"Movie" | b"Sound" | b"Screen") => Some(Risk::Media),
        Some(b"FileAttachment") => Some(Risk::Attachments),
        _ => None,
    }
}

/// Takes the unwanted parts out of one object, in place.
struct Cleaner<'a> {
    remove: &'a HashSet<Risk>,
    /// Indirect objects that are unwanted actions or annotations, with their group.
    marked: &'a HashMap<ObjectId, Risk>,
    /// Indirect objects an /OpenAction may keep pointing at.
    harmless: &'a HashSet<ObjectId>,
    removed: HashMap<Risk, usize>,
}

impl Cleaner<'_> {
    /// The group `object` is removed for, when it is something unwanted.
    fn unwanted(&self, object: &Object) -> Option<Risk> {
        let risk = match object {
            Object::Reference(id) => self.marked.get(id).copied(),
            Object::Dictionary(dict) => risk_of(dict).or_else(|| annotation_risk(dict)),
            _ => None,
        };
        risk.filter(|r| self.remove.contains(r))
    }

    fn count(&mut self, risk: Risk) {
        *self.removed.entry(risk).or_default() += 1;
    }

    fn clean(&mut self, object: &mut Object) {
        match object {
            Object::Dictionary(dict) => self.dict(dict),
            Object::Stream(stream) => self.dict(&mut stream.dict),
            Object::Array(items) => {
                let mut gone = Vec::new();
                items.retain(|item| match self.unwanted(item) {
                    Some(risk) => {
                        gone.push(risk);
                        false
                    }
                    None => true,
                });
                for risk in gone {
                    self.count(risk);
                }
                for item in items {
                    self.clean(item);
                }
            }
            _ => {}
        }
    }

    fn dict(&mut self, dict: &mut Dictionary) {
        // Keys that exist only to hold what a group covers.
        let by_key: [(&[u8], Risk); 8] = [
            (b"JavaScript", Risk::Javascript),
            (b"AA", Risk::Actions),
            (b"XFA", Risk::Xfa),
            (b"EmbeddedFiles", Risk::Attachments),
            (b"EF", Risk::Attachments),
            (b"RF", Risk::Attachments),
            (b"AF", Risk::Attachments),
            (b"Collection", Risk::Attachments),
        ];
        for (key, risk) in by_key {
            if self.remove.contains(&risk) && dict.remove(key).is_some() {
                self.count(risk);
            }
        }
        if self.remove.contains(&Risk::Actions) {
            let stays = dict.get(b"OpenAction").ok().is_none_or(|open| match open {
                Object::Reference(id) => self.harmless.contains(id),
                direct => opens_harmlessly(direct),
            });
            if !stays {
                dict.remove(b"OpenAction");
                self.count(Risk::Actions);
            }
        }
        let unwanted: Vec<(Vec<u8>, Risk)> = dict
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), self.unwanted(value)?)))
            .collect();
        for (key, risk) in unwanted {
            dict.remove(&key);
            self.count(risk);
        }
        for (_, value) in dict.iter_mut() {
            self.clean(value);
        }
    }
}

pub fn sanitize(a: SanitizeArgs) -> Result<Value> {
    let remove: HashSet<Risk> = RISKS.into_iter().filter(|r| !a.keep.contains(r)).collect();
    if remove.is_empty() {
        bail!("every group is kept, so there is nothing to remove");
    }
    let mut d = doc::load(&a.input, a.password.as_deref())?;

    let dict_of = |object: &Object| match object {
        Object::Dictionary(dict) => Some(dict.clone()),
        Object::Stream(stream) => Some(stream.dict.clone()),
        _ => None,
    };
    let marked: HashMap<ObjectId, Risk> = d
        .objects
        .iter()
        .filter_map(|(id, object)| {
            let dict = dict_of(object)?;
            Some((*id, risk_of(&dict).or_else(|| annotation_risk(&dict))?))
        })
        .collect();
    // What an /OpenAction may point at and stay: a destination, or an action that only goes to one.
    let harmless: HashSet<ObjectId> = d
        .objects
        .iter()
        .filter(|(_, object)| opens_harmlessly(object))
        .map(|(id, _)| *id)
        .collect();
    let mut cleaner = Cleaner {
        remove: &remove,
        marked: &marked,
        harmless: &harmless,
        removed: HashMap::new(),
    };
    for object in d.objects.values_mut() {
        cleaner.clean(object);
    }
    let removed = cleaner.removed;
    // The order in which form scripts recalculate means nothing without the scripts.
    if remove.contains(&Risk::Javascript) {
        let form = d
            .catalog()
            .ok()
            .and_then(|c| c.get(b"AcroForm").ok())
            .and_then(|f| f.as_reference().ok());
        if let Some(form) = form.and_then(|id| d.get_dictionary_mut(id).ok()) {
            form.remove(b"CO");
        }
    }
    // Unreferenced objects go too: they are the scripts and files themselves.
    prune(&mut d);
    doc::seal(&mut d)?;
    let mut bytes = Vec::new();
    d.save_to(&mut bytes)?;
    let bytes = Arc::new(bytes);

    // Proof before delivery: the result is inspected the way any other file would be.
    let check = inspect(bytes.clone(), a.password.as_deref());
    if !check.complete || check.pages.is_none() {
        bail!("the cleaned file could not be checked; nothing was written");
    }
    let mut left: Vec<&str> = remove
        .iter()
        .flat_map(|risk| risk.kinds())
        .filter(|kind| check.found.has(kind))
        .copied()
        .collect();
    left.sort_unstable();
    if !left.is_empty() {
        bail!(
            "the cleaned file would still contain {}; nothing was written",
            left.join(", ")
        );
    }
    if !a.dry_run {
        doc::write_atomic(&a.output, &bytes)?;
    }
    let counts: Map<String, Value> = RISKS
        .iter()
        .filter(|risk| remove.contains(risk))
        .map(|risk| {
            (
                risk.name().to_string(),
                json!(removed.get(risk).copied().unwrap_or(0)),
            )
        })
        .collect();
    Ok(json!({
        "output": a.output,
        "dry_run": a.dry_run,
        "removed": counts,
        "kept": a.keep.iter().map(|risk| risk.name()).collect::<Vec<_>>(),
        "verified": true,
        "remaining": check.found.report(),
        "size_bytes": bytes.len(),
    }))
}

/// Whether an object is fine as the target of /OpenAction.
fn opens_harmlessly(object: &Object) -> bool {
    match object {
        Object::Dictionary(dict) => dict
            .get(b"S")
            .ok()
            .and_then(|s| s.as_name().ok())
            .is_some_and(|s| s == b"GoTo"),
        Object::Array(_) | Object::Name(_) | Object::String(..) => true,
        _ => false,
    }
}
