//! Redaction: content under the given areas is removed from the file, not just covered.
//!
//! Two interpreters cooperate. hayro, which renders the page, says where every
//! glyph lands; this module walks the same content stream with lopdf and deletes
//! the glyphs, vector paths, image pixels and annotations inside the areas. The
//! result is then scanned again, and nothing is written unless the areas are empty.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Args;
use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, SoftMask, interpret_page,
};
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::content::TypedIter;
use hayro::hayro_syntax::content::ops::TypedInstruction;
use hayro::hayro_syntax::object::stream::{ImageColorSpace, ImageDecodeParams};
use hayro::hayro_syntax::object::{ObjectIdentifier, Stream as LazyStream};
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{Affine, BezPath, Point, Rect};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::font::TextFont;
use crate::ops::edit::{append_content, own_resources, prune, visual_space};
use crate::ops::layout;
use crate::progress::Progress;
use crate::{doc, pagespec};

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct RedactArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Where to write the redacted PDF (may be the input file)
    #[arg(short, long)]
    pub output: PathBuf,
    /// Area to redact as "page:x0,y0,x1,y1" in the coordinates layout reports, e.g. "2:100,200,300,220"
    #[arg(long = "rect")]
    #[serde(default)]
    pub rects: Vec<String>,
    /// Text to find and redact wherever it occurs, e.g. a name or an ID number
    #[arg(long = "text")]
    #[serde(default)]
    pub texts: Vec<String>,
    /// Treat the texts as regular expressions
    #[arg(long)]
    #[serde(default)]
    pub regex: bool,
    /// Match case exactly (default: case-insensitive)
    #[arg(long)]
    #[serde(default)]
    pub case_sensitive: bool,
    /// Pages searched for the texts, e.g. "1-5" (default: all)
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

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct ReplaceArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Where to write the result (may be the input file)
    #[arg(short, long)]
    pub output: PathBuf,
    /// Text to find
    #[arg(long)]
    pub find: String,
    /// Text to put in its place; with regex, $1 refers to a capture group
    #[arg(long = "with")]
    pub with: String,
    /// Treat the text to find as a regular expression
    #[arg(long)]
    #[serde(default)]
    pub regex: bool,
    /// Match case exactly (default: case-insensitive)
    #[arg(long)]
    #[serde(default)]
    pub case_sensitive: bool,
    /// Pages to change, e.g. "1-5" (default: all)
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

pub(crate) type Matrix = [f64; 6];
pub(crate) type Area = [f64; 4];

const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `first` followed by `second`, in PDF's row-vector convention.
fn concat(first: Matrix, second: Matrix) -> Matrix {
    [
        first[0] * second[0] + first[1] * second[2],
        first[0] * second[1] + first[1] * second[3],
        first[2] * second[0] + first[3] * second[2],
        first[2] * second[1] + first[3] * second[3],
        first[4] * second[0] + first[5] * second[2] + second[4],
        first[4] * second[1] + first[5] * second[3] + second[5],
    ]
}

pub(crate) fn apply(m: Matrix, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

pub(crate) fn invert(m: Matrix) -> Option<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    (det.abs() > 1e-12).then(|| {
        [
            m[3] / det,
            -m[1] / det,
            -m[2] / det,
            m[0] / det,
            (m[2] * m[5] - m[3] * m[4]) / det,
            (m[1] * m[4] - m[0] * m[5]) / det,
        ]
    })
}

pub(crate) fn bounds(points: impl IntoIterator<Item = (f64, f64)>) -> Area {
    points.into_iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |b, (x, y)| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)],
    )
}

/// A box as reported to callers: tenths of a point.
pub(crate) fn round_box(area: &Area) -> [f64; 4] {
    area.map(|v| (v * 10.0).round() / 10.0)
}

fn intersects(a: &Area, b: &Area) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn contains(outer: &Area, inner: &Area) -> bool {
    let slack = 0.5;
    inner[0] >= outer[0] - slack
        && inner[1] >= outer[1] - slack
        && inner[2] <= outer[2] + slack
        && inner[3] <= outer[3] + slack
}

fn number(o: &Object) -> Option<f64> {
    match o {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(*r as f64),
        _ => None,
    }
}

fn numbers<const N: usize>(operands: &[Object]) -> Option<[f64; N]> {
    let v: Vec<f64> = operands.iter().filter_map(number).collect();
    <[f64; N]>::try_from(v).ok()
}

/// One glyph as the renderer places it.
#[derive(Clone)]
struct Placed {
    /// Box in layout coordinates (top-left origin).
    bbox: Area,
    /// Advance in thousandths of the font size, when the font states one.
    advance: Option<f32>,
    origin: Point,
    /// The direction and length of one em of horizontal advance on the page.
    em: (f64, f64),
    /// Which font drew it and what it reads as, to reuse the font for replacement text.
    font: Option<u128>,
    text: Option<String>,
}

impl Placed {
    /// How far `to` lies ahead of this glyph, in thousandths of the font size.
    fn units_to(&self, to: Point) -> f64 {
        let length = self.em.0 * self.em.0 + self.em.1 * self.em.1;
        let delta = to - self.origin;
        if length > 0.0 {
            (delta.x * self.em.0 + delta.y * self.em.1) / length * 1000.0
        } else {
            0.0
        }
    }
}

impl Placed {
    /// The first area covering the glyph: its centre, or a substantial part of its box.
    fn hit(&self, areas: &[Area]) -> Option<usize> {
        let (cx, cy) = (
            (self.bbox[0] + self.bbox[2]) / 2.0,
            (self.bbox[1] + self.bbox[3]) / 2.0,
        );
        let size = (self.bbox[2] - self.bbox[0]) * (self.bbox[3] - self.bbox[1]);
        areas.iter().position(|a| {
            let w = (self.bbox[2].min(a[2]) - self.bbox[0].max(a[0])).max(0.0);
            let h = (self.bbox[3].min(a[3]) - self.bbox[1].max(a[1])).max(0.0);
            (cx >= a[0] && cx <= a[2] && cy >= a[1] && cy <= a[3])
                || (size > 0.0 && w * h > 0.3 * size)
        })
    }
}

/// Records the glyphs of every text-showing operator, in content order.
#[derive(Default)]
struct Runs(Vec<Vec<Placed>>);

impl<'a> Device<'a> for Runs {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, mode: &DrawMode) {
        let glyphs: Vec<Placed> = run
            .glyphs()
            .iter()
            .map(|glyph| {
                let to_page: Affine = props.transform * glyph.transform();
                let (advance, font) = match &**glyph {
                    Glyph::Outline(o) => (o.advance_width(), Some(o.font_cache_key())),
                    Glyph::Type3(_) => (None, None),
                };
                let text = glyph.as_unicode().map(|u| match u {
                    BfString::Char(c) => c.to_string(),
                    BfString::String(s) => s,
                });
                let origin = to_page * Point::new(0.0, 0.0);
                let end = to_page * Point::new(advance.unwrap_or(500.0) as f64, 0.0);
                let up = (to_page * Point::new(0.0, 1000.0)) - origin;
                let corners = [
                    origin - up * 0.2,
                    origin + up * 0.8,
                    end - up * 0.2,
                    end + up * 0.8,
                ];
                let em = (to_page * Point::new(1000.0, 0.0)) - origin;
                Placed {
                    bbox: bounds(corners.iter().map(|p| (p.x, p.y))),
                    advance,
                    origin,
                    em: (em.x, em.y),
                    font,
                    text,
                }
            })
            .collect();
        // Text that is both filled and stroked arrives twice; it is one operator.
        let repeat = matches!(mode, DrawMode::Stroke(_))
            && self.0.last().is_some_and(|last| {
                last.len() == glyphs.len()
                    && last
                        .iter()
                        .zip(&glyphs)
                        .all(|(a, b)| (a.origin - b.origin).hypot() < 1e-6)
            });
        if !repeat {
            self.0.push(glyphs);
        }
    }
}

fn glyph_runs(page: &Page<'_>, annotations: bool) -> Vec<Vec<Placed>> {
    let (w, h) = page.render_dimensions();
    let cache = InterpreterCache::new();
    let settings = InterpreterSettings {
        render_annotations: annotations,
        ..Default::default()
    };
    let mut context = Context::new(
        page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, w as f64, h as f64),
        &cache,
        page.xref(),
        settings,
    );
    let mut runs = Runs::default();
    interpret_page(page, &mut context, &mut runs);
    runs.0
}

#[derive(Default, Clone, Copy)]
struct Stats {
    glyphs: usize,
    paths: usize,
    images: usize,
    annotations: usize,
}

/// The part of the graphics state that q and Q save and restore.
#[derive(Clone, Copy)]
struct State {
    ctm: Matrix,
    char_spacing: f64,
    word_spacing: f64,
    font_size: f64,
    render_mode: i64,
    /// The current font, as an index into the names seen in this stream.
    font: Option<usize>,
}

struct Rewriter<'a> {
    pdf: &'a Pdf,
    runs: &'a [Vec<Placed>],
    next_run: usize,
    /// Areas in layout coordinates, for glyphs.
    areas: &'a [Area],
    /// The same areas in the page's user space, for paths and images.
    user_areas: &'a [Area],
    stats: Stats,
    /// Text to put in place of each area's glyphs; empty when redacting.
    replacements: &'a [String],
    /// What became of each replacement so far.
    placed: Vec<Outcome>,
    /// Every character seen on the page, by font: its code and width. This is how
    /// replacement text is written in the document's own (usually subsetted) font.
    seen: HashMap<(u128, String), (Vec<u8>, f32)>,
    /// First pass: only fill `seen`.
    collect_only: bool,
    /// Changes of width that the rest of each line follows; empty when nothing moves.
    reflow: Vec<Reflow>,
}

