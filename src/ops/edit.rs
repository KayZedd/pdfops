//! Commands that modify a document in place: rotate, set-meta, encrypt, decrypt, compress, stamp.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use clap::{Args, ValueEnum};
use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
use lopdf::{
    Dictionary, Document, EncryptionState, EncryptionVersion, Object, ObjectId, Permissions, Stream,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::font::TextFont;
use crate::ops::lossy;
use crate::{doc, pagespec};

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct RotateArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Where to write the result (may be the input file)
    #[arg(short, long)]
    pub output: PathBuf,
    /// Clockwise degrees added to the current rotation: a multiple of 90, may be negative
    #[arg(short, long, allow_hyphen_values = true)]
    pub angle: i64,
    /// Pages to rotate, e.g. "1,4-6" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct SetMetaArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Where to write the result (may be the input file)
    #[arg(short, long)]
    pub output: PathBuf,
    /// New title; an empty string removes the field
    #[arg(long)]
    pub title: Option<String>,
    /// New author; an empty string removes the field
    #[arg(long)]
    pub author: Option<String>,
    /// New subject; an empty string removes the field
    #[arg(long)]
    pub subject: Option<String>,
    /// New keywords; an empty string removes the field
    #[arg(long)]
    pub keywords: Option<String>,
    /// New creator; an empty string removes the field
    #[arg(long)]
    pub creator: Option<String>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct EncryptArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Where to write the encrypted PDF
    #[arg(short, long)]
    pub output: PathBuf,
    /// Password needed to open the document (default: none, opens freely with restrictions)
    #[arg(long)]
    pub user_password: Option<String>,
    /// Password that grants full control
    #[arg(long)]
    pub owner_password: String,
    /// Forbid printing
    #[arg(long)]
    #[serde(default)]
    pub deny_print: bool,
    /// Forbid copying text and images
    #[arg(long)]
    #[serde(default)]
    pub deny_copy: bool,
    /// Forbid editing, annotating and form filling
    #[arg(long)]
    #[serde(default)]
    pub deny_modify: bool,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct DecryptArgs {
    /// Encrypted PDF file
    pub input: PathBuf,
    /// Where to write the decrypted PDF
    #[arg(short, long)]
    pub output: PathBuf,
    /// User or owner password (default: empty)
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct CompressArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Where to write the result (may be the input file)
    #[arg(short, long)]
    pub output: PathBuf,
    /// Lossy: re-encode photos as JPEG at this quality, 1-100 (default: off, or 75 when a maximum edge is given)
    #[arg(long)]
    pub image_quality: Option<u8>,
    /// Lossy: downscale images so neither side exceeds this many pixels (default: off)
    #[arg(long)]
    pub max_image_edge: Option<u32>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(ValueEnum, Deserialize, JsonSchema, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StampPosition {
    /// Large diagonal text across the page
    Watermark,
    /// Top margin
    Header,
    /// Bottom margin
    Footer,
}

#[derive(ValueEnum, Deserialize, JsonSchema, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct StampArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Where to write the result (may be the input file)
    #[arg(short, long)]
    pub output: PathBuf,
    /// Text to draw; {page} and {pages} expand to the page number and page count
    #[arg(short, long)]
    pub text: String,
    /// Where to draw it (default: watermark)
    #[arg(long, value_enum)]
    pub position: Option<StampPosition>,
    /// Horizontal alignment for header and footer (default: center)
    #[arg(long, value_enum)]
    pub align: Option<Align>,
    /// Font size in points (default: 10, or fitted to the page for a watermark)
    #[arg(long)]
    pub size: Option<f64>,
    /// Opacity from 0 to 1 (default: 0.25 for a watermark, otherwise 1)
    #[arg(long)]
    pub opacity: Option<f64>,
    /// Text colour as RRGGBB hex (default: 000000)
    #[arg(long)]
    pub color: Option<String>,
    /// TrueType or OpenType font file to embed (default: Helvetica, or an installed font when the text needs one)
    #[arg(long)]
    pub font: Option<PathBuf>,
    /// Pages to stamp, e.g. "2-" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

pub fn rotate(a: RotateArgs) -> Result<Value> {
    if a.angle % 90 != 0 {
        bail!("angle must be a multiple of 90, got {}", a.angle);
    }
    let mut d = doc::load(&a.input, a.password.as_deref())?;
    let ids = doc::page_ids(&d);
    let pages = pagespec::parse_or_all(a.pages.as_deref(), ids.len() as u32)?;
    for &n in &pages {
        let id = ids[n as usize - 1];
        let angle = (doc::rotation(&d, id) + a.angle).rem_euclid(360);
        d.get_dictionary_mut(id)?.set("Rotate", angle);
    }
    let size = doc::save(&mut d, &a.output)?;
    Ok(json!({"output": a.output, "rotated_pages": pages, "angle": a.angle, "size_bytes": size}))
}

pub fn set_meta(a: SetMetaArgs) -> Result<Value> {
    let fields = [
        ("Title", &a.title),
        ("Author", &a.author),
        ("Subject", &a.subject),
        ("Keywords", &a.keywords),
        ("Creator", &a.creator),
    ];
    if fields.iter().all(|(_, v)| v.is_none()) {
        bail!("nothing to set: give at least one of title, author, subject, keywords, creator");
    }
    let mut d = doc::load(&a.input, a.password.as_deref())?;
    let info_id = doc::ensure_indirect_dict(&mut d, None, b"Info")?;
    let info = d.get_dictionary_mut(info_id)?;
    let mut changed = Vec::new();
    for (key, value) in fields {
        let Some(value) = value else { continue };
        if value.is_empty() {
            info.remove(key.as_bytes());
        } else {
            info.set(key, lopdf::text_string(value));
        }
        changed.push(key.to_lowercase());
    }
    // An XMP packet would keep the old values and viewers prefer it over the info dictionary.
    d.catalog_mut()?.remove(b"Metadata");
    let size = doc::save(&mut d, &a.output)?;
    Ok(json!({"output": a.output, "changed": changed, "size_bytes": size}))
}

pub fn encrypt(a: EncryptArgs) -> Result<Value> {
    if a.owner_password.is_empty() {
        bail!("owner password must not be empty");
    }
    let mut d = doc::load(&a.input, None)?;
    if d.was_encrypted() {
        bail!(
            "{} is already encrypted: decrypt it first",
            a.input.display()
        );
    }
    let mut permissions = Permissions::all();
    if a.deny_print {
        permissions.remove(Permissions::PRINTABLE | Permissions::PRINTABLE_IN_HIGH_QUALITY);
    }
    if a.deny_copy {
        permissions.remove(Permissions::COPYABLE);
    }
    if a.deny_modify {
        permissions.remove(
            Permissions::MODIFIABLE
                | Permissions::ANNOTABLE
                | Permissions::FILLABLE
                | Permissions::ASSEMBLABLE,
        );
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| anyhow!("no system randomness: {e}"))?;
    let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
    let user_password = a.user_password.unwrap_or_default();
    let state = EncryptionState::try_from(EncryptionVersion::V5 {
        encrypt_metadata: true,
        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
        file_encryption_key: &key,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: &a.owner_password,
        user_password: &user_password,
        permissions,
    })?;
    // AES-256 is a PDF 2.0 security handler.
    d.version = "2.0".to_string();
    d.encrypt(&state)?;
    let size = doc::save(&mut d, &a.output)?;
    Ok(json!({
        "output": a.output,
        "algorithm": "AES-256",
        "requires_password_to_open": !user_password.is_empty(),
        "size_bytes": size,
    }))
}

pub fn decrypt(a: DecryptArgs) -> Result<Value> {
    let mut d = doc::load(&a.input, Some(a.password.as_deref().unwrap_or("")))?;
    if !d.was_encrypted() {
        bail!("{} is not encrypted", a.input.display());
    }
    d.encryption_state = None;
    let size = doc::save(&mut d, &a.output)?;
    Ok(json!({"output": a.output, "size_bytes": size}))
}

/// Removes objects that nothing reachable from the trailer refers to.
pub fn prune(d: &mut Document) -> usize {
    fn refs(obj: &Object, out: &mut Vec<ObjectId>) {
        match obj {
            Object::Reference(id) => out.push(*id),
            Object::Array(a) => a.iter().for_each(|o| refs(o, out)),
            Object::Dictionary(dict) => dict.iter().for_each(|(_, o)| refs(o, out)),
            Object::Stream(s) => s.dict.iter().for_each(|(_, o)| refs(o, out)),
            _ => {}
        }
    }
    let mut live = HashSet::new();
    let mut queue = Vec::new();
    d.trailer.iter().for_each(|(_, o)| refs(o, &mut queue));
    while let Some(id) = queue.pop() {
        if live.insert(id)
            && let Some(obj) = d.objects.get(&id)
        {
            refs(obj, &mut queue);
        }
    }
    let before = d.objects.len();
    d.objects.retain(|id, _| live.contains(id));
    before - d.objects.len()
}

pub fn compress(a: CompressArgs) -> Result<Value> {
    let before = std::fs::metadata(&a.input)?.len();
    let mut d = doc::load(&a.input, a.password.as_deref())?;
    let removed = prune(&mut d);
    let lossy = a.image_quality.is_some() || a.max_image_edge.is_some();
    let quality = a.image_quality.unwrap_or(75);
    if !(1..=100).contains(&quality) {
        bail!("image quality must be between 1 and 100");
    }
    if a.max_image_edge == Some(0) {
        bail!("maximum image edge must be at least 1");
    }
    let recompressed = if lossy {
        lossy::recompress_images(&mut d, quality, a.max_image_edge)
    } else {
        0
    };
    d.compress();
    let mut buf = Vec::new();
    d.save_modern(&mut buf)?;
    // Never hand back a larger file than the one we were given.
    let smaller = (buf.len() as u64) < before;
    if smaller {
        write_atomic(&a.output, &buf)?;
    } else if a.output != a.input {
        std::fs::copy(&a.input, &a.output)?;
    }
    let after = std::fs::metadata(&a.output)?.len();
    Ok(json!({
        "output": a.output,
        "size_before": before,
        "size_after": after,
        "saved_percent": ((before.saturating_sub(after)) as f64 * 1000.0 / before.max(1) as f64).round() / 10.0,
        "unused_objects_removed": removed,
        "images_recompressed": if smaller { recompressed } else { 0 },
        "rewritten": smaller,
    }))
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn parse_color(hex: &str) -> Result<[f64; 3]> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        bail!("colour must be RRGGBB hex, got '{hex}'");
    }
    let mut rgb = [0.0; 3];
    for (i, c) in rgb.iter_mut().enumerate() {
        *c = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)? as f64 / 255.0;
    }
    Ok(rgb)
}

/// Matrix taking upright visual coordinates to page space, and the visual page size.
fn visual_space(page_box: [f64; 4], rotation: i64) -> ([f64; 6], f64, f64) {
    let [x0, y0, x1, y1] = page_box;
    let (w, h) = (x1 - x0, y1 - y0);
    match rotation {
        90 => ([0.0, 1.0, -1.0, 0.0, x0 + w, y0], h, w),
        180 => ([-1.0, 0.0, 0.0, -1.0, x0 + w, y0 + h], w, h),
        270 => ([0.0, -1.0, 1.0, 0.0, x0, y0 + h], h, w),
        _ => ([1.0, 0.0, 0.0, 1.0, x0, y0], w, h),
    }
}

/// Adds `target` to a private copy of the `category` resources and returns the name it got.
///
/// The name is chosen to be unused, so stamping an already stamped page does not
/// redirect the resources its earlier stamps refer to.
fn add_resource(
    d: &Document,
    res: &mut Dictionary,
    category: &str,
    base: &str,
    target: ObjectId,
) -> String {
    let mut sub = res
        .get(category.as_bytes())
        .ok()
        .and_then(|o| doc::resolve(d, o).as_dict().ok())
        .cloned()
        .unwrap_or_default();
    let name = (1..)
        .map(|i| {
            if i == 1 {
                base.to_string()
            } else {
                format!("{base}{i}")
            }
        })
        .find(|n| !sub.has(n.as_bytes()))
        .expect("an unbounded range always yields a free name");
    sub.set(name.as_str(), target);
    res.set(category, sub);
    name
}

pub fn stamp(a: StampArgs) -> Result<Value> {
    let position = a.position.unwrap_or(StampPosition::Watermark);
    let align = a.align.unwrap_or(Align::Center);
    let watermark = position == StampPosition::Watermark;
    let opacity = a.opacity.unwrap_or(if watermark { 0.25 } else { 1.0 });
    if !(0.0..=1.0).contains(&opacity) {
        bail!("opacity must be between 0 and 1");
    }
    if a.size.is_some_and(|s| s <= 0.0) {
        bail!("size must be positive");
    }
    let [r, g, b] = parse_color(a.color.as_deref().unwrap_or("000000"))?;

    let mut d = doc::load(&a.input, a.password.as_deref())?;
    let ids = doc::page_ids(&d);
    let total = ids.len() as u32;
    let pages = pagespec::parse_or_all(a.pages.as_deref(), total)?;

    // Page numbers are substituted per page, so the font must cover digits too.
    let font = TextFont::new(&mut d, &format!("{}0123456789", a.text), a.font.as_deref())?;
    let font_id = font.id;
    let mut gs = Dictionary::new();
    gs.set("Type", Object::Name(b"ExtGState".to_vec()));
    gs.set("ca", opacity as f32);
    let gs_id = d.add_object(gs);
    // Wrapping the existing content in q/Q keeps its graphics state from leaking into the stamp.
    let open_id = d.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));

    let margin = 24.0;
    let mut unique = pages.clone();
    unique.sort_unstable();
    unique.dedup();
    for &n in &unique {
        let id = ids[n as usize - 1];
        let text = a
            .text
            .replace("{pages}", &total.to_string())
            .replace("{page}", &n.to_string());
        let (m, vw, vh) = visual_space(doc::page_box(&d, id), doc::rotation(&d, id));

        let mut res = doc::inherited(&d, id, b"Resources")
            .and_then(|o| o.as_dict().ok())
            .cloned()
            .unwrap_or_default();
        let font_name = add_resource(&d, &mut res, "Font", "PdfopsF", font_id);
        let gs_name = add_resource(&d, &mut res, "ExtGState", "PdfopsGS", gs_id);

        let mut ops = format!(
            "Q\nq\n{} {} {} {} {} {} cm\n/{gs_name} gs\n",
            m[0], m[1], m[2], m[3], m[4], m[5]
        );
        let size = if watermark {
            // Fit the text to 70% of the diagonal unless a size was requested.
            let size = a.size.unwrap_or_else(|| {
                (0.7 * vw.hypot(vh) / font.width(&text, 1.0).max(0.001)).clamp(8.0, 144.0)
            });
            let angle = (vh / vw).atan();
            let (sin, cos) = angle.sin_cos();
            ops += &format!(
                "{cos:.5} {sin:.5} {:.5} {cos:.5} {:.2} {:.2} cm\n",
                -sin,
                vw / 2.0,
                vh / 2.0
            );
            ops += &format!(
                "BT\n1 0 0 1 {:.2} {:.2} Tm\n",
                -font.width(&text, size) / 2.0,
                -size * 0.35
            );
            size
        } else {
            let size = a.size.unwrap_or(10.0);
            let width = font.width(&text, size);
            let x = match align {
                Align::Left => margin,
                Align::Center => (vw - width) / 2.0,
                Align::Right => vw - margin - width,
            };
            let y = if position == StampPosition::Header {
                vh - margin - size * 0.72
            } else {
                margin
            };
            ops += &format!("BT\n1 0 0 1 {x:.2} {y:.2} Tm\n");
            size
        };
        ops += &format!(
            "/{font_name} {size:.2} Tf\n{r:.3} {g:.3} {b:.3} rg\n{} Tj\nET\nQ\n",
            font.encode(&text)
        );
        let stream_id = d.add_object(Stream::new(Dictionary::new(), ops.into_bytes()));

        let page = d.get_dictionary(id)?;
        let mut contents = vec![Object::Reference(open_id)];
        match page.get(b"Contents").map(|c| (c, doc::resolve(&d, c))) {
            Ok((_, Object::Array(items))) => contents.extend(items.iter().cloned()),
            Ok((r @ Object::Reference(_), _)) => contents.push(r.clone()),
            _ => {}
        }
        contents.push(Object::Reference(stream_id));
        let page = d.get_dictionary_mut(id)?;
        page.set("Contents", contents);
        page.set("Resources", res);
    }
    let size = doc::save(&mut d, &a.output)?;
    Ok(json!({"output": a.output, "stamped_pages": unique, "size_bytes": size}))
}
