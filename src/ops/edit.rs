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

#[derive(ValueEnum, Deserialize, JsonSchema, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
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
    pub text: Option<String>,
    /// PNG or JPEG file to draw instead of text, e.g. a signature scan or a company seal; PNG transparency is kept
    #[arg(long)]
    pub image: Option<PathBuf>,
    /// Content of a QR code to draw instead of text, e.g. a URL
    #[arg(long)]
    pub qr: Option<String>,
    /// Where text is drawn (default: watermark)
    #[arg(long, value_enum)]
    pub position: Option<StampPosition>,
    /// Corner or centre an image or QR code is placed at (default: bottom-right)
    #[arg(long, value_enum)]
    pub anchor: Option<Anchor>,
    /// Left edge of an image or QR code in points from the page's left edge; with y, overrides anchor
    #[arg(long)]
    pub x: Option<f64>,
    /// Top edge of an image or QR code in points from the page's top edge, as reported by layout
    #[arg(long)]
    pub y: Option<f64>,
    /// Width of an image or QR code in points; height follows the aspect ratio (default: 120, QR 80)
    #[arg(long)]
    pub width: Option<f64>,
    /// Distance from the page edges when placing by anchor, in points (default: 24)
    #[arg(long)]
    pub margin: Option<f64>,
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
    /// Report what would change without writing the output file
    #[arg(long)]
    #[serde(default)]
    pub dry_run: bool,
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
    // Readers expect an encrypted file to carry an identifier.
    doc::ensure_id(&mut d)?;
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
    // Object streams would wrap already encrypted objects, so a protected file keeps the classic layout.
    if doc::seal(&mut d)? {
        d.save_to(&mut buf)?;
    } else {
        d.save_modern(&mut buf)?;
    }
    // Never hand back a larger file than the one we were given.
    let smaller = (buf.len() as u64) < before;
    if smaller {
        doc::write_atomic(&a.output, &buf)?;
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
pub(crate) fn visual_space(page_box: [f64; 4], rotation: i64) -> ([f64; 6], f64, f64) {
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

/// Appends drawing operators to a page, isolated from whatever state its content leaves behind.
///
/// `open` is a stream holding just `q`; it goes in front so that the page's own
/// content can be closed with `Q` before `body` runs.
pub(crate) fn append_content(
    d: &mut Document,
    page: ObjectId,
    open: ObjectId,
    body: String,
    resources: Dictionary,
) -> Result<()> {
    let stream = d.add_object(Stream::new(
        Dictionary::new(),
        format!("Q\n{body}").into_bytes(),
    ));
    let dict = d.get_dictionary(page)?;
    let mut contents = vec![Object::Reference(open)];
    match dict.get(b"Contents").map(|c| (c, doc::resolve(d, c))) {
        Ok((_, Object::Array(items))) => contents.extend(items.iter().cloned()),
        Ok((r @ Object::Reference(_), _)) => contents.push(r.clone()),
        _ => {}
    }
    contents.push(Object::Reference(stream));
    let dict = d.get_dictionary_mut(page)?;
    dict.set("Contents", contents);
    dict.set("Resources", resources);
    Ok(())
}

/// The page's resources as a private dictionary that may be extended freely.
pub(crate) fn own_resources(d: &Document, page: ObjectId) -> Dictionary {
    doc::inherited(d, page, b"Resources")
        .and_then(|o| o.as_dict().ok())
        .cloned()
        .unwrap_or_default()
}

/// Adds an image file as an XObject and returns it with its height-to-width ratio.
pub(crate) fn embed_image(d: &mut Document, path: &std::path::Path) -> Result<(ObjectId, f64)> {
    use image::ImageDecoder;
    let bytes = std::fs::read(path).map_err(|e| anyhow!("cannot read {}: {e}", path.display()))?;
    let unsupported = || anyhow!("{} is not a PNG or JPEG image", path.display());
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("BitsPerComponent", 8);

    if image::guess_format(&bytes).map_err(|_| unsupported())? == image::ImageFormat::Jpeg {
        let decoder = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(&bytes))
            .map_err(|_| unsupported())?;
        let (w, h) = decoder.dimensions();
        let space = match decoder.color_type() {
            image::ColorType::L8 => Some("DeviceGray"),
            image::ColorType::Rgb8 => Some("DeviceRGB"),
            _ => None,
        };
        // A plain grey or RGB JPEG is a valid image stream as it is: no recompression, no loss.
        if let Some(space) = space {
            dict.set("Width", w as i64);
            dict.set("Height", h as i64);
            dict.set("ColorSpace", Object::Name(space.as_bytes().to_vec()));
            dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
            let id = d.add_object(Stream::new(dict, bytes).with_compression(false));
            return Ok((id, h as f64 / w.max(1) as f64));
        }
    }
    let rgba = image::load_from_memory(&bytes)
        .map_err(|_| unsupported())?
        .to_rgba8();
    let (w, h) = rgba.dimensions();
    let rgb: Vec<u8> = rgba.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let alpha: Vec<u8> = rgba.pixels().map(|p| p[3]).collect();
    dict.set("Width", w as i64);
    dict.set("Height", h as i64);
    if alpha.iter().any(|&a| a != 255) {
        let mut mask = dict.clone();
        mask.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
        let mut mask = Stream::new(mask, alpha);
        let _ = mask.compress();
        let mask = d.add_object(mask);
        dict.set("SMask", mask);
    }
    dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    let mut stream = Stream::new(dict, rgb);
    let _ = stream.compress();
    Ok((d.add_object(stream), h as f64 / w.max(1) as f64))
}

/// Path operators filling the dark modules of a QR code inside the square at (x, y) of side `side`.
fn qr_ops(content: &str, x: f64, y: f64, side: f64) -> Result<String> {
    let code = qrcode::QrCode::new(content.as_bytes())
        .map_err(|e| anyhow!("cannot encode QR code: {e}"))?;
    let n = code.width();
    let colors = code.to_colors();
    // Scanners need a blank margin of four modules around the code.
    let module = side / (n + 8) as f64;
    let mut ops = format!("1 1 1 rg\n{x:.2} {y:.2} {side:.2} {side:.2} re f\n0 0 0 rg\n");
    for row in 0..n {
        let mut col = 0;
        while col < n {
            // Consecutive dark modules of a row become one rectangle.
            let run = (col..n)
                .take_while(|&c| colors[row * n + c] == qrcode::Color::Dark)
                .count();
            if run > 0 {
                ops += &format!(
                    "{:.3} {:.3} {:.3} {:.3} re\n",
                    x + (4 + col) as f64 * module,
                    y + side - (5 + row) as f64 * module,
                    run as f64 * module,
                    module
                );
            }
            col += run.max(1);
        }
    }
    Ok(ops + "f\n")
}

/// What a stamp draws.
enum Mark {
    Text(TextFont, String),
    Image(ObjectId, f64),
    Qr(String),
}

pub fn stamp(a: StampArgs) -> Result<Value> {
    if [a.text.is_some(), a.image.is_some(), a.qr.is_some()]
        .iter()
        .filter(|given| **given)
        .count()
        != 1
    {
        bail!("give exactly one of text, image or qr");
    }
    let position = a.position.unwrap_or(StampPosition::Watermark);
    let align = a.align.unwrap_or(Align::Center);
    let watermark = a.text.is_some() && position == StampPosition::Watermark;
    let opacity = a.opacity.unwrap_or(if watermark { 0.25 } else { 1.0 });
    if !(0.0..=1.0).contains(&opacity) {
        bail!("opacity must be between 0 and 1");
    }
    if a.size.is_some_and(|s| s <= 0.0) || a.width.is_some_and(|w| w <= 0.0) {
        bail!("size and width must be positive");
    }
    if a.x.is_some() != a.y.is_some() {
        bail!("give both x and y, or neither");
    }
    let [r, g, b] = parse_color(a.color.as_deref().unwrap_or("000000"))?;

    let mut d = doc::load(&a.input, a.password.as_deref())?;
    let ids = doc::page_ids(&d);
    let total = ids.len() as u32;
    let pages = pagespec::parse_or_all(a.pages.as_deref(), total)?;

    let mark = if let Some(text) = &a.text {
        // Page numbers are substituted per page, so the font must cover digits too.
        Mark::Text(
            TextFont::new(&mut d, &format!("{text}0123456789"), a.font.as_deref())?,
            text.clone(),
        )
    } else if let Some(path) = &a.image {
        let (id, ratio) = embed_image(&mut d, path)?;
        Mark::Image(id, ratio)
    } else {
        Mark::Qr(a.qr.clone().unwrap_or_default())
    };
    let mut gs = Dictionary::new();
    gs.set("Type", Object::Name(b"ExtGState".to_vec()));
    gs.set("ca", opacity as f32);
    let gs_id = d.add_object(gs);
    // Wrapping the existing content in q/Q keeps its graphics state from leaking into the stamp.
    let open_id = d.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));

    let margin = a.margin.unwrap_or(24.0);
    let mut unique = pages.clone();
    unique.sort_unstable();
    unique.dedup();
    for &n in &unique {
        let id = ids[n as usize - 1];
        let (m, vw, vh) = visual_space(doc::page_box(&d, id), doc::rotation(&d, id));
        let mut res = own_resources(&d, id);
        let gs_name = add_resource(&d, &mut res, "ExtGState", "PdfopsGS", gs_id);
        let mut ops = format!(
            "q\n{} {} {} {} {} {} cm\n/{gs_name} gs\n",
            m[0], m[1], m[2], m[3], m[4], m[5]
        );

        // Images and QR codes: a box placed by anchor or by its top-left corner, in upright page space.
        let place = |ratio: f64, default_width: f64| {
            let width = a.width.unwrap_or(default_width);
            let height = width * ratio;
            let (x, top) = match (a.x, a.y, a.anchor.unwrap_or(Anchor::BottomRight)) {
                (Some(x), Some(y), _) => (x, y),
                (_, _, Anchor::TopLeft) => (margin, margin),
                (_, _, Anchor::TopRight) => (vw - margin - width, margin),
                (_, _, Anchor::BottomLeft) => (margin, vh - margin - height),
                (_, _, Anchor::BottomRight) => (vw - margin - width, vh - margin - height),
                (_, _, Anchor::Center) => ((vw - width) / 2.0, (vh - height) / 2.0),
            };
            // Content space has its origin at the bottom-left.
            (x, vh - top - height, width, height)
        };
        match &mark {
            Mark::Image(image, ratio) => {
                let name = add_resource(&d, &mut res, "XObject", "PdfopsIm", *image);
                let (x, y, width, height) = place(*ratio, 120.0);
                ops += &format!("{width:.2} 0 0 {height:.2} {x:.2} {y:.2} cm\n/{name} Do\n");
            }
            Mark::Qr(content) => {
                let (x, y, side, _) = place(1.0, 80.0);
                ops += &qr_ops(content, x, y, side)?;
            }
            Mark::Text(font, template) => {
                let font_name = add_resource(&d, &mut res, "Font", "PdfopsF", font.id);
                let text = template
                    .replace("{pages}", &total.to_string())
                    .replace("{page}", &n.to_string());
                let size = if watermark {
                    // Fit the text to 70% of the diagonal unless a size was requested.
                    let size = a.size.unwrap_or_else(|| {
                        (0.7 * vw.hypot(vh) / font.width(&text, 1.0).max(0.001)).clamp(8.0, 144.0)
                    });
                    let (sin, cos) = (vh / vw).atan().sin_cos();
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
                    "/{font_name} {size:.2} Tf\n{r:.3} {g:.3} {b:.3} rg\n{}\nET\n",
                    font.show(&text, size)
                );
            }
        }
        append_content(&mut d, id, open_id, ops + "Q\n", res)?;
    }
    let size = doc::save_unless(a.dry_run, &mut d, &a.output)?;
    Ok(json!({
        "output": a.output,
        "dry_run": a.dry_run,
        "stamped_pages": unique,
        "size_bytes": size,
    }))
}