/// What one walk over a content stream carries along and builds up.
struct Pass {
    /// The rewritten operations.
    out: Vec<Operation>,
    /// The stream's resources, extended when fonts or replaced objects are added.
    resources: Dictionary,
    changed: bool,
    state: State,
    saved: Vec<State>,
    /// Font names seen in `Tf`, which `State::font` indexes.
    fonts: Vec<Vec<u8>>,
    /// The path under construction: where it starts in `out`, its extent, and whether it clips.
    path: Option<(usize, Area, bool)>,
    /// Open marked-content sections, as positions in `out` (None when they carry no properties).
    marked: Vec<Option<usize>>,
    /// The inline images of this stream, drawn as image objects in `out`.
    inline: Vec<Lifted>,
}

/// How a replacement was written.
#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Pending,
    Written {
        /// In the font of the text it replaces; otherwise that font lacked a glyph
        /// and another one wrote it.
        own_font: bool,
        /// Widths of the new and the old text, in points.
        new: f64,
        old: f64,
        /// Where on the page it starts, and the direction its line runs in.
        at: Point,
        along: (f64, f64),
    },
}

/// A change in the width of a piece of text, which what follows on its line makes room for.
#[derive(Clone, Copy)]
struct Reflow {
    at: Point,
    along: (f64, f64),
    /// By how much the text grew, in points; negative when it shrank.
    delta: f64,
    /// Beyond this point the line is drawn closer together by `factor`, to stay in its
    /// column. The same for every change on one line; a factor of one changes nothing.
    anchor: Point,
    factor: f64,
}

fn stream_bytes(stream: &Stream) -> Result<Vec<u8>> {
    if stream.dict.has(b"Filter") {
        stream
            .decompressed_content()
            .map_err(|e| anyhow!("cannot decode a content stream: {e}"))
    } else {
        Ok(stream.content.clone())
    }
}

/// Sets `count` samples of `bits` bits each to zero, starting at sample `from` of a packed row.
fn zero_bits(row: &mut [u8], from: usize, count: usize, bits: usize) {
    for bit in from * bits..(from + count) * bits {
        if let Some(byte) = row.get_mut(bit / 8) {
            *byte &= !(0x80 >> (bit % 8));
        }
    }
}

/// An inline image taken out of a content stream, to be drawn as an image object instead.
struct Lifted {
    name: Vec<u8>,
    /// The image as it stood in the content, from `BI` to `EI`.
    source: Vec<u8>,
    image: Stream,
}

fn unreadable_inline_image() -> anyhow::Error {
    anyhow!("the page has an inline image that cannot be read with certainty; nothing was written")
}

/// The inline image that `content` starts with.
fn inline_image(content: &[u8]) -> Option<LazyStream<'_>> {
    match TypedIter::new(content).next()? {
        TypedInstruction::InlineImage(image) => Some(image.0.clone()),
        _ => None,
    }
}

/// Whether a byte can be part of a number, a name or an operator.
fn is_regular(byte: u8) -> bool {
    !matches!(
        byte,
        0 | 9
            | 10
            | 12
            | 13
            | 32
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

/// The long form of what an inline image may abbreviate in a colour space or a filter.
fn spelled_out(name: &[u8]) -> Option<&'static str> {
    Some(match name {
        b"G" => "DeviceGray",
        b"RGB" => "DeviceRGB",
        b"CMYK" => "DeviceCMYK",
        b"I" => "Indexed",
        b"AHx" => "ASCIIHexDecode",
        b"A85" => "ASCII85Decode",
        b"LZW" => "LZWDecode",
        b"Fl" => "FlateDecode",
        b"RL" => "RunLengthDecode",
        b"CCF" => "CCITTFaxDecode",
        b"DCT" => "DCTDecode",
        _ => return None,
    })
}

/// An inline image as the image object that draws the same.
fn image_object(
    d: &Document,
    image: &LazyStream<'_>,
    data: &[u8],
    resources: &Dictionary,
) -> Result<Stream> {
    let name = |name: &[u8]| match spelled_out(name) {
        Some(long) => Object::Name(long.into()),
        None => Object::Name(name.to_vec()),
    };
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    for (key, value) in doc::direct_dictionary(image.dict())?.iter() {
        let key: &[u8] = match key.as_slice() {
            b"BPC" => b"BitsPerComponent",
            b"CS" => b"ColorSpace",
            b"D" => b"Decode",
            b"DP" => b"DecodeParms",
            b"F" => b"Filter",
            b"H" => b"Height",
            b"IM" => b"ImageMask",
            b"I" => b"Interpolate",
            b"W" => b"Width",
            b"L" | b"Length" => continue,
            other => other,
        };
        let value = match (key, value) {
            // Any other name stands for a colour space among the resources.
            (b"ColorSpace", Object::Name(n)) if spelled_out(n).is_none() => resources
                .get(b"ColorSpace")
                .ok()
                .and_then(|spaces| doc::resolve(d, spaces).as_dict().ok())
                .and_then(|spaces| spaces.get(n).ok())
                .cloned()
                .unwrap_or_else(|| value.clone()),
            (b"ColorSpace" | b"Filter", Object::Name(n)) => name(n),
            (b"ColorSpace" | b"Filter", Object::Array(items)) => Object::Array(
                items
                    .iter()
                    .map(|item| match item {
                        Object::Name(n) => name(n),
                        other => other.clone(),
                    })
                    .collect(),
            ),
            _ => value.clone(),
        };
        dict.set(key, value);
    }
    Ok(Stream::new(dict, data.to_vec()))
}

/// Takes the inline images out of a content stream. Each becomes the drawing of an
/// image object, which is returned with the name it is drawn by.
///
/// lopdf, which rewrites the content, reads only the plainest inline images and drops
/// the rest, so none is left to it: where one ends is decided by the reader that
/// also renders the page.
fn lift_inline_images(
    d: &Document,
    content: &[u8],
    resources: &Dictionary,
) -> Result<(Vec<u8>, Vec<Lifted>)> {
    let mut lifted = Vec::new();
    let mut out = Vec::new();
    if !content.windows(2).any(|w| w == b"BI") {
        return Ok((out, lifted));
    }
    let taken = resources
        .get(b"XObject")
        .ok()
        .and_then(|x| doc::resolve(d, x).as_dict().ok());
    let mut number = 0;
    let (mut at, mut copied) = (0, 0);
    while at < content.len() {
        let rest = &content[at..];
        at += match rest[0] {
            b'%' => rest
                .iter()
                .position(|b| matches!(b, b'\n' | b'\r'))
                .unwrap_or(rest.len()),
            b'(' => {
                let (mut depth, mut end) = (0, 0);
                while end < rest.len() {
                    match rest[end] {
                        b'\\' => end += 1,
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    end += 1;
                    if depth == 0 {
                        break;
                    }
                }
                end
            }
            b'<' if rest.get(1) != Some(&b'<') => rest
                .iter()
                .position(|&b| b == b'>')
                .map_or(rest.len(), |end| end + 1),
            b'/' => 1 + rest[1..].iter().take_while(|&&b| is_regular(b)).count(),
            byte if is_regular(byte) => {
                let len = rest.iter().take_while(|&&b| is_regular(b)).count();
                if &rest[..len] == b"BI" {
                    let image = inline_image(rest).ok_or_else(unreadable_inline_image)?;
                    let std::borrow::Cow::Borrowed(data) = image.raw_data() else {
                        return Err(unreadable_inline_image());
                    };
                    // The data is a part of `rest`, and `EI` follows it.
                    let end = (data.as_ptr() as usize + data.len())
                        .checked_sub(rest.as_ptr() as usize)
                        .filter(|&end| rest.get(end..end + 2) == Some(b"EI"))
                        .ok_or_else(unreadable_inline_image)?
                        + 2;
                    let name = loop {
                        number += 1;
                        let name = format!("Inline{number}").into_bytes();
                        if taken.is_none_or(|taken| !taken.has(&name)) {
                            break name;
                        }
                    };
                    out.extend_from_slice(&content[copied..at]);
                    out.extend_from_slice(b" /");
                    out.extend_from_slice(&name);
                    out.extend_from_slice(b" Do ");
                    copied = at + end;
                    lifted.push(Lifted {
                        name,
                        source: rest[..end].to_vec(),
                        image: image_object(d, &image, data, resources)?,
                    });
                    end
                } else {
                    len
                }
            }
            _ => 1,
        };
    }
    if !lifted.is_empty() {
        out.extend_from_slice(&content[copied.min(content.len())..]);
    }
    Ok((out, lifted))
}

/// Whether a colour space is indexed, and how many components a sample has in it,
/// where that can be told without reading further.
fn components(d: &Document, space: &Object) -> (bool, Option<u8>) {
    let by_name = |name: &[u8]| match name {
        b"DeviceGray" | b"CalGray" | b"Separation" => Some(1),
        b"DeviceRGB" | b"CalRGB" | b"Lab" => Some(3),
        b"DeviceCMYK" => Some(4),
        _ => None,
    };
    match space {
        Object::Name(name) => (false, by_name(name)),
        Object::Array(items) => {
            let family = items.first().and_then(|n| n.as_name().ok()).unwrap_or(b"");
            let detail = items.get(1).map(|o| doc::resolve(d, o));
            match family {
                b"Indexed" => (true, Some(1)),
                b"ICCBased" => (
                    false,
                    detail
                        .and_then(|profile| profile.as_stream().ok())
                        .and_then(|profile| profile.dict.get(b"N").ok())
                        .and_then(|n| doc::resolve(d, n).as_i64().ok())
                        .and_then(|n| u8::try_from(n).ok()),
                ),
                b"DeviceN" => (
                    false,
                    detail
                        .and_then(|names| names.as_array().ok())
                        .and_then(|names| u8::try_from(names.len()).ok()),
                ),
                _ => (false, by_name(family)),
            }
        }
        _ => (false, None),
    }
}

/// Sets to zero the samples of a packed raster that fall into the areas.
fn zero_areas(
    data: &mut [u8],
    (width, height): (usize, usize),
    sample_bits: usize,
    to_image: Matrix,
    areas: &[Area],
) {
    let row_bytes = data.len() / height;
    for area in areas {
        let corners = [
            (area[0], area[1]),
            (area[2], area[1]),
            (area[0], area[3]),
            (area[2], area[3]),
        ];
        // Image space is the unit square with the first row of samples at the top.
        let b = bounds(corners.map(|(x, y)| apply(to_image, x, y)));
        let x0 = ((b[0].clamp(0.0, 1.0) * width as f64).floor() as usize).min(width);
        let x1 = ((b[2].clamp(0.0, 1.0) * width as f64).ceil() as usize).min(width);
        let y0 = (((1.0 - b[3].clamp(0.0, 1.0)) * height as f64).floor() as usize).min(height);
        let y1 = (((1.0 - b[1].clamp(0.0, 1.0)) * height as f64).ceil() as usize).min(height);
        for row in data.chunks_mut(row_bytes).take(y1).skip(y0) {
            zero_bits(row, x0, x1.saturating_sub(x0), sample_bits);
        }
    }
}

impl Rewriter<'_> {
    /// The areas an image drawn with this matrix reaches into.
    fn covered(&self, ctm: Matrix) -> Vec<Area> {
        let placed =
            bounds([(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].map(|(x, y)| apply(ctm, x, y)));
        self.user_areas
            .iter()
            .filter(|a| intersects(a, &placed))
            .copied()
            .collect()
    }

    /// Blanks the pixels of an image object that fall into an area. Returns the replacement image.
    fn image(&mut self, d: &mut Document, id: ObjectId, ctm: Matrix) -> Result<Option<ObjectId>> {
        let hits = self.covered(ctm);
        if hits.is_empty() {
            return Ok(None);
        }
        let dict = d.get_object(id)?.as_stream()?.dict.clone();
        let source: LazyStream<'_> = self
            .pdf
            .xref()
            .get(ObjectIdentifier::new(id.0 as i32, id.1 as i32))
            .ok_or_else(|| anyhow!("cannot redact unreadable images; nothing was written"))?;
        let blanked = self.blank(d, &dict, &source, ctm, &hits)?;
        Ok(Some(d.add_object(blanked)))
    }

    /// An image with the pixels in the areas set to zero, stored without loss.
    ///
    /// The samples are taken as the renderer decodes them, whatever they are packed
    /// with: fax and JBIG2 as scanners write them, JPEG in any colour model, JPEG 2000.
    fn blank(
        &mut self,
        d: &mut Document,
        dict: &Dictionary,
        source: &LazyStream<'_>,
        ctm: Matrix,
        hits: &[Area],
    ) -> Result<Stream> {
        let unsupported = |what: &str| anyhow!("cannot redact {what} images; nothing was written");
        let int = |key: &[u8]| {
            dict.get(key)
                .ok()
                .and_then(|o| doc::resolve(d, o).as_i64().ok())
        };
        let mask = dict
            .get(b"ImageMask")
            .is_ok_and(|m| doc::resolve(d, m).as_bool().unwrap_or(false));
        let stated_bits = if mask {
            Some(1)
        } else {
            int(b"BitsPerComponent")
        };
        let space = dict.get(b"ColorSpace").ok().map(|s| doc::resolve(d, s));
        let (indexed, in_a_sample) = space.map_or((false, None), |s| components(d, s));
        let mut size = (
            int(b"Width").unwrap_or(0) as usize,
            int(b"Height").unwrap_or(0) as usize,
        );
        let inside = int(b"SMaskInData").unwrap_or(0) != 0;
        let decoded = source
            .decoded_image(&ImageDecodeParams {
                // Bilevel data is wanted as its packed bits, which this asks for.
                is_indexed: indexed || stated_bits == Some(1),
                bpc: stated_bits.and_then(|bits| u8::try_from(bits).ok()),
                num_components: in_a_sample,
                target_dimension: None,
                width: size.0 as u32,
                height: size.1 as u32,
            })
            .map_err(|_| unsupported("undecodable"))?;
        let mut bits = stated_bits.unwrap_or(8) as usize;
        let mut new = dict.clone();
        let mut alpha = None;
        // Formats that carry their own description are taken at their word.
        if let Some(found) = decoded.image_data {
            size = (found.width as usize, found.height as usize);
            new.set("Width", found.width as i64);
            new.set("Height", found.height as i64);
            if !mask {
                bits = found.bits_per_component as usize;
                new.set("BitsPerComponent", bits as i64);
                if space.is_none() {
                    let space = match found.color_space {
                        Some(ImageColorSpace::Gray) => "DeviceGray",
                        Some(ImageColorSpace::Rgb) => "DeviceRGB",
                        Some(ImageColorSpace::Cmyk) => "DeviceCMYK",
                        _ => return Err(unsupported("JPEG 2000 of an unknown colour model")),
                    };
                    new.set("ColorSpace", Object::Name(space.into()));
                }
            }
            alpha = found.alpha;
        }
        let mut data = decoded.data.into_owned();
        let (width, height) = size;
        if width == 0 || height == 0 || bits == 0 || data.len() % height != 0 {
            return Err(unsupported("malformed"));
        }
        let in_a_sample = data.len() / height * 8 / (width * bits);
        if in_a_sample == 0 {
            return Err(unsupported("malformed"));
        }
        let to_image = invert(ctm).ok_or_else(|| unsupported("degenerate"))?;
        zero_areas(&mut data, size, bits * in_a_sample, to_image, hits);
        // Transparency stored inside JPEG 2000 data becomes a mask of its own, blank
        // in the same places: its shape could show what was there.
        new.remove(b"SMaskInData");
        if let Some(mut alpha) = alpha.filter(|_| inside) {
            let alpha_bits = alpha.len() / height * 8 / width;
            if alpha.len() % height != 0 || alpha_bits == 0 {
                return Err(unsupported("malformed"));
            }
            zero_areas(&mut alpha, size, alpha_bits, to_image, hits);
            let mut soft = Dictionary::new();
            soft.set("Type", Object::Name(b"XObject".to_vec()));
            soft.set("Subtype", Object::Name(b"Image".to_vec()));
            soft.set("Width", width as i64);
            soft.set("Height", height as i64);
            soft.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
            soft.set("BitsPerComponent", alpha_bits as i64);
            let mut soft = Stream::new(soft, alpha);
            let _ = soft.compress();
            new.set("SMask", d.add_object(soft));
        }
        new.remove(b"Filter");
        new.remove(b"DecodeParms");
        let mut replacement = Stream::new(new, data);
        let _ = replacement.compress();
        self.stats.images += 1;
        Ok(replacement)
    }

    /// Rewrites one content stream. Returns the new content and resources if anything changed.
    fn content(
        &mut self,
        d: &mut Document,
        content: &[u8],
        resources: &Dictionary,
        base: Matrix,
        depth: u32,
    ) -> Result<Option<(Vec<u8>, Dictionary)>> {
        if depth > 16 {
            bail!("forms are nested too deeply to redact safely; nothing was written");
        }
        let (without_inline, inline) = lift_inline_images(d, content, resources)?;
        let content = if inline.is_empty() {
            content
        } else {
            &without_inline
        };
        let operations = Content::decode(content)
            .map_err(|e| anyhow!("cannot parse page content: {e}"))?
            .operations;
        let mut pass = Pass {
            out: Vec::with_capacity(operations.len()),
            resources: resources.clone(),
            changed: false,
            state: State {
                ctm: base,
                char_spacing: 0.0,
                word_spacing: 0.0,
                font_size: 0.0,
                render_mode: 0,
                font: None,
            },
            saved: Vec::new(),
            fonts: Vec::new(),
            path: None,
            marked: Vec::new(),
            inline,
        };

        for op in operations {
            let name = op.operator.as_str();
            match name {
                "q" => pass.saved.push(pass.state),
                "Q" => pass.state = pass.saved.pop().unwrap_or(pass.state),
                "cm" => {
                    if let Some(m) = numbers::<6>(&op.operands) {
                        pass.state.ctm = concat(m, pass.state.ctm);
                    }
                }
                "Tc" => {
                    pass.state.char_spacing = op.operands.first().and_then(number).unwrap_or(0.0)
                }
                "Tw" => {
                    pass.state.word_spacing = op.operands.first().and_then(number).unwrap_or(0.0)
                }
                "Tr" => {
                    pass.state.render_mode = op
                        .operands
                        .first()
                        .and_then(|o| o.as_i64().ok())
                        .unwrap_or(0)
                }
                "Tf" => {
                    pass.state.font_size = op.operands.get(1).and_then(number).unwrap_or(0.0);
                    if let Some(font) = op.operands.first().and_then(|n| n.as_name().ok()) {
                        pass.fonts.push(font.to_vec());
                        pass.state.font = Some(pass.fonts.len() - 1);
                    }
                }
                "BMC" => pass.marked.push(None),
                "BDC" => pass.marked.push(Some(pass.out.len())),
                "EMC" => {
                    pass.marked.pop();
                }
                "BI" => return Err(unreadable_inline_image()),
                _ => {}
            }

            match name {
                "m" | "l" | "c" | "v" | "y" | "re" | "h" => {
                    let n: Vec<f64> = op.operands.iter().filter_map(number).collect();
                    let points: Vec<(f64, f64)> = if name == "re" && n.len() == 4 {
                        vec![
                            (n[0], n[1]),
                            (n[0] + n[2], n[1]),
                            (n[0], n[1] + n[3]),
                            (n[0] + n[2], n[1] + n[3]),
                        ]
                    } else {
                        n.as_chunks::<2>().0.iter().map(|p| (p[0], p[1])).collect()
                    };
                    let extent =
                        bounds(points.into_iter().map(|(x, y)| apply(pass.state.ctm, x, y)));
                    let (start, so_far, clips) =
                        pass.path.unwrap_or((pass.out.len(), extent, false));
                    pass.path = Some((
                        start,
                        bounds([
                            (so_far[0], so_far[1]),
                            (so_far[2], so_far[3]),
                            (extent[0], extent[1]),
                            (extent[2], extent[3]),
                        ]),
                        clips,
                    ));
                    pass.out.push(op);
                }
                "W" | "W*" => {
                    if let Some(p) = &mut pass.path {
                        p.2 = true;
                    }
                    pass.out.push(op);
                }
                "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                    // A drawing that lies wholly inside an area goes, with its coordinates.
                    // One that only crosses an area stays: it also draws what is outside.
                    let inside = pass.path.is_some_and(|(_, extent, clips)| {
                        name != "n"
                            && !clips
                            && self.user_areas.iter().any(|a| contains(a, &extent))
                    }) && self.replacements.is_empty()
                        && !self.collect_only;
                    match pass.path.take() {
                        Some((start, ..)) if inside => {
                            pass.out.truncate(start);
                            self.stats.paths += 1;
                            pass.changed = true;
                        }
                        _ => pass.out.push(op),
                    }
                }
                "Tj" | "'" | "\"" | "TJ" => self.show_text(d, op, &mut pass)?,
                "Do" => self.draw_object(d, op, &mut pass, depth)?,
                _ => pass.out.push(op),
            }
        }
        if !pass.changed {
            return Ok(None);
        }
        if !pass.inline.is_empty() {
            let mut xobjects = pass
                .resources
                .get(b"XObject")
                .ok()
                .and_then(|x| doc::resolve(d, x).as_dict().ok())
                .cloned()
                .unwrap_or_default();
            for lifted in pass.inline {
                xobjects.set(lifted.name, d.add_object(lifted.image));
            }
            pass.resources.set("XObject", xobjects);
        }
        let encoded = Content {
            operations: pass.out,
        }
        .encode()
        .map_err(|e| anyhow!("cannot write page content: {e}"))?;
        Ok(Some((encoded, pass.resources)))
    }

    /// How far along its line a glyph at `origin` moves to follow the changes before it.
    fn shift_at(&self, origin: Point, em: (f64, f64)) -> f64 {
        let size = em.0.hypot(em.1);
        if self.reflow.is_empty() || size <= 0.0 {
            return 0.0;
        }
        let mut moved = 0.0;
        let mut line = None;
        for change in &self.reflow {
            let from = origin - change.at;
            let same_way = (em.0 * change.along.0 + em.1 * change.along.1) / size > 0.99;
            let beside = (from.x * change.along.1 - from.y * change.along.0).abs();
            if same_way && beside < 0.3 * size {
                if from.x * change.along.0 + from.y * change.along.1 > 0.01 {
                    moved += change.delta;
                }
                line = Some(change);
            }
        }
        if let Some(change) = line.filter(|c| c.factor < 1.0) {
            let from = origin - change.anchor;
            let beyond = from.x * change.along.0 + from.y * change.along.1 + moved;
            if beyond > 0.0 {
                moved -= beyond * (1.0 - change.factor);
            }
        }
        moved
    }

    /// Handles one text-showing operator: passes it on, or rewrites it without the glyphs in an area.
    fn show_text(&mut self, d: &mut Document, op: Operation, pass: &mut Pass) -> Result<()> {
        let name = op.operator.as_str();
        let strings: usize = match name {
            "TJ" => op
                .operands
                .first()
                .and_then(|a| a.as_array().ok())
                .map_or(0, |a| {
                    a.iter()
                        .filter_map(|o| o.as_str().ok())
                        .map(<[u8]>::len)
                        .sum()
                }),
            _ => op
                .operands
                .last()
                .and_then(|s| s.as_str().ok())
                .map_or(0, <[u8]>::len),
        };
        if name == "\""
            && let Some([word, character]) =
                numbers::<2>(&op.operands[..op.operands.len().saturating_sub(1)])
        {
            pass.state.word_spacing = word;
            pass.state.char_spacing = character;
        }
        // Empty strings and clip-only text draw nothing, so the renderer reports no run.
        if strings == 0 || pass.state.render_mode == 7 {
            pass.out.push(op);
            return Ok(());
        }
        let run = self.runs.get(self.next_run).ok_or_else(out_of_step)?;
        self.next_run += 1;
        if self.collect_only {
            if !run.is_empty() && strings.is_multiple_of(run.len()) {
                let codes = match name {
                    "TJ" => op
                        .operands
                        .first()
                        .and_then(|a| a.as_array().ok())
                        .cloned()
                        .unwrap_or_default(),
                    _ => op.operands.last().cloned().into_iter().collect(),
                };
                let bytes: Vec<u8> = codes
                    .iter()
                    .filter_map(|o| o.as_str().ok())
                    .flatten()
                    .copied()
                    .collect();
                for (code, glyph) in bytes.chunks(strings / run.len()).zip(run) {
                    if let (Some(font), Some(text), Some(advance)) =
                        (glyph.font, &glyph.text, glyph.advance)
                    {
                        self.seen
                            .entry((font, text.clone()))
                            .or_insert_with(|| (code.to_vec(), advance));
                    }
                }
            }
            pass.out.push(op);
            return Ok(());
        }
        let hits: Vec<Option<usize>> = run.iter().map(|g| g.hit(self.areas)).collect();
        let remove: Vec<bool> = hits.iter().map(Option::is_some).collect();
        // Where each glyph belongs once the line has made room for changed text.
        let wants: Vec<f64> = run.iter().map(|g| self.shift_at(g.origin, g.em)).collect();
        let removes = remove.contains(&true);
        if !removes && !wants.iter().any(|w| w.abs() > 0.01) {
            pass.out.push(op);
            return Ok(());
        }
        if run.is_empty() || !strings.is_multiple_of(run.len()) || pass.state.font_size == 0.0 {
            if !removes {
                // Text that cannot be taken apart stays where it is.
                pass.out.push(op);
                return Ok(());
            }
            bail!(
                "text in the area uses an encoding that cannot be redacted safely; nothing was written"
            );
        }
        let code_len = strings / run.len();

        // The operator becomes a TJ in which every removed glyph is replaced by
        // the same amount of movement, so the text around it stays where it was.
        let elements: Vec<Object> = match name {
            "TJ" => op
                .operands
                .first()
                .and_then(|a| a.as_array().ok())
                .cloned()
                .unwrap_or_default(),
            _ => op.operands.last().cloned().into_iter().collect(),
        };
        // Flattened to single character codes and the movements between them.
        let mut pieces: Vec<Result<(Vec<u8>, lopdf::StringFormat), Object>> = Vec::new();
        for element in elements {
            match element {
                Object::String(bytes, format) => {
                    pieces.extend(
                        bytes
                            .chunks(code_len)
                            .map(|code| Ok((code.to_vec(), format))),
                    );
                }
                other => pieces.push(Err(other)),
            }
        }
        // ' and " start a new line first; that part is kept as separate operators.
        if name == "\"" {
            pass.out.push(Operation::new(
                "Tw",
                vec![Object::Real(pass.state.word_spacing as f32)],
            ));
            pass.out.push(Operation::new(
                "Tc",
                vec![Object::Real(pass.state.char_spacing as f32)],
            ));
        }
        if name != "Tj" && name != "TJ" {
            pass.out.push(Operation::new("T*", vec![]));
        }
        let mut rebuilt: Vec<Object> = Vec::new();
        let mut kept: Option<(Vec<u8>, lopdf::StringFormat)> = None;
        let mut index = 0;
        // How far the pen has been moved off its own course so far, in points, and the
        // size of the glyphs it was moved by.
        let mut applied = 0.0f64;
        let mut em = 1.0f64;
        // Moves the pen so that what is shown next lands `want` points along the line
        // from where it would have. A number in a TJ array moves the pen back by
        // thousandths of the font size.
        let mut settle =
            |want: f64,
             glyph: &Placed,
             rebuilt: &mut Vec<Object>,
             kept: &mut Option<(Vec<u8>, lopdf::StringFormat)>| {
                em = glyph.em.0.hypot(glyph.em.1).max(0.001);
                if (want - applied).abs() > 0.01 {
                    rebuilt.extend(
                        kept.take()
                            .map(|(bytes, format)| Object::String(bytes, format)),
                    );
                    rebuilt.push(Object::Real((-(want - applied) / em * 1000.0) as f32));
                    applied = want;
                }
            };
        for (at, piece) in pieces.iter().enumerate() {
            let (code, format) = match piece {
                Ok(code) => code,
                Err(movement) => {
                    rebuilt.extend(
                        kept.take()
                            .map(|(bytes, format)| Object::String(bytes, format)),
                    );
                    rebuilt.push(movement.clone());
                    continue;
                }
            };
            if !remove[index] {
                settle(wants[index], &run[index], &mut rebuilt, &mut kept);
                kept.get_or_insert_with(|| (Vec::new(), *format))
                    .0
                    .extend_from_slice(code);
                index += 1;
                continue;
            }
            rebuilt.extend(
                kept.take()
                    .map(|(bytes, format)| Object::String(bytes, format)),
            );
            let glyph = &run[index];
            let area = hits[index].expect("removed glyphs have an area");
            if self.placed.get(area) == Some(&Outcome::Pending) {
                settle(wants[index], glyph, &mut rebuilt, &mut kept);
                self.place_replacement(d, glyph, area, *format, pass, &mut rebuilt)?;
            }
            // The renderer's own positions say how far the glyph moved the pen:
            // the distance to the next glyph, less any explicit movement in between.
            let shift = match run.get(index + 1) {
                Some(next) => {
                    let between: f64 = pieces[at + 1..]
                        .iter()
                        .map_while(|p| p.as_ref().err())
                        .filter_map(number)
                        .sum();
                    glyph.units_to(next.origin) + between
                }
                // Nothing follows in this operator. The stated width is used when there
                // is one; otherwise the next operator shows where the pen ended up,
                // provided it carries on along the same line.
                None => match glyph.advance {
                    Some(advance) => {
                        let space = if code == &[32] {
                            pass.state.word_spacing
                        } else {
                            0.0
                        };
                        advance as f64
                            + (pass.state.char_spacing + space) * 1000.0 / pass.state.font_size
                    }
                    None => self
                        .runs
                        .get(self.next_run)
                        .and_then(|run| run.first())
                        .map(|next| glyph.units_to(next.origin))
                        .filter(|units| (0.0..3000.0).contains(units))
                        .unwrap_or(500.0),
                },
            };
            if !shift.is_finite() || shift.abs() > 100_000.0 {
                bail!(
                    "text in the area is laid out in a way that cannot be redacted safely; nothing was written"
                );
            }
            rebuilt.push(Object::Real(-shift as f32));
            if let Some(Outcome::Written { old, .. }) = self.placed.get_mut(area) {
                *old += shift * glyph.em.0.hypot(glyph.em.1) / 1000.0;
            }
            self.stats.glyphs += 1;
            index += 1;
        }
        rebuilt.extend(
            kept.take()
                .map(|(bytes, format)| Object::String(bytes, format)),
        );
        // The pen goes back on its own course, so that the next operator starts where
        // it always did and is moved, if at all, on its own account.
        if applied.abs() > 0.01 {
            rebuilt.push(Object::Real((applied / em * 1000.0) as f32));
        }
        if !rebuilt.is_empty() {
            pass.out
                .push(Operation::new("TJ", vec![Object::Array(rebuilt)]));
        }
        pass.changed = true;
        if !removes {
            return Ok(());
        }
        // Marked content may repeat the removed text as /ActualText or /Alt.
        for section in pass.marked.iter().flatten() {
            let tag = pass.out[*section]
                .operands
                .first()
                .cloned()
                .into_iter()
                .collect();
            pass.out[*section] = Operation::new("BMC", tag);
        }
        pass.changed = true;
        Ok(())
    }

    /// Writes an area's replacement text where its first removed glyph was.
    ///
    /// The text goes into `rebuilt`, followed by a movement back over its own
    /// width, so that whatever comes after keeps its position.
    fn place_replacement(
        &mut self,
        d: &mut Document,
        glyph: &Placed,
        area: usize,
        format: lopdf::StringFormat,
        pass: &mut Pass,
        rebuilt: &mut Vec<Object>,
    ) -> Result<()> {
        // The replacement goes where the first removed glyph was. It is written
        // with the codes this font uses for the same characters elsewhere, then
        // the pen is moved back, so everything after keeps its position.
        let size = (glyph.em.0.hypot(glyph.em.1)).max(0.001);
        let (at, along) = (glyph.origin, (glyph.em.0 / size, glyph.em.1 / size));
        let written: Option<Vec<&(Vec<u8>, f32)>> = glyph.font.and_then(|font| {
            self.replacements[area]
                .chars()
                .map(|c| self.seen.get(&(font, c.to_string())))
                .collect()
        });
        self.placed[area] = match written {
            Some(codes) => {
                let units: f64 = codes
                    .iter()
                    .map(|(code, advance)| {
                        let space = if code == &[32] {
                            pass.state.word_spacing
                        } else {
                            0.0
                        };
                        *advance as f64
                            + (pass.state.char_spacing + space) * 1000.0 / pass.state.font_size
                    })
                    .sum();
                if !codes.is_empty() {
                    let bytes: Vec<u8> = codes.iter().flat_map(|(code, _)| code.clone()).collect();
                    rebuilt.push(Object::String(bytes, format));
                    rebuilt.push(Object::Real(units as f32));
                }
                Outcome::Written {
                    own_font: true,
                    new: units * size / 1000.0,
                    old: 0.0,
                    at,
                    along,
                }
            }
            None => {
                // The font cannot write it. Another font takes over for
                // these characters only, in the same text object, so size,
                // colour, position and reading order all carry over.
                let text = &self.replacements[area];
                let mut new = 0.0;
                let original = pass
                    .state
                    .font
                    .and_then(|i| pass.fonts.get(i))
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!("text is shown without a font being set; nothing was written")
                    })?;
                if !text.is_empty() {
                    let font = TextFont::new(d, text, None)?;
                    let mut fonts = pass
                        .resources
                        .get(b"Font")
                        .ok()
                        .and_then(|f| doc::resolve(d, f).as_dict().ok())
                        .cloned()
                        .unwrap_or_default();
                    // One font, or several where no one has all the characters.
                    let mut keys = Vec::new();
                    for id in font.ids() {
                        let key = (1..)
                            .map(|i| format!("PdfopsR{i}"))
                            .find(|k| !fonts.has(k.as_bytes()))
                            .expect("an unbounded range always yields a free name");
                        fonts.set(key.as_str(), id);
                        keys.push(key);
                    }
                    pass.resources.set("Font", fonts);
                    let pieces = font.pieces(text);
                    // Word spacing applies to single-byte spaces only, which an embedded font has none of.
                    let spaces: usize = pieces
                        .iter()
                        .filter(|(_, font, _)| !font.is_embedded())
                        .map(|(_, _, piece)| piece.matches(' ').count())
                        .sum();
                    let spacing = pass.state.char_spacing * text.chars().count() as f64
                        + pass.state.word_spacing * spaces as f64;
                    let units = font.width(text, 1000.0) + spacing * 1000.0 / pass.state.font_size;
                    new = units * size / 1000.0;
                    if !rebuilt.is_empty() {
                        pass.out.push(Operation::new(
                            "TJ",
                            vec![Object::Array(std::mem::take(rebuilt))],
                        ));
                    }
                    let size = Object::Real(pass.state.font_size as f32);
                    let last = pieces.len() - 1;
                    for (n, (k, font, piece)) in pieces.into_iter().enumerate() {
                        let mut shown = font.elements(piece);
                        // After the last piece the pen goes back over all of it.
                        if n == last {
                            shown.push(Object::Real(units as f32));
                        }
                        pass.out.push(Operation::new(
                            "Tf",
                            vec![Object::Name(keys[k].clone().into_bytes()), size.clone()],
                        ));
                        pass.out
                            .push(Operation::new("TJ", vec![Object::Array(shown)]));
                    }
                    pass.out
                        .push(Operation::new("Tf", vec![Object::Name(original), size]));
                }
                Outcome::Written {
                    own_font: false,
                    new,
                    old: 0.0,
                    at,
                    along,
                }
            }
        };
        Ok(())
    }

    /// Handles `Do`: blanks an image, or descends into a form and swaps in the rewritten copy.
    fn draw_object(
        &mut self,
        d: &mut Document,
        op: Operation,
        pass: &mut Pass,
        depth: u32,
    ) -> Result<()> {
        let lifted = op
            .operands
            .first()
            .and_then(|n| n.as_name().ok())
            .and_then(|n| pass.inline.iter().position(|lifted| lifted.name == n));
        if let Some(lifted) = lifted {
            let hits = self.covered(pass.state.ctm);
            if !hits.is_empty() && self.replacements.is_empty() && !self.collect_only {
                let image = &pass.inline[lifted];
                let source = inline_image(&image.source).ok_or_else(unreadable_inline_image)?;
                let blanked = self.blank(d, &image.image.dict, &source, pass.state.ctm, &hits)?;
                pass.inline[lifted].image = blanked;
                pass.changed = true;
            }
            pass.out.push(op);
            return Ok(());
        }
        let target = op
            .operands
            .first()
            .and_then(|n| n.as_name().ok())
            .and_then(|n| {
                let xobjects = doc::resolve(d, pass.resources.get(b"XObject").ok()?)
                    .as_dict()
                    .ok()?;
                Some((n.to_vec(), xobjects.get(n).ok()?.as_reference().ok()?))
            });
        if let Some((key, id)) = target {
            let stream = d
                .get_object(id)
                .ok()
                .and_then(|o| o.as_stream().ok())
                .cloned();
            let subtype = stream.as_ref().and_then(|s| {
                s.dict
                    .get(b"Subtype")
                    .ok()?
                    .as_name()
                    .ok()
                    .map(<[u8]>::to_vec)
            });
            let replacement = match (subtype.as_deref(), stream) {
                (Some(b"Image"), _) if self.replacements.is_empty() && !self.collect_only => {
                    self.image(d, id, pass.state.ctm)?
                }
                (Some(b"Form"), Some(form)) => {
                    let matrix = form
                        .dict
                        .get(b"Matrix")
                        .ok()
                        .and_then(|m| doc::resolve(d, m).as_array().ok())
                        .and_then(|m| numbers::<6>(m))
                        .unwrap_or(IDENTITY);
                    let inner = form
                        .dict
                        .get(b"Resources")
                        .ok()
                        .and_then(|r| doc::resolve(d, r).as_dict().ok())
                        .cloned()
                        .unwrap_or_else(|| pass.resources.clone());
                    let body = stream_bytes(&form)?;
                    // A changed form becomes a copy, so other pages drawing it keep theirs.
                    match self.content(
                        d,
                        &body,
                        &inner,
                        concat(matrix, pass.state.ctm),
                        depth + 1,
                    )? {
                        Some((body, inner)) => {
                            let mut dict = form.dict.clone();
                            dict.remove(b"Filter");
                            dict.remove(b"DecodeParms");
                            dict.set("Resources", inner);
                            let mut copy = Stream::new(dict, body);
                            let _ = copy.compress();
                            Some(d.add_object(copy))
                        }
                        None => None,
                    }
                }
                _ => None,
            };
            if let Some(new) = replacement {
                let mut xobjects = pass
                    .resources
                    .get(b"XObject")
                    .ok()
                    .and_then(|x| doc::resolve(d, x).as_dict().ok())
                    .cloned()
                    .unwrap_or_default();
                xobjects.set(key, new);
                pass.resources.set("XObject", xobjects);
                pass.changed = true;
            }
        }
        pass.out.push(op);
        Ok(())
    }
}

fn out_of_step() -> anyhow::Error {
    anyhow!("the page draws text in a way that cannot be redacted safely; nothing was written")
}

/// Parses "page:x0,y0,x1,y1".
pub(crate) fn parse_rect(spec: &str, total: u32) -> Result<(u32, Area)> {
    let bad =
        || anyhow!("invalid rect '{spec}'; expected page:x0,y0,x1,y1, e.g. 2:100,200,300,220");
    let (page, coords) = spec.split_once(':').ok_or_else(bad)?;
    let page: u32 = page.trim().parse().map_err(|_| bad())?;
    if page == 0 || page > total {
        bail!("rect '{spec}': page {page} out of range 1-{total}");
    }
    let v: Vec<f64> = coords
        .split(',')
        .map(|c| c.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| bad())?;
    let [x0, y0, x1, y1] = v[..] else {
        return Err(bad());
    };
    let area = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
    if area[2] - area[0] <= 0.0 || area[3] - area[1] <= 0.0 {
        bail!("rect '{spec}' is empty");
    }
    Ok((page, area))
}

/// The area and text of every match of `patterns` on the page, line by line.
///
/// Areas follow the matched glyphs exactly, so a match inside a longer word
/// covers only its own characters.
pub(crate) fn text_areas(
    page: &layout::PageLayout,
    patterns: &[regex::Regex],
) -> Vec<(Area, String, usize)> {
    let mut areas = Vec::new();
    for line in layout::lines(&page.words) {
        // What each glyph reads as, with its box, and the spaces between words.
        let mut pieces: Vec<(&str, Option<Area>)> = Vec::new();
        for (i, word) in line.iter().enumerate() {
            if i > 0 {
                pieces.push((" ", None));
            }
            pieces.extend(
                word.pieces()
                    .zip(&word.parts)
                    .map(|(piece, part)| (piece, Some(part.1))),
            );
        }
        // The line as one string in the order it is read, which is how a text to
        // find is given, with each glyph's byte range and box.
        let drawn: Vec<&str> = pieces.iter().map(|p| p.0).collect();
        let order = layout::reading_order(&drawn)
            .unwrap_or_else(|| (0..pieces.len()).map(|i| (i, false)).collect());
        let mut text = String::new();
        let mut glyphs: Vec<(usize, usize, Area)> = Vec::new();
        for (i, turned) in order {
            let (piece, bbox) = pieces[i];
            let piece = if turned {
                layout::mirrored(piece)
            } else {
                piece
            };
            if let Some(bbox) = bbox {
                glyphs.push((text.len(), text.len() + piece.len(), bbox));
            }
            text.push_str(piece);
        }
        for (which, pattern) in patterns.iter().enumerate() {
            for found in pattern.find_iter(&text) {
                let hit = glyphs
                    .iter()
                    .filter(|g| g.0 < found.end() && found.start() < g.1);
                let b = bounds(hit.flat_map(|g| [(g.2[0], g.2[1]), (g.2[2], g.2[3])]));
                if b[0].is_finite() {
                    areas.push((b, found.as_str().to_string(), which));
                }
            }
        }
    }
    areas
}

pub(crate) fn compile(
    texts: &[String],
    regex: bool,
    case_sensitive: bool,
) -> Result<Vec<regex::Regex>> {
    texts
        .iter()
        .filter(|t| !t.is_empty())
        .map(|t| {
            let pattern = if regex { t.clone() } else { regex::escape(t) };
            Ok(regex::RegexBuilder::new(&pattern)
                .case_insensitive(!case_sensitive)
                .build()?)
        })
        .collect()
}

/// Removes annotations that touch an area, with the form values they show.
fn drop_annotations(d: &mut Document, page: ObjectId, user_areas: &[Area]) -> Result<usize> {
    let annots: Vec<Object> = match d.get_dictionary(page)?.get(b"Annots") {
        Ok(a) => doc::resolve(d, a).as_array().cloned().unwrap_or_default(),
        Err(_) => return Ok(0),
    };
    let mut kept = Vec::with_capacity(annots.len());
    let mut values = Vec::new();
    for annot in &annots {
        let dict = doc::resolve(d, annot).as_dict().ok();
        let rect = dict
            .and_then(|a| doc::resolve(d, a.get(b"Rect").ok()?).as_array().ok())
            .and_then(|r| numbers::<4>(r))
            .map(|r| {
                [
                    r[0].min(r[2]),
                    r[1].min(r[3]),
                    r[0].max(r[2]),
                    r[1].max(r[3]),
                ]
            });
        if rect.is_some_and(|r| user_areas.iter().any(|a| intersects(a, &r))) {
            // A widget's value lives on the field, which may be the widget or its parent.
            values.extend(annot.as_reference().ok());
            values.extend(dict.and_then(|a| a.get(b"Parent").ok()?.as_reference().ok()));
        } else {
            kept.push(annot.clone());
        }
    }
    let removed = annots.len() - kept.len();
    if removed > 0 {
        d.get_dictionary_mut(page)?.set("Annots", kept);
        for id in values {
            if let Ok(field) = d.get_dictionary_mut(id) {
                // The appearance stream draws the value, and stays reachable through the form.
                for key in [&b"V"[..], b"DV", b"RV", b"AP"] {
                    field.remove(key);
                }
            }
        }
    }
    Ok(removed)
}

/// What was done about the texts to redact where a document speaks of itself outside
/// its pages.
#[derive(Default)]
struct Beside {
    /// Entries of the document information that held one of the texts.
    fields: Vec<String>,
    /// The metadata stream held one, and went: it repeats the document information.
    xmp: bool,
    bookmarks: usize,
    /// Attached files that hold one of the texts. They are reported, not opened up.
    attachments: Vec<String>,
}

/// `text` without what the patterns match.
fn struck(text: &str, patterns: &[regex::Regex]) -> String {
    patterns.iter().fold(text.to_string(), |text, pattern| {
        pattern.replace_all(&text, "").into_owned()
    })
}

/// Takes the texts out of the document information, the metadata stream and the
/// bookmark titles, and notes the attached files that hold them.
fn clean_beside_pages(d: &mut Document, patterns: &[regex::Regex]) -> Beside {
    let mut done = Beside::default();
    let info = d
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|i| i.as_reference().ok());
    if let Some(info) = info.and_then(|id| d.get_dictionary_mut(id).ok()) {
        for (key, value) in info.iter_mut() {
            let Some(text) = doc::text(value).filter(|_| matches!(value, Object::String(..)))
            else {
                continue;
            };
            let cleaned = struck(&text, patterns);
            if cleaned != text {
                *value = lopdf::text_string(&cleaned);
                done.fields.push(String::from_utf8_lossy(key).into_owned());
            }
        }
    }
    let holds = |bytes: &[u8]| {
        // Text is looked for as UTF-8 and, as PDF strings and office files often have
        // it, as UTF-16 in either byte order.
        let wide = |big: bool| -> String {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| {
                    if big {
                        u16::from_be_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_le_bytes([pair[0], pair[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        };
        [
            String::from_utf8_lossy(bytes).into_owned(),
            wide(true),
            wide(false),
        ]
        .iter()
        .any(|text| patterns.iter().any(|pattern| pattern.is_match(text)))
    };
    let content = |d: &Document, object: &Object| {
        doc::resolve(d, object)
            .as_stream()
            .ok()
            .and_then(|stream| stream_bytes(stream).ok())
    };
    let catalog = d.catalog().ok().cloned().unwrap_or_default();
    if catalog
        .get(b"Metadata")
        .ok()
        .and_then(|metadata| content(d, metadata))
        .is_some_and(|xmp| holds(&xmp))
        && let Ok(catalog) = d.catalog_mut()
    {
        catalog.remove(b"Metadata");
        done.xmp = true;
    }
    // Bookmarks: every entry reached from the root, each once.
    let link =
        |dict: &Dictionary, key: &[u8]| dict.get(key).ok().and_then(|o| o.as_reference().ok());
    let mut open: Vec<ObjectId> = catalog
        .get(b"Outlines")
        .ok()
        .and_then(|root| doc::resolve(d, root).as_dict().ok())
        .and_then(|root| link(root, b"First"))
        .into_iter()
        .collect();
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = open.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Ok(item) = d.get_dictionary_mut(id) else {
            continue;
        };
        open.extend(link(item, b"Next"));
        open.extend(link(item, b"First"));
        if let Some(title) = item.get(b"Title").ok().and_then(doc::text) {
            let cleaned = struck(&title, patterns);
            if cleaned != title {
                item.set("Title", lopdf::text_string(&cleaned));
                done.bookmarks += 1;
            }
        }
    }
    for object in d.objects.values() {
        let Ok(file) = object.as_dict() else {
            continue;
        };
        let Some(embedded) = file
            .get(b"EF")
            .ok()
            .and_then(|ef| doc::resolve(d, ef).as_dict().ok())
        else {
            continue;
        };
        let name = [b"UF".as_slice(), b"F"]
            .iter()
            .find_map(|key| doc::text(doc::resolve(d, file.get(key).ok()?)))
            .unwrap_or_default();
        let inside = embedded
            .iter()
            .filter_map(|(_, data)| content(d, data))
            .any(|bytes| holds(&bytes));
        if inside || patterns.iter().any(|pattern| pattern.is_match(&name)) {
            done.attachments.push(name);
        }
    }
    done.attachments.sort();
    done.attachments.dedup();
    done
}

pub fn redact(a: RedactArgs) -> Result<Value> {
    if a.rects.is_empty() && a.texts.is_empty() {
        bail!("nothing to redact: give rects (page:x0,y0,x1,y1) or texts to find");
    }
    let (pdf, _) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let total = pdf.pages().len() as u32;

    let mut areas: HashMap<u32, Vec<Area>> = HashMap::new();
    // The text each area was found by, for the plan a dry run reports.
    let mut found_by: HashMap<u32, Vec<Option<String>>> = HashMap::new();
    for spec in &a.rects {
        let (page, area) = parse_rect(spec, total)?;
        areas.entry(page).or_default().push(area);
        found_by.entry(page).or_default().push(None);
    }
    let mut matches = 0;
    let patterns = compile(&a.texts, a.regex, a.case_sensitive)?;
    if !a.texts.is_empty() {
        for n in pagespec::parse_or_all(a.pages.as_deref(), total)? {
            let found = text_areas(&layout::scan(&pdf.pages()[n as usize - 1]), &patterns);
            matches += found.len();
            for (area, text, _) in found {
                areas.entry(n).or_default().push(area);
                found_by.entry(n).or_default().push(Some(text));
            }
        }
    }
    let mut d = doc::load(&a.input, a.password.as_deref())?;
    let beside = clean_beside_pages(&mut d, &patterns);
    if areas.is_empty() && beside.fields.is_empty() && !beside.xmp && beside.bookmarks == 0 {
        bail!("none of the texts were found; nothing was written");
    }
    let ids = doc::page_ids(&d);
    let open = d.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let mut pages: Vec<u32> = areas.keys().copied().collect();
    pages.sort_unstable();
    let mut report = Vec::new();
    let progress = Progress::new("redact", pages.len());
    for &n in &pages {
        let id = *ids
            .get(n as usize - 1)
            .with_context(|| format!("page {n} is missing"))?;
        let page_areas = &areas[&n];
        // Layout coordinates have their origin at the top-left; user space is the page's own.
        let (to_user, _, visual_height) =
            visual_space(doc::page_box(&d, id), doc::rotation(&d, id));
        let user_areas: Vec<Area> = page_areas
            .iter()
            .map(|r| {
                bounds(
                    [(r[0], r[1]), (r[2], r[3])].map(|(x, y)| apply(to_user, x, visual_height - y)),
                )
            })
            .collect();

        let runs = glyph_runs(&pdf.pages()[n as usize - 1], false);
        let mut rewriter = Rewriter {
            pdf: &pdf,
            runs: &runs,
            next_run: 0,
            areas: page_areas,
            user_areas: &user_areas,
            stats: Stats::default(),
            replacements: &[],
            placed: Vec::new(),
            seen: HashMap::new(),
            collect_only: false,
            reflow: Vec::new(),
        };
        let content = d.get_page_content(id);
        let resources = own_resources(&d, id);
        let rewritten = rewriter
            .content(&mut d, &content, &resources, IDENTITY, 0)
            .with_context(|| format!("page {n}"))?;
        if rewriter.next_run != runs.len() {
            return Err(out_of_step()).with_context(|| format!("page {n}"));
        }
        let mut stats = rewriter.stats;
        let resources = match rewritten {
            Some((body, resources)) => {
                let mut stream = Stream::new(Dictionary::new(), body);
                let _ = stream.compress();
                let stream = d.add_object(stream);
                d.get_dictionary_mut(id)?.set("Contents", stream);
                resources
            }
            None => resources,
        };
        stats.annotations = drop_annotations(&mut d, id, &user_areas)?;

        let boxes: String = user_areas
            .iter()
            .map(|r| {
                format!(
                    "{:.2} {:.2} {:.2} {:.2} re\n",
                    r[0],
                    r[1],
                    r[2] - r[0],
                    r[3] - r[1]
                )
            })
            .collect();
        append_content(
            &mut d,
            id,
            open,
            format!("q\n0 0 0 rg\n{boxes}f\nQ\n"),
            resources,
        )?;
        let mut entry = json!({
            "page": n,
            "areas": page_areas.len(),
            "glyphs_removed": stats.glyphs,
            "paths_removed": stats.paths,
            "images_blanked": stats.images,
            "annotations_removed": stats.annotations,
        });
        if a.dry_run {
            entry["targets"] = page_areas
                .iter()
                .zip(&found_by[&n])
                .map(|(area, text)| {
                    let mut target = json!({"bbox": round_box(area)});
                    if let Some(text) = text {
                        target["text"] = json!(text);
                    }
                    target
                })
                .collect();
        }
        report.push(entry);
        progress.tick(json!({"page": n}));
    }

    // The structure tree can repeat page text as alternative descriptions, and nothing
    // ties those to the areas, so it goes as a whole. Unreferenced objects go too:
    // they include the original content streams.
    if let Ok(catalog) = d.catalog_mut() {
        catalog.remove(b"StructTreeRoot");
        catalog.remove(b"MarkInfo");
    }
    prune(&mut d);
    doc::seal(&mut d)?;
    let mut bytes = Vec::new();
    d.save_to(&mut bytes)?;

    // Proof before delivery: read the result back and look inside every area.
    let check = Pdf::new_with_password(bytes.clone(), a.password.as_deref().unwrap_or(""))
        .map_err(|_| anyhow!("the redacted file does not read back; nothing was written"))?;
    for &n in &pages {
        let left = glyph_runs(&check.pages()[n as usize - 1], true)
            .iter()
            .flatten()
            .filter(|g| g.hit(&areas[&n]).is_some())
            .count();
        if left > 0 {
            bail!(
                "page {n}: {left} glyphs are still inside a redacted area after rewriting; nothing was written"
            );
        }
    }
    if !a.dry_run {
        doc::write_atomic(&a.output, &bytes)?;
    }
    let mut result = json!({
        "output": a.output,
        "dry_run": a.dry_run,
        "text_matches": matches,
        "pages": report,
        "verified": true,
        "size_bytes": bytes.len(),
    });
    if !patterns.is_empty() {
        result["beside_pages"] = json!({
            "metadata_fields_cleaned": beside.fields,
            "xmp_metadata_removed": beside.xmp,
            "bookmarks_cleaned": beside.bookmarks,
            "attachments_holding_the_text": beside.attachments,
        });
    }
    Ok(result)
}

/// The matches on one page.
struct PageMatches {
    page: u32,
    /// Each match's area, the text it becomes and the text it was.
    areas: Vec<Area>,
    texts: Vec<String>,
    olds: Vec<String>,
    /// The box of every word on the page and its width, to tell how far a line may grow.
    words: Vec<Area>,
    width: f64,
}

/// How much closer together the rest of a line may be drawn to stay in its column,
/// as a share of its length.
const SQUEEZE: f64 = 0.08;

/// The changes of width on a page, each with what its line does about it, and for
/// each replacement by how much its line still runs over its column.
///
/// What follows a replacement on its line moves by the difference in width. A line
/// that would then run past the column it stands in is drawn closer together from
/// the replacement on, up to `SQUEEZE`; what is left over is the overflow.
fn plan_reflow(placed: &[Outcome], words: &[Area], width: f64) -> (Vec<Reflow>, Vec<f64>) {
    let mut changes: Vec<(usize, Reflow, f64)> = placed
        .iter()
        .enumerate()
        .filter_map(|(i, outcome)| match *outcome {
            Outcome::Written {
                new,
                old,
                at,
                along,
                ..
            } => Some((
                i,
                Reflow {
                    at,
                    along,
                    delta: new - old,
                    anchor: at,
                    factor: 1.0,
                },
                new,
            )),
            Outcome::Pending => None,
        })
        .collect();
    let mut overflow = vec![0.0; placed.len()];
    // Lines are told apart by their baseline. Only level text is fitted to a column.
    let level = |c: &Reflow| c.along.0 > 0.99;
    let mut lines: Vec<f64> = changes
        .iter()
        .filter(|c| level(&c.1))
        .map(|c| c.1.at.y)
        .collect();
    lines.sort_by(f64::total_cmp);
    lines.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    for baseline in lines {
        let on_line = |c: &Reflow| level(c) && (c.at.y - baseline).abs() < 1.0;
        let grown: f64 = changes
            .iter()
            .filter(|c| on_line(&c.1))
            .map(|c| c.1.delta)
            .sum();
        // Words of this line have the baseline inside their box; the others that
        // overlap it sideways show how wide the column is.
        let mine = |w: &Area| w[1] < baseline && baseline <= w[3] + 0.5;
        let extent = bounds(
            words
                .iter()
                .filter(|w| mine(w))
                .flat_map(|w| [(w[0], w[1]), (w[2], w[3])]),
        );
        if !extent[0].is_finite() {
            continue;
        }
        // Two lines or more above and below make a column. With fewer there is none to
        // keep to, and the line may run to a right margin as wide as the left one.
        let beside: Vec<&Area> = words
            .iter()
            .filter(|w| !mine(w) && w[0] < extent[2] && extent[0] < w[2])
            .collect();
        let mut others: Vec<i64> = beside.iter().map(|w| w[3].round() as i64).collect();
        others.sort_unstable();
        others.dedup();
        let left = words.iter().map(|w| w[0]).fold(extent[0], f64::min);
        let column = if others.len() >= 2 {
            beside.iter().map(|w| w[2]).fold(extent[2], f64::max)
        } else {
            (width - left).max(extent[2])
        };
        let over = extent[2] + grown - column;
        if over <= 0.5 {
            continue;
        }
        // The line is drawn together from the end of its first replacement.
        let Some(first) = changes
            .iter()
            .filter(|c| on_line(&c.1))
            .min_by(|a, b| a.1.at.x.total_cmp(&b.1.at.x))
            .map(|c| (c.1.at, c.2))
        else {
            continue;
        };
        let anchor = Point::new(first.0.x + first.1, first.0.y);
        let tail = extent[2] + grown - anchor.x;
        let taken = if tail > 0.0 {
            over.min(SQUEEZE * tail)
        } else {
            0.0
        };
        for (i, change, _) in changes.iter_mut().filter(|c| on_line(&c.1)) {
            change.anchor = anchor;
            if tail > 0.0 {
                change.factor = 1.0 - taken / tail;
            }
            overflow[*i] = over - taken;
        }
    }
    let moves = changes.iter().any(|c| c.1.delta.abs() > 0.05);
    let reflow = if moves {
        changes.into_iter().map(|c| c.1).collect()
    } else {
        Vec::new()
    };
    (reflow, overflow)
}

pub fn replace(a: ReplaceArgs) -> Result<Value> {
    if a.find.is_empty() {
        bail!("the text to find is empty");
    }
    let patterns = compile(std::slice::from_ref(&a.find), a.regex, a.case_sensitive)?;
    let (pdf, _) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let total = pdf.pages().len() as u32;

    // Per page: where each match is and what replaces it.
    let mut found: Vec<PageMatches> = Vec::new();
    for n in pagespec::parse_or_all(a.pages.as_deref(), total)? {
        let scanned = layout::scan(&pdf.pages()[n as usize - 1]);
        let matches = text_areas(&scanned, &patterns);
        if matches.is_empty() {
            continue;
        }
        let texts = matches
            .iter()
            .map(|(_, matched, _)| {
                if a.regex {
                    patterns[0].replace(matched, a.with.as_str()).into_owned()
                } else {
                    a.with.clone()
                }
            })
            .collect();
        let (areas, olds) = matches.into_iter().map(|m| (m.0, m.1)).unzip();
        found.push(PageMatches {
            page: n,
            areas,
            texts,
            olds,
            words: scanned.words.iter().map(|w| w.bbox).collect(),
            width: scanned.width,
        });
    }
    if found.is_empty() {
        bail!("'{}' was not found; nothing was written", a.find);
    }

    let mut d = doc::load(&a.input, a.password.as_deref())?;
    let ids = doc::page_ids(&d);
    let mut report = Vec::new();
    let mut total = 0;
    let progress = Progress::new("replace", found.len());
    for PageMatches {
        page: n,
        areas,
        texts,
        olds,
        words,
        width,
    } in &found
    {
        let id = *ids
            .get(*n as usize - 1)
            .with_context(|| format!("page {n} is missing"))?;
        let runs = glyph_runs(&pdf.pages()[*n as usize - 1], false);
        let content = d.get_page_content(id);
        let resources = own_resources(&d, id);
        let mut rewriter = Rewriter {
            pdf: &pdf,
            runs: &runs,
            next_run: 0,
            areas,
            user_areas: &[],
            stats: Stats::default(),
            replacements: texts,
            placed: vec![Outcome::Pending; areas.len()],
            seen: HashMap::new(),
            collect_only: true,
            reflow: Vec::new(),
        };
        // First pass: learn which characters the page's fonts can write.
        rewriter
            .content(&mut d, &content, &resources, IDENTITY, 0)
            .with_context(|| format!("page {n}"))?;
        // Second pass: write the replacements in place, which also measures them.
        rewriter.next_run = 0;
        rewriter.collect_only = false;
        let mut rewritten = rewriter
            .content(&mut d, &content, &resources, IDENTITY, 0)
            .with_context(|| format!("page {n}"))?;
        if rewriter.next_run != runs.len() {
            return Err(out_of_step()).with_context(|| format!("page {n}"));
        }
        // Third pass, when a width changed: the same again, with what follows each
        // replacement on its line moved along by the difference.
        let (reflow, overflow) = plan_reflow(&rewriter.placed, words, *width);
        if !reflow.is_empty() {
            rewriter.reflow = reflow;
            rewriter.next_run = 0;
            rewriter.placed = vec![Outcome::Pending; areas.len()];
            rewritten = rewriter
                .content(&mut d, &content, &resources, IDENTITY, 0)
                .with_context(|| format!("page {n}"))?;
            if rewriter.next_run != runs.len() {
                return Err(out_of_step()).with_context(|| format!("page {n}"));
            }
        }
        let resources = match rewritten {
            Some((body, resources)) => {
                let mut stream = Stream::new(Dictionary::new(), body);
                let _ = stream.compress();
                let stream = d.add_object(stream);
                d.get_dictionary_mut(id)?.set("Contents", stream);
                resources
            }
            None => resources,
        };

        let written = |own: bool| {
            rewriter
                .placed
                .iter()
                .filter(|o| matches!(o, Outcome::Written { own_font, .. } if *own_font == own))
                .count()
        };
        let (original, substituted) = (written(true), written(false));
        d.get_dictionary_mut(id)?.set("Resources", resources);
        // A match the page content does not draw itself sits in an annotation,
        // such as a form field's value; those are left alone and counted.
        total += original + substituted;
        let tenths = |v: f64| (v * 10.0).round() / 10.0;
        let mut entry = json!({
            "page": n,
            "replaced": original + substituted,
            "not_replaced_in_annotations": areas.len() - original - substituted,
            "in_original_font": original,
            "in_substitute_font": substituted,
            "overflow_pt": tenths(overflow.iter().copied().fold(0.0, f64::max)),
        });
        if a.dry_run {
            entry["matches"] = (0..areas.len())
                .map(|i| {
                    let mut m =
                        json!({"bbox": round_box(&areas[i]), "old": olds[i], "new": texts[i]});
                    match rewriter.placed[i] {
                        Outcome::Written {
                            own_font, new, old, ..
                        } => {
                            m["font"] = json!(if own_font { "original" } else { "substitute" });
                            m["width_change_pt"] = json!(tenths(new - old));
                            m["overflow_pt"] = json!(tenths(overflow[i]));
                        }
                        // Drawn by an annotation or form field, which replace leaves alone.
                        Outcome::Pending => m["font"] = Value::Null,
                    }
                    m
                })
                .collect();
        }
        report.push(entry);
        progress.tick(json!({"page": n}));
    }
    if total == 0 {
        bail!(
            "'{}' only occurs inside annotations or form fields, which replace does not edit; nothing was written",
            a.find
        );
    }
    prune(&mut d);
    let size = doc::save_unless(a.dry_run, &mut d, &a.output)?;
    Ok(json!({
        "output": a.output,
        "dry_run": a.dry_run,
        "replacements": total,
        "pages": report,
        "size_bytes": size,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrices_compose_and_invert() {
        let m = concat(
            [2.0, 0.0, 0.0, 3.0, 10.0, 20.0],
            [1.0, 0.0, 0.0, 1.0, 5.0, 5.0],
        );
        assert_eq!(apply(m, 1.0, 1.0), (17.0, 28.0));
        let back = invert(m).unwrap();
        let (x, y) = apply(back, 17.0, 28.0);
        assert!((x - 1.0).abs() < 1e-9 && (y - 1.0).abs() < 1e-9);
        assert!(invert([0.0; 6]).is_none());
    }

    #[test]
    fn zero_bits_clears_only_the_requested_samples() {
        let mut row = [0xffu8; 2];
        zero_bits(&mut row, 3, 6, 1);
        assert_eq!(row, [0b1110_0000, 0b0111_1111]);
        let mut rgb = [1u8; 9];
        zero_bits(&mut rgb, 1, 1, 24);
        assert_eq!(rgb, [1, 1, 1, 0, 0, 0, 1, 1, 1]);
    }

    #[test]
    fn rect_specs_are_validated() {
        assert_eq!(
            parse_rect("2: 10,20,5,40", 3).unwrap(),
            (2, [5.0, 20.0, 10.0, 40.0])
        );
        assert!(parse_rect("4:1,2,3,4", 3).is_err());
        assert!(parse_rect("1:1,2,3", 3).is_err());
        assert!(parse_rect("1:1,2,1,4", 3).is_err());
        assert!(parse_rect("x", 3).is_err());
    }
}
