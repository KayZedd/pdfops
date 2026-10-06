//! Page geometry: where each word is, and the tables that words and ruling lines form.
//!
//! Coordinates are in points with the origin at the top-left corner of the page
//! as displayed, y growing downwards. A pixel of `render` at 72 dpi is one point,
//! so boxes map straight onto rendered images.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Args, ValueEnum};
use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, Paint, SoftMask, interpret_page,
};
use hayro::hayro_syntax::content::TypedIter;
use hayro::hayro_syntax::content::ops::TypedInstruction;
use hayro::hayro_syntax::object::{Array, Dict, Name, Object, Stream, String as PdfString};
use hayro::hayro_syntax::page::{Page, Resources};
use hayro::kurbo::{Affine, BezPath, PathSeg, Point, Rect, Shape};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{doc, pagespec};

#[derive(ValueEnum, Deserialize, JsonSchema, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// One entry per text line: compact, enough to locate passages
    Lines,
    /// One entry per word: for exact marking
    Words,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct LayoutArgs {
    /// PDF file to analyse
    pub input: PathBuf,
    /// Pages to analyse, e.g. "1-3" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// Detail of the result (default: lines)
    #[arg(long, value_enum)]
    pub level: Option<Level>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(ValueEnum, Deserialize, JsonSchema, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TableFormat {
    /// Rows as arrays of cell strings
    Json,
    /// A Markdown table
    Markdown,
    /// Comma separated values
    Csv,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct TablesArgs {
    /// PDF file to read tables from
    pub input: PathBuf,
    /// Pages to scan, e.g. "4-6" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// How each table is returned (default: json)
    #[arg(long, value_enum)]
    pub format: Option<TableFormat>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

/// A run of glyphs without a space between them.
#[derive(Clone, Debug)]
pub struct Word {
    pub text: String,
    /// `[x0, y0, x1, y1]`, top-left origin.
    pub bbox: [f64; 4],
    /// Font size in points as displayed.
    pub size: f64,
    pub font: Option<Arc<str>>,
    pub bold: bool,
    pub italic: bool,
    /// Drawn in the invisible text mode: extracted as text, but nothing is painted.
    pub invisible: bool,
    /// None of its glyphs has an outline, as when a font is neither embedded nor known.
    /// Only established when a page is scanned for what can be seen.
    pub(crate) blank: bool,
    /// What the glyphs read as by their font, where text stated around some of them
    /// was taken instead. Only established when a page is scanned for what can be seen.
    pub(crate) drawn: Option<String>,
    /// Each glyph's byte offset into `text` and its box, for marking part of a word.
    pub(crate) parts: Vec<(usize, [f64; 4])>,
    /// Where the first glyph starts and where the next one would, on the baseline.
    start: Point,
    end: Point,
}

impl Word {
    pub fn center(&self) -> (f64, f64) {
        (
            (self.bbox[0] + self.bbox[2]) / 2.0,
            (self.bbox[1] + self.bbox[3]) / 2.0,
        )
    }

    /// What each glyph reads as, in the order the glyphs are drawn.
    pub(crate) fn pieces(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().enumerate().map(|(i, part)| {
            let end = self.parts.get(i + 1).map_or(self.text.len(), |next| next.0);
            &self.text[part.0..end]
        })
    }

    /// The word as it is read. `text` holds it as it is drawn, which for Hebrew and
    /// Arabic is back to front.
    pub fn read(&self) -> String {
        line_text(&[self])
    }
}

/// The order in which pieces of a line are read, given from left to right as they
/// are drawn; `None` where that is the same order.
///
/// A page holds text where it is drawn, so what runs from the right comes out back to
/// front: its last letter is the leftmost glyph. This undoes what a typesetter did to
/// the line: stretches that run from the right are turned round, and in a line that
/// runs from the right as a whole, so is their sequence. A glyph that reads as several
/// characters, as a ligature does, moves as one.
///
/// With each piece comes whether it stands in text running from the right. There a
/// bracket is drawn as its mirror image, and reads as the one it was typed as: see
/// `mirrored`.
pub(crate) fn reading_order(pieces: &[&str]) -> Option<Vec<(usize, bool)>> {
    use unicode_bidi::BidiClass::{AL, L, R};
    let (mut from_right, mut from_left) = (0usize, 0usize);
    for c in pieces.iter().flat_map(|p| p.chars()) {
        match unicode_bidi::bidi_class(c) {
            R | AL => from_right += 1,
            L => from_left += 1,
            _ => {}
        }
    }
    if from_right == 0 {
        return None;
    }
    let base = if from_right > from_left {
        unicode_bidi::Level::rtl()
    } else {
        unicode_bidi::Level::ltr()
    };
    let text = pieces.concat();
    let bidi = unicode_bidi::BidiInfo::new(&text, Some(base));
    let mut at = 0;
    let levels: Vec<u8> = pieces
        .iter()
        .map(|piece| {
            let level = bidi.levels.get(at).map_or(base.number(), |l| l.number());
            at += piece.len();
            level
        })
        .collect();
    let highest = *levels.iter().max()?;
    let lowest_odd = levels.iter().copied().filter(|l| l % 2 == 1).min()?;
    let mut order: Vec<usize> = (0..pieces.len()).collect();
    for level in (lowest_odd..=highest).rev() {
        let mut i = 0;
        while i < order.len() {
            let mut j = i;
            while j < order.len() && levels[order[j]] >= level {
                j += 1;
            }
            order[i..j].reverse();
            i = j.max(i + 1);
        }
    }
    Some(order.into_iter().map(|i| (i, levels[i] % 2 == 1)).collect())
}

impl PageLayout {
    /// The straight strokes on the page, as the boxes they take up.
    pub(crate) fn strokes(&self) -> Vec<[f64; 4]> {
        self.rules
            .iter()
            .map(|rule| {
                if rule.horizontal {
                    [rule.from, rule.at, rule.to, rule.at]
                } else {
                    [rule.at, rule.from, rule.at, rule.to]
                }
            })
            .collect()
    }
}

/// A bracket as it was typed, given as it is drawn in text running from the right.
pub(crate) fn mirrored(piece: &str) -> &str {
    match piece {
        "(" => ")",
        ")" => "(",
        "[" => "]",
        "]" => "[",
        "{" => "}",
        "}" => "{",
        "<" => ">",
        ">" => "<",
        "\u{ab}" => "\u{bb}",
        "\u{bb}" => "\u{ab}",
        other => other,
    }
}

/// The text of words that stand on one line, given from left to right, as it is read.
pub(crate) fn line_text(line: &[&Word]) -> String {
    let mut pieces: Vec<&str> = Vec::new();
    for (i, word) in line.iter().enumerate() {
        if i > 0 {
            pieces.push(" ");
        }
        pieces.extend(word.pieces());
    }
    match reading_order(&pieces) {
        Some(order) => order
            .into_iter()
            .map(|(i, turned)| {
                if turned {
                    mirrored(pieces[i])
                } else {
                    pieces[i]
                }
            })
            .collect(),
        None => pieces.concat(),
    }
}

/// A straight horizontal or vertical stroke, the raw material of table grids.
#[derive(Clone, Copy, Debug)]
struct Rule {
    horizontal: bool,
    /// y for a horizontal rule, x for a vertical one.
    at: f64,
    from: f64,
    to: f64,
}

pub struct PageLayout {
    pub width: f64,
    pub height: f64,
    pub words: Vec<Word>,
    rules: Vec<Rule>,
    /// Areas drawn on translucently or with a blend mode, which show what is under them.
    /// Only recorded when a page is scanned for what can be seen.
    pub(crate) veils: Vec<[f64; 4]>,
}

type FontInfo = (Option<Arc<str>>, bool, bool);

#[derive(Default)]
struct Collector {
    words: Vec<Word>,
    /// False once a space or a jump ended the current word.
    open: bool,
    rules: Vec<Rule>,
    fonts: HashMap<u128, FontInfo>,
    /// Whether to note what bears on visibility: glyphs without outlines, translucent drawing.
    visibility: bool,
    veils: Vec<[f64; 4]>,
    /// One entry per open transparency group: whether it lets what is under it show.
    groups: Vec<bool>,
    /// The marked content the page is expected to open, in order, each with the text
    /// it states for what it holds; see `marked_content`.
    marked: Vec<Marked>,
    /// How many of those have been opened.
    met: usize,
    /// Set once the page opened something other than what was expected.
    astray: bool,
    /// One entry per open marked content: its stated text, and whether a glyph took it.
    stated: Vec<Option<(String, bool)>>,
}

/// A marked content sequence: its tag, and the text it gives for its content.
type Marked = (Vec<u8>, Option<String>);

/// The marked content a page opens, in the order in which interpreting it meets them,
/// or nothing when none of it states text.
///
/// A producer that cannot say through a font what its glyphs read as, because shaping
/// reordered, split or stacked them, states the text around them: `/Span <</ActualText
/// ...>> BDC`. The interpreter reports that a sequence opens and with which tag, not
/// what it states, so that is read here beforehand, and `Collector` counts along.
fn marked_content(page: &Page<'_>, annotations: bool) -> Vec<Marked> {
    let appearances: Vec<Stream<'_>> = match annotations {
        true => page
            .raw()
            .get::<Array<'_>>(b"Annots")
            .into_iter()
            .flat_map(|annots| annots.iter::<Dict<'_>>().collect::<Vec<_>>())
            // The same choice of what to draw as the interpreter makes.
            .filter(|annot| annot.get::<u32>(b"F").unwrap_or(0) & 2 == 0)
            .filter_map(
                |annot| match annot.get::<Dict<'_>>(b"AP")?.get::<Object<'_>>(b"N")? {
                    Object::Stream(stream) => Some(stream),
                    Object::Dict(states) => annot
                        .get::<Name<'_>>(b"AS")
                        .and_then(|state| states.get::<Stream<'_>>(state))
                        .or_else(|| states.get::<Stream<'_>>(b"Off")),
                    _ => None,
                },
            )
            .collect(),
        false => Vec::new(),
    };
    let states = |data: &[u8]| data.windows(11).any(|w| w == b"/ActualText");
    let any = page.page_stream().is_some_and(states)
        || appearances
            .iter()
            .any(|stream| stream.decoded().is_ok_and(|data| states(&data)));
    let mut out = Vec::new();
    if any {
        walk(page.typed_operations(), page.resources(), 0, &mut out);
        for stream in &appearances {
            form(stream, page.resources(), 0, &mut out);
        }
    }
    out
}

/// Adds the marked content of a form to `out`, if the interpreter would draw it.
fn form(stream: &Stream<'_>, resources: &Resources<'_>, depth: u32, out: &mut Vec<Marked>) {
    let dict = stream.dict();
    if depth > 16 || dict.get::<[f32; 4]>(b"BBox").is_none() {
        return;
    }
    let Ok(data) = stream.decoded() else {
        return;
    };
    let own = dict.get::<Dict<'_>>(b"Resources").map(Resources::new);
    walk(
        TypedIter::new(&data),
        own.as_ref().unwrap_or(resources),
        depth + 1,
        out,
    );
}

fn walk(mut ops: TypedIter<'_>, resources: &Resources<'_>, depth: u32, out: &mut Vec<Marked>) {
    while let Some(op) = ops.next() {
        match op {
            TypedInstruction::BeginMarkedContentWithProperties(begin) => {
                // The properties stand in place, or under a name in the resources.
                let properties = match begin.1.clone() {
                    Object::Dict(dict) => Some(dict),
                    Object::Stream(stream) => Some(stream.dict().clone()),
                    Object::Name(name) => resources.properties.get::<Dict<'_>>(name),
                    _ => None,
                };
                let text = properties
                    .and_then(|p| p.get::<PdfString<'_>>(b"ActualText"))
                    .map(|text| doc::text_string(text.as_ref()));
                out.push((begin.0.to_vec(), text));
            }
            TypedInstruction::BeginMarkedContent(begin) => out.push((begin.0.to_vec(), None)),
            TypedInstruction::XObject(shown) => {
                let drawn = resources.get_x_object(shown.0).filter(|stream| {
                    stream
                        .dict()
                        .get::<Name<'_>>(b"Subtype")
                        .is_some_and(|kind| kind.as_ref() == b"Form")
                });
                if let Some(stream) = drawn {
                    form(&stream, resources, depth, out);
                }
            }
            _ => {}
        }
    }
}

impl Collector {
    /// Notes an area as drawn on translucently, if it is, or if a group it sits in is.
    fn veil(&mut self, sheer: bool, area: Rect) {
        if self.visibility && (sheer || self.groups.contains(&true)) {
            self.veils.push([area.x0, area.y0, area.x1, area.y1]);
        }
    }

    fn rule(&mut self, a: Point, b: Point) {
        // Slightly slanted strokes still count; scanned-in forms are rarely exact.
        if (a.y - b.y).abs() < 1.0 && (a.x - b.x).abs() > 3.0 {
            self.rules.push(Rule {
                horizontal: true,
                at: (a.y + b.y) / 2.0,
                from: a.x.min(b.x),
                to: a.x.max(b.x),
            });
        } else if (a.x - b.x).abs() < 1.0 && (a.y - b.y).abs() > 3.0 {
            self.rules.push(Rule {
                horizontal: false,
                at: (a.x + b.x) / 2.0,
                from: a.y.min(b.y),
                to: a.y.max(b.y),
            });
        }
    }
}

impl<'a> Device<'a> for Collector {
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn pop_clip(&mut self) {}

    fn begin_marked_content(&mut self, tag: &[u8], _: Option<i32>) {
        let expected = self.marked.get(self.met).filter(|(name, _)| name == tag);
        self.astray |= expected.is_none() && !self.marked.is_empty();
        self.stated
            .push(expected.and_then(|(_, text)| Some((text.clone()?, false))));
        self.met += 1;
    }

    fn end_marked_content(&mut self) {
        self.stated.pop();
    }

    fn push_transparency_group(
        &mut self,
        opacity: f32,
        mask: Option<SoftMask<'a>>,
        blend: BlendMode,
    ) {
        self.groups
            .push(opacity < 1.0 || mask.is_some() || blend != BlendMode::Normal);
    }

    fn pop_transparency_group(&mut self) {
        self.groups.pop();
    }

    fn draw_image(&mut self, _: Image<'a, '_>, props: ImageDrawProps<'a>) {
        let sheer = props.soft_mask.is_some() || props.blend_mode != BlendMode::Normal;
        // An image fills the unit square of its own space.
        self.veil(
            sheer,
            (props.transform * Rect::new(0.0, 0.0, 1.0, 1.0).to_path(0.1)).bounding_box(),
        );
    }

    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, mode: &DrawMode) {
        let path = props.transform * path.clone();
        let sheer = props.soft_mask.is_some()
            || props.blend_mode != BlendMode::Normal
            || matches!(&props.paint, Paint::Color(c) if c.to_rgba().to_rgba8()[3] < 255);
        if self.visibility {
            self.veil(sheer, path.bounding_box());
        }
        match mode {
            DrawMode::Stroke(_) | DrawMode::FillAndStroke(..) => {
                for seg in path.segments() {
                    if let PathSeg::Line(line) = seg {
                        self.rule(line.p0, line.p1);
                    }
                }
            }
            // Many producers draw rules as thin filled rectangles.
            DrawMode::Fill(_) => {
                let b = path.bounding_box();
                if b.height() < 2.0 && b.width() > 3.0 {
                    self.rule(
                        Point::new(b.x0, b.center().y),
                        Point::new(b.x1, b.center().y),
                    );
                } else if b.width() < 2.0 && b.height() > 3.0 {
                    self.rule(
                        Point::new(b.center().x, b.y0),
                        Point::new(b.center().x, b.y1),
                    );
                }
            }
            DrawMode::Invisible => {}
        }
    }

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, mode: &DrawMode) {
        let invisible = matches!(mode, DrawMode::Invisible);
        for glyph in run.glyphs() {
            // Glyph space is 1000 units per em.
            let to_page: Affine = props.transform * glyph.transform();
            let (advance, key) = match &**glyph {
                Glyph::Outline(o) => (
                    o.advance_width().unwrap_or(500.0) as f64,
                    Some(o.font_cache_key()),
                ),
                Glyph::Type3(_) => (500.0, None),
            };
            // A glyph the renderer has no shape for paints nothing, whatever a viewer shows.
            let blank =
                self.visibility && matches!(&**glyph, Glyph::Outline(o) if o.outline().is_empty());
            let origin = to_page * Point::new(0.0, 0.0);
            let end = to_page * Point::new(advance, 0.0);
            let up = (to_page * Point::new(0.0, 1000.0)) - origin;
            let size = up.hypot();
            if size < 0.01 {
                continue;
            }
            // The direction of writing is taken from an em rather than from the advance:
            // a mark advances by nothing, and would be near everything.
            let em = (to_page * Point::new(1000.0, 0.0)) - origin;
            let along = em / em.hypot().max(1e-9);
            // Where the text of what is being drawn is stated, the first glyph stands
            // for all of it and the others for none: they only add to its extent.
            let reading = || match glyph.as_unicode() {
                Some(BfString::Char(c)) => c.to_string(),
                Some(BfString::String(s)) => s,
                // Kept as a placeholder, so unmapped glyphs still occupy their place.
                None => "\u{fffd}".to_string(),
            };
            let within = self.stated.iter_mut().rev().find_map(|s| s.as_mut());
            // What the glyph itself reads as, kept beside stated text to compare them.
            let drawn = (self.visibility && within.is_some()).then(reading);
            let text = match within {
                Some((stated, taken)) if !*taken => {
                    *taken = true;
                    stated.clone()
                }
                Some(_) => {
                    if let Some(word) = self.words.last_mut().filter(|_| self.open) {
                        if let (Some(all), Some(drawn)) = (&mut word.drawn, &drawn) {
                            all.push_str(drawn);
                        }
                        let part = word.parts.last_mut().expect("a word has a glyph");
                        for (i, value) in [origin.x.min(end.x), origin.y.min(end.y)]
                            .into_iter()
                            .enumerate()
                        {
                            part.1[i] = part.1[i].min(value);
                            word.bbox[i] = word.bbox[i].min(value);
                        }
                        for (i, value) in [origin.x.max(end.x), origin.y.max(end.y)]
                            .into_iter()
                            .enumerate()
                        {
                            part.1[i + 2] = part.1[i + 2].max(value);
                            word.bbox[i + 2] = word.bbox[i + 2].max(value);
                        }
                        // The word goes on from the glyph that reaches furthest.
                        let further = end - word.end;
                        if along.x * further.x + along.y * further.y > 0.0 {
                            word.end = end;
                        }
                    }
                    continue;
                }
                None => reading(),
            };
            if text.chars().all(char::is_whitespace) {
                self.open = false;
                continue;
            }
            // A glyph box from the baseline: ascent and descent of a typical text face.
            let corners = [
                origin - up * 0.2,
                origin + up * 0.8,
                end - up * 0.2,
                end + up * 0.8,
            ];
            let bbox = [
                corners.iter().map(|p| p.x).fold(f64::INFINITY, f64::min),
                corners.iter().map(|p| p.y).fold(f64::INFINITY, f64::min),
                corners
                    .iter()
                    .map(|p| p.x)
                    .fold(f64::NEG_INFINITY, f64::max),
                corners
                    .iter()
                    .map(|p| p.y)
                    .fold(f64::NEG_INFINITY, f64::max),
            ];
            // The gap is judged along the baseline; across it there is more room, so that a
            // raised or lowered glyph (an asterisk, an exponent) stays in its word.
            let continues = self.open
                && self.words.last().is_some_and(|w| {
                    let gap = origin - w.end;
                    (gap.x * along.x + gap.y * along.y).abs() < 0.15 * size
                        && (gap.x * along.y - gap.y * along.x).abs() < 0.4 * size
                        && (w.size - size).abs() < 0.1 * size
                        && w.invisible == invisible
                });
            if continues {
                let word = self.words.last_mut().expect("checked above");
                word.blank &= blank;
                word.parts.push((word.text.len(), bbox));
                match (&mut word.drawn, &drawn) {
                    (Some(all), drawn) => all.push_str(drawn.as_ref().unwrap_or(&text)),
                    (all @ None, Some(drawn)) => *all = Some(format!("{}{drawn}", word.text)),
                    (None, None) => {}
                }
                word.text.push_str(&text);
                word.bbox = [
                    word.bbox[0].min(bbox[0]),
                    word.bbox[1].min(bbox[1]),
                    word.bbox[2].max(bbox[2]),
                    word.bbox[3].max(bbox[3]),
                ];
                word.end = end;
            } else {
                let (font, bold, italic) = match (key, &**glyph) {
                    (Some(key), Glyph::Outline(o)) => self
                        .fonts
                        .entry(key)
                        .or_insert_with(|| match o.font_data() {
                            Some(d) => (
                                d.postscript_name.map(Arc::from),
                                d.weight.is_some_and(|w| w >= 600),
                                d.is_italic,
                            ),
                            None => (None, false, false),
                        })
                        .clone(),
                    _ => (None, false, false),
                };
                self.words.push(Word {
                    text,
                    bbox,
                    size,
                    font,
                    bold,
                    italic,
                    invisible,
                    blank,
                    drawn,
                    parts: vec![(0, bbox)],
                    start: origin,
                    end,
                });
                self.open = true;
            }
        }
    }
}

/// Interprets a page and returns its words and ruling lines.
pub fn scan(page: &Page<'_>) -> PageLayout {
    scan_with(page, true, false)
}

/// Like `scan`, with or without what the page's annotations and form fields draw, and
/// optionally noting what bears on whether the words can be seen.
pub(crate) fn scan_with(page: &Page<'_>, annotations: bool, visibility: bool) -> PageLayout {
    let (w, h) = page.render_dimensions();
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, w as f64, h as f64),
        &cache,
        page.xref(),
        InterpreterSettings {
            render_annotations: annotations,
            ..Default::default()
        },
    );
    let mut collector = Collector {
        visibility,
        marked: marked_content(page, annotations),
        ..Default::default()
    };
    interpret_page(page, &mut context, &mut collector);
    if collector.astray || collector.met != collector.marked.len() && !collector.marked.is_empty() {
        // The page did not open what was read off it beforehand, so which text was
        // stated for what is not known: it is read again from its glyphs alone.
        let cache = InterpreterCache::new();
        let mut context = Context::new(
            page.initial_transform(true).to_kurbo(),
            Rect::new(0.0, 0.0, w as f64, h as f64),
            &cache,
            page.xref(),
            InterpreterSettings {
                render_annotations: annotations,
                ..Default::default()
            },
        );
        collector = Collector {
            visibility,
            ..Default::default()
        };
        interpret_page(page, &mut context, &mut collector);
    }
    PageLayout {
        width: w as f64,
        height: h as f64,
        words: collector.words,
        rules: collector.rules,
        veils: collector.veils,
    }
}

/// Groups words into text lines, top to bottom, each ordered left to right.
pub fn lines(words: &[Word]) -> Vec<Vec<&Word>> {
    let mut sorted: Vec<&Word> = words.iter().collect();
    sorted.sort_by(|a, b| a.center().1.total_cmp(&b.center().1));
    let mut lines: Vec<Vec<&Word>> = Vec::new();
    for word in sorted {
        // Same line when the vertical centres are closer than half the smaller font size.
        let fits = lines.last().is_some_and(|line| {
            let anchor = line[0];
            (anchor.center().1 - word.center().1).abs() < 0.5 * anchor.size.min(word.size)
        });
        if fits {
            lines.last_mut().expect("checked above").push(word);
        } else {
            lines.push(vec![word]);
        }
    }
    for line in &mut lines {
        line.sort_by(|a, b| a.bbox[0].total_cmp(&b.bbox[0]));
    }
    lines
}

/// The words as running text, in the order the page draws them.
///
/// Drawing order is the author's reading order far more often than any geometric
/// guess is: it keeps columns, sidebars and captions together. Within a line that
/// holds text running from the right, the order is the one it is read in.
pub fn plain_text(words: &[Word]) -> String {
    use unicode_bidi::BidiClass::{AL, R};
    let mut out = String::new();
    let mut line: Vec<&Word> = Vec::new();
    let finish = |line: &mut Vec<&Word>, out: &mut String| {
        let from_right = |c: char| matches!(unicode_bidi::bidi_class(c), R | AL);
        if line.iter().any(|w| w.text.chars().any(from_right)) {
            line.sort_by(|a, b| a.bbox[0].total_cmp(&b.bbox[0]));
            out.push_str(&line_text(line));
        } else {
            for (i, word) in line.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(&word.text);
            }
        }
        line.clear();
    };
    for word in words {
        if let Some(before) = line.last() {
            let drop = word.center().1 - before.center().1;
            let size = before.size.min(word.size);
            // On the same line: level with the word before, or, for text that is not
            // horizontal, starting about where that word ended.
            if drop.abs() >= 0.5 * size && (word.start - before.end).hypot() >= 1.2 * size {
                // More than a line's worth of space separates paragraphs.
                let apart = drop.abs() > 1.7 * before.size.max(word.size);
                finish(&mut line, &mut out);
                out.push_str(if apart { "\n\n" } else { "\n" });
            }
        }
        line.push(word);
    }
    finish(&mut line, &mut out);
    out
}

fn round(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn union(boxes: impl Iterator<Item = [f64; 4]>) -> [f64; 4] {
    boxes.fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |a, b| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        },
    )
}

fn describe(text: String, bbox: [f64; 4], word: &Word) -> Value {
    let mut v = json!({"text": text, "bbox": bbox.map(round), "size": round(word.size), "font": word.font.as_deref()});
    // Only stated when true, to keep the common case short.
    if word.bold {
        v["bold"] = json!(true);
    }
    if word.italic {
        v["italic"] = json!(true);
    }
    v
}

pub fn layout(a: LayoutArgs) -> Result<Value> {
    let (pdf, _) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let all = pdf.pages();
    let pages = pagespec::parse_or_all(a.pages.as_deref(), all.len() as u32)?;
    let level = a.level.unwrap_or(Level::Lines);
    let out: Vec<Value> = pages
        .par_iter()
        .map(|&n| {
            let page = scan(&all[n as usize - 1]);
            let grouped = lines(&page.words);
            let mut v =
                json!({"page": n, "width": round(page.width), "height": round(page.height)});
            if level == Level::Words {
                let words = grouped
                    .iter()
                    .flatten()
                    .map(|w| describe(w.read(), w.bbox, w));
                v["words"] = words.collect();
            } else {
                let lines = grouped.iter().map(|line| {
                    describe(line_text(line), union(line.iter().map(|w| w.bbox)), line[0])
                });
                v["lines"] = lines.collect();
            }
            v
        })
        .collect();
    Ok(json!({
        "file": a.input,
        "units": "points, origin top-left, y down; equals pixels of render at 72 dpi",
        "pages": out,
    }))
}

/// A detected table: its outline and the text of each cell, row by row.
pub struct Table {
    pub bbox: [f64; 4],
    pub rows: Vec<Vec<String>>,
    /// "lines" when ruling lines gave the grid, "alignment" when text columns did.
    pub method: &'static str,
}

/// Merges coordinates closer than `tolerance` and returns them sorted.
fn cluster(mut values: Vec<f64>, tolerance: f64) -> Vec<f64> {
    values.sort_by(f64::total_cmp);
    let mut out: Vec<(f64, usize)> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some((mean, n)) if v - *mean <= tolerance => {
                *mean = (*mean * *n as f64 + v) / (*n as f64 + 1.0);
                *n += 1;
            }
            _ => out.push((v, 1)),
        }
    }
    out.into_iter().map(|(v, _)| v).collect()
}

/// Text of the words inside a cell, in reading order.
fn cell_text(words: &[&Word]) -> String {
    let owned: Vec<Word> = words.iter().map(|w| (*w).clone()).collect();
    lines(&owned)
        .iter()
        .map(|line| line_text(line))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Tables whose cells are drawn with ruling lines.
fn ruled_tables(page: &PageLayout) -> Vec<Table> {
    // Rules that touch belong to the same grid.
    let rules = &page.rules;
    let touches = |a: &Rule, b: &Rule| {
        let slack = 2.0;
        if a.horizontal == b.horizontal {
            (a.at - b.at).abs() < slack && a.from <= b.to + slack && b.from <= a.to + slack
        } else {
            a.at >= b.from - slack
                && a.at <= b.to + slack
                && b.at >= a.from - slack
                && b.at <= a.to + slack
        }
    };
    let mut group: Vec<usize> = (0..rules.len()).collect();
    fn root(group: &mut [usize], mut i: usize) -> usize {
        while group[i] != i {
            group[i] = group[group[i]];
            i = group[i];
        }
        i
    }
    for i in 0..rules.len() {
        for j in i + 1..rules.len() {
            if touches(&rules[i], &rules[j]) {
                let (a, b) = (root(&mut group, i), root(&mut group, j));
                group[a] = b;
            }
        }
    }
    let mut grids: HashMap<usize, Vec<&Rule>> = HashMap::new();
    for (i, rule) in rules.iter().enumerate() {
        grids.entry(root(&mut group, i)).or_default().push(rule);
    }

    let mut tables = Vec::new();
    for grid in grids.into_values() {
        let xs = cluster(
            grid.iter()
                .filter(|r| !r.horizontal)
                .map(|r| r.at)
                .collect(),
            2.0,
        );
        let ys = cluster(
            grid.iter().filter(|r| r.horizontal).map(|r| r.at).collect(),
            2.0,
        );
        // A grid needs at least one cell boundary in each direction beyond its frame.
        if xs.len() < 2 || ys.len() < 2 || (xs.len() - 1) * (ys.len() - 1) < 2 {
            continue;
        }
        let mut rows = vec![vec![Vec::<&Word>::new(); xs.len() - 1]; ys.len() - 1];
        let mut filled = 0;
        for word in &page.words {
            let (cx, cy) = word.center();
            let col = xs.windows(2).position(|x| cx >= x[0] && cx < x[1]);
            let row = ys.windows(2).position(|y| cy >= y[0] && cy < y[1]);
            if let (Some(row), Some(col)) = (row, col) {
                rows[row][col].push(word);
                filled += 1;
            }
        }
        if filled == 0 {
            continue;
        }
        tables.push(Table {
            bbox: [xs[0], ys[0], xs[xs.len() - 1], ys[ys.len() - 1]],
            rows: rows
                .iter()
                .map(|row| row.iter().map(|cell| cell_text(cell)).collect())
                .collect(),
            method: "lines",
        });
    }
    tables
}

/// Words of a line merged into cells: a gap wider than a generous space starts a new cell.
fn segments<'w>(line: &[&'w Word]) -> Vec<Vec<&'w Word>> {
    let mut out: Vec<Vec<&Word>> = Vec::new();
    for &word in line {
        let joins = out
            .last()
            .and_then(|s| s.last())
            .is_some_and(|prev| word.bbox[0] - prev.bbox[2] < 0.9 * prev.size);
        if joins {
            out.last_mut().expect("checked above").push(word);
        } else {
            out.push(vec![word]);
        }
    }
    out
}

/// One line of text as cells, each a run of words.
type Cells<'w> = Vec<Vec<&'w Word>>;

fn extent(cell: &[&Word]) -> (f64, f64) {
    (cell[0].bbox[0], cell[cell.len() - 1].bbox[2])
}

/// Whether a line belongs to a list of contents: a title, a row of dots, a page number.
fn has_leaders(line: &Cells<'_>) -> bool {
    let words = || line.iter().flatten();
    let loose: usize = words()
        .filter(|w| w.text.chars().all(|c| matches!(c, '.' | '·' | '…')))
        .map(|w| w.text.chars().count())
        .sum();
    loose >= 4 || words().any(|w| w.text.contains("...."))
}

/// How many rows may cross a gap between two columns without closing it.
///
/// A heading set across two columns, or one cell that runs over, should not
/// make one column of two; in a short table every row counts.
fn crossings_allowed(rows: usize) -> usize {
    if rows >= 4 {
        (rows * 15 / 100).max(1)
    } else {
        0
    }
}

/// The stretches of x that hold a column each, from the cells of a block's rows.
///
/// Columns are first the stretches covered by some cell. A stretch is then cut
/// where few rows cover it while more do on both sides: that is a gap between
/// two columns with something lying across it.
fn column_spans(rows: &[&Cells<'_>]) -> Vec<(f64, f64)> {
    let mut spans: Vec<(f64, f64)> = rows
        .iter()
        .flat_map(|row| row.iter().map(|cell| extent(cell)))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut covered: Vec<(f64, f64)> = Vec::new();
    for &(from, to) in &spans {
        match covered.last_mut() {
            Some(last) if from <= last.1 + 2.0 => last.1 = last.1.max(to),
            _ => covered.push((from, to)),
        }
    }
    let allowed = crossings_allowed(rows.len());
    if allowed == 0 {
        return covered;
    }
    let mut columns = Vec::new();
    for (from, to) in covered {
        let mut edges: Vec<f64> = spans
            .iter()
            .flat_map(|s| [s.0, s.1])
            .filter(|x| *x >= from && *x <= to)
            .collect();
        edges.sort_by(f64::total_cmp);
        edges.dedup();
        // Each stretch between two edges with the number of cells lying over it.
        let pieces: Vec<(f64, f64, usize)> = edges
            .windows(2)
            .map(|e| {
                let middle = (e[0] + e[1]) / 2.0;
                let over = spans
                    .iter()
                    .filter(|s| s.0 <= middle && middle <= s.1)
                    .count();
                (e[0], e[1], over)
            })
            .collect();
        let mut start = from;
        let mut i = 0;
        while i < pieces.len() {
            if pieces[i].2 > allowed {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < pieces.len() && pieces[j].2 <= allowed {
                j += 1;
            }
            let (low_from, low_to) = (pieces[i].0, pieces[j - 1].1);
            // A dip counts only with busier stretches on both sides of it.
            let busy_before = pieces[..i].iter().any(|p| p.2 > allowed);
            let busy_after = pieces[j..].iter().any(|p| p.2 > allowed);
            if busy_before && busy_after && low_to - low_from >= 3.0 {
                columns.push((start, low_from));
                start = low_to;
            }
            i = j;
        }
        columns.push((start, to));
    }
    columns
}

/// The words of one line, sorted into the block's columns.
fn into_columns<'w>(line: &Cells<'w>, columns: &[(f64, f64)]) -> Cells<'w> {
    let overlap = |from: f64, to: f64, column: &(f64, f64)| to.min(column.1) - from.max(column.0);
    let best = |from: f64, to: f64| {
        (0..columns.len())
            .max_by(|&a, &b| {
                overlap(from, to, &columns[a]).total_cmp(&overlap(from, to, &columns[b]))
            })
            .unwrap_or(0)
    };
    let mut row: Cells<'w> = vec![Vec::new(); columns.len()];
    for cell in line {
        let (from, to) = extent(cell);
        let of = |w: &Word| best(w.bbox[0], w.bbox[2]);
        // Two columns set close together come as one run of words. It is taken apart
        // where a change of column coincides with more room than a space takes.
        let parts = cell
            .windows(2)
            .any(|p| of(p[0]) != of(p[1]) && p[1].bbox[0] - p[0].bbox[2] > 0.5 * p[0].size);
        if parts {
            for &word in cell {
                row[of(word)].push(word);
            }
        } else {
            row[best(from, to)].extend(cell.iter().copied());
        }
    }
    row
}

fn joined(cell: &[&Word]) -> String {
    line_text(cell)
}

/// Whether the line continues the row above it rather than starting a row of its own.
///
/// `tighter` says that the line sits closer to the one above than rows of this table do.
fn continues(above: &Cells<'_>, line: &Cells<'_>, tighter: bool) -> bool {
    let filled = |row: &Cells<'_>| row.iter().filter(|c| !c.is_empty()).count();
    let wordy = |cell: &[&Word]| cell.iter().any(|w| w.text.chars().any(char::is_alphabetic));
    if line[0].is_empty() {
        // A line that fills just the cells the row above fills, beyond the first, is a
        // row like it whose first cell is left empty: a group named once, or a column
        // in which something beside the table stands. A cell that wrapped reads on in
        // small letters, and rarely do all cells of a row wrap at once.
        let reads_on = line
            .iter()
            .flatten()
            .next()
            .and_then(|w| w.text.chars().next())
            .is_some_and(char::is_lowercase);
        let alike = filled(line) >= 2
            && (above[0].is_empty() || !reads_on)
            && line
                .iter()
                .zip(above)
                .skip(1)
                .all(|(cell, over)| cell.is_empty() == over.is_empty());
        // Text under cells that have text, with nothing in the first column: a cell
        // that wrapped. Figures alone are a row, such as a total set under its column.
        return !alike
            && line
                .iter()
                .zip(above)
                .all(|(cell, over)| cell.is_empty() || !over.is_empty())
            && line.iter().any(|cell| wordy(cell));
    }
    // A label with nothing beside it, then a line that reads on: the label wrapped,
    // and the figures stand on its last line.
    if above[0].is_empty() || filled(above) != 1 || above[0].iter().any(|w| !wordy(&[*w])) {
        return false;
    }
    let indent = line[0][0].bbox[0] - above[0][0].bbox[0];
    let lowercase = line[0][0]
        .text
        .chars()
        .next()
        .is_some_and(char::is_lowercase);
    lowercase || (tighter && indent >= 0.4 * line[0][0].size)
}

/// The rows of a block whose cells are centred on their rows, as the lines each takes
/// up; `None` for a block that is not set that way.
///
/// A cell of one line beside a cell of two stands half a line lower than the first of
/// those two. So there are lines closer together than the lines of any one cell are,
/// filling different columns: parts of one row, which no two rows of a table are.
/// Such a table has room between its rows, more than between the lines of a cell,
/// and that room is where a row ends.
fn bands(block: &[Cells<'_>], columns: &[(f64, f64)]) -> Option<Vec<std::ops::Range<usize>>> {
    let top = |line: &Cells<'_>| line[0][0].center().1;
    let filled: Vec<Vec<bool>> = block
        .iter()
        .map(|line| {
            into_columns(line, columns)
                .iter()
                .map(|cell| !cell.is_empty())
                .collect()
        })
        .collect();
    // The pitch of the lines within a cell: the least distance between two lines
    // that both have something in one column.
    let size = block[0][0][0].size;
    let pitch = (0..columns.len())
        .flat_map(|column| {
            let tops: Vec<f64> = (0..block.len())
                .filter(|&i| filled[i][column])
                .map(|i| top(&block[i]))
                .collect();
            tops.windows(2).map(|p| p[1] - p[0]).collect::<Vec<_>>()
        })
        .filter(|d| *d > 0.8 * size)
        .fold(f64::INFINITY, f64::min);
    if !pitch.is_finite() {
        return None;
    }
    let staggered = (0..block.len() - 1).any(|i| {
        top(&block[i + 1]) - top(&block[i]) <= 0.75 * pitch
            && filled[i]
                .iter()
                .zip(&filled[i + 1])
                .all(|(a, b)| !(*a && *b))
    });
    if !staggered {
        return None;
    }
    let mut out = Vec::new();
    let mut from = 0;
    for i in 0..block.len() - 1 {
        if top(&block[i + 1]) - top(&block[i]) > 1.2 * pitch {
            out.push(from..i + 1);
            from = i + 1;
        }
    }
    out.push(from..block.len());
    // Rows set this way come with room between every two of them. One or two such
    // stretches are lines standing beside something else, a drawing's labels.
    (out.len() >= 3).then_some(out)
}

/// Whether rows are two columns of running text rather than a table: most cells on
/// both sides are whole lines of prose, which a page set in two columns gives.
fn prose(rows: &[Cells<'_>]) -> bool {
    let Some(columns) = rows.first().map(Vec::len).filter(|n| *n == 2) else {
        return false;
    };
    (0..columns).all(|column| {
        let cells: Vec<&Vec<&Word>> = rows
            .iter()
            .map(|row| &row[column])
            .filter(|cell| !cell.is_empty())
            .collect();
        let wordy = cells
            .iter()
            .filter(|cell| {
                cell.iter()
                    .map(|w| w.text.split(' ').count())
                    .sum::<usize>()
                    >= 6
            })
            .count();
        !cells.is_empty() && wordy * 2 > cells.len()
    })
}

/// Whether rows are the items of a list: a number, a letter or a bullet in front, and
/// what it says beside it.
fn listed(rows: &[Cells<'_>]) -> bool {
    let marker = |cell: &Vec<&Word>| {
        let [word] = cell.as_slice() else {
            return false;
        };
        let text = word.text.trim_end_matches(['.', ')', ':']);
        let short = text.chars().count() <= 3
            && text.chars().all(char::is_alphanumeric)
            && text.len() < word.text.len();
        short
            || matches!(
                word.text.as_str(),
                "\u{2022}" | "-" | "\u{2013}" | "*" | "\u{25aa}"
            )
    };
    rows.iter().all(|row| row.len() == 2)
        && rows.iter().all(|row| row[0].is_empty() || marker(&row[0]))
        && rows.iter().any(|row| !row[0].is_empty())
}

/// Builds the table of a block of lines, or the tables of its parts where a line of
/// running text cuts through it.
fn block_tables(block: &[Cells<'_>], tables: &mut Vec<Table>) {
    let wide: Vec<&Cells<'_>> = block.iter().filter(|line| line.len() >= 2).collect();
    if wide.len() < 2 {
        return;
    }
    let columns = column_spans(&wide);
    if columns.len() < 2 {
        return;
    }
    // A line in one piece that lies across columns is text around the table, not a row.
    let crosses = |line: &Cells<'_>| {
        let (from, to) = extent(&line[0]);
        line.len() == 1
            && columns
                .iter()
                .filter(|c| to.min(c.1) - from.max(c.0) > 1.0)
                .count()
                >= 2
    };
    if let Some(cut) = block.iter().position(crosses) {
        block_tables(&block[..cut], tables);
        block_tables(&block[cut + 1..], tables);
        return;
    }
    if block.iter().filter(|line| has_leaders(line)).count() * 2 >= block.len() {
        return;
    }

    let top = |line: &Cells<'_>| line[0][0].center().1;
    let mut pitches: Vec<f64> = block.windows(2).map(|p| top(&p[1]) - top(&p[0])).collect();
    pitches.sort_by(f64::total_cmp);
    let pitch = pitches.get(pitches.len() / 2).copied().unwrap_or(0.0);

    let mut rows: Vec<Cells<'_>> = Vec::new();
    if let Some(bands) = bands(block, &columns) {
        // Rows set apart by room, each of as many lines as its fullest cell has.
        for band in bands {
            let mut row: Cells<'_> = vec![Vec::new(); columns.len()];
            for line in &block[band] {
                for (cell, more) in row.iter_mut().zip(into_columns(line, &columns)) {
                    cell.extend(more);
                }
            }
            rows.push(row);
        }
    } else {
        for (i, line) in block.iter().enumerate() {
            let row = into_columns(line, &columns);
            let gap = if i > 0 {
                top(line) - top(&block[i - 1])
            } else {
                0.0
            };
            let close = gap <= 1.6 * line[0][0].size;
            match rows.last_mut() {
                Some(above) if close && continues(above, &row, gap <= 0.93 * pitch) => {
                    for (cell, more) in above.iter_mut().zip(row) {
                        cell.extend(more);
                    }
                }
                _ => rows.push(row),
            }
        }
    }
    if prose(&rows) || listed(&rows) {
        return;
    }
    // A last line that stands alone under the table was only taken on trial.
    if block.last().is_some_and(|line| line.len() == 1) && rows.len() > 1 {
        let last = &rows[rows.len() - 1];
        if last.iter().filter(|c| !c.is_empty()).count() == 1 && !last[0].is_empty() {
            rows.pop();
        }
    }
    if rows.len() < 2 {
        return;
    }
    tables.push(Table {
        bbox: union(rows.iter().flatten().flatten().map(|w| w.bbox)),
        rows: rows
            .iter()
            .map(|row| row.iter().map(|cell| joined(cell)).collect())
            .collect(),
        method: "alignment",
    });
}

/// Tables without ruling lines, found from text that lines up in columns.
fn aligned_tables(page: &PageLayout, taken: &[[f64; 4]]) -> Vec<Table> {
    let inside = |w: &Word| {
        let (cx, cy) = w.center();
        taken
            .iter()
            .any(|b| cx >= b[0] && cx <= b[2] && cy >= b[1] && cy <= b[3])
    };
    let free: Vec<Word> = page.words.iter().filter(|w| !inside(w)).cloned().collect();
    let lines: Vec<Cells<'_>> = lines(&free).iter().map(|l| segments(l)).collect();
    let top = |line: &Cells<'_>| line[0][0].center().1;
    // A line in one piece may sit inside a table: a heading over a group of rows, a
    // label that wrapped, a single figure. It is taken along when the table goes on
    // within two lines of it and the lines are not far apart.
    let near = |a: usize, b: usize| top(&lines[b]) - top(&lines[a]) <= 2.5 * lines[b][0][0].size;
    // Rows may stand further apart than that, where cells of several lines are given
    // room. A line then belongs to the table by its cells: they begin, end or are
    // centred where cells of the lines before it are.
    let within_reach =
        |a: usize, b: usize| top(&lines[b]) - top(&lines[a]) <= 6.0 * lines[b][0][0].size;
    let lined_up = |start: usize, end: usize, next: usize| {
        let marks = |cell: &Vec<&Word>| {
            let (from, to) = extent(cell);
            [from, to, (from + to) / 2.0]
        };
        let known: Vec<[f64; 3]> = lines[start..end]
            .iter()
            .flat_map(|line| line.iter().map(marks))
            .collect();
        let matching = lines[next]
            .iter()
            .filter(|cell| {
                let own = marks(cell);
                known
                    .iter()
                    .any(|other| (0..3).any(|k| (own[k] - other[k]).abs() < 2.0))
            })
            .count();
        matching >= 2 && matching * 2 >= lines[next].len()
    };

    let mut tables = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        if lines[start].len() < 2 {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        loop {
            let ahead = (end..lines.len().min(end + 3))
                .take_while(|&k| k == end || lines[k - 1].len() < 2)
                .find(|&k| lines[k].len() >= 2);
            match ahead {
                Some(next)
                    if (end..=next).all(|k| k == next && next == end || near(k - 1, k))
                        || (end..=next).all(|k| within_reach(k - 1, k))
                            && lined_up(start, end, next) =>
                {
                    end = next + 1;
                }
                _ => break,
            }
        }
        // One more line may be the wrapped end of the last row.
        let trailing = end < lines.len() && lines[end].len() == 1 && near(end - 1, end);
        block_tables(&lines[start..end + usize::from(trailing)], &mut tables);
        start = end;
    }
    tables
}

/// All tables of a page, top to bottom.
pub fn tables_on(page: &PageLayout) -> Vec<Table> {
    let mut tables = ruled_tables(page);
    let taken: Vec<[f64; 4]> = tables.iter().map(|t| t.bbox).collect();
    tables.extend(aligned_tables(page, &taken));
    tables.sort_by(|a, b| a.bbox[1].total_cmp(&b.bbox[1]));
    tables
}

fn markdown(rows: &[Vec<String>]) -> String {
    let line = |cells: &[String]| {
        let escaped: Vec<String> = cells.iter().map(|c| c.replace('|', "\\|")).collect();
        format!("| {} |", escaped.join(" | "))
    };
    let mut out = Vec::with_capacity(rows.len() + 1);
    for (i, row) in rows.iter().enumerate() {
        out.push(line(row));
        if i == 0 {
            out.push(format!("|{}", " --- |".repeat(row.len())));
        }
    }
    out.join("\n")
}

fn csv(rows: &[Vec<String>]) -> String {
    let field = |c: &String| {
        if c.contains([',', '"', '\n']) {
            format!("\"{}\"", c.replace('"', "\"\""))
        } else {
            c.clone()
        }
    };
    rows.iter()
        .map(|r| r.iter().map(field).collect::<Vec<_>>().join(","))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn tables(a: TablesArgs) -> Result<Value> {
    let (pdf, _) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let all = pdf.pages();
    let pages = pagespec::parse_or_all(a.pages.as_deref(), all.len() as u32)?;
    let format = a.format.unwrap_or(TableFormat::Json);
    let found: Vec<Value> = pages
        .par_iter()
        .flat_map_iter(|&n| {
            tables_on(&scan(&all[n as usize - 1]))
                .into_iter()
                .map(move |t| {
                    let mut v = json!({
                        "page": n,
                        "bbox": t.bbox.map(round),
                        "rows": t.rows.len(),
                        "columns": t.rows.first().map_or(0, Vec::len),
                        "detected_by": t.method,
                    });
                    match format {
                        TableFormat::Json => v["cells"] = json!(t.rows),
                        TableFormat::Markdown => v["markdown"] = json!(markdown(&t.rows)),
                        TableFormat::Csv => v["csv"] = json!(csv(&t.rows)),
                    }
                    v
                })
        })
        .collect();
    Ok(json!({"file": a.input, "tables": found}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_merges_close_values() {
        assert_eq!(
            cluster(vec![10.0, 50.0, 10.6, 49.5, 90.0], 2.0),
            [10.3, 49.75, 90.0]
        );
    }

    #[test]
    fn markdown_and_csv_escape_their_separators() {
        let rows = vec![
            vec!["a|b".to_string(), "c".to_string()],
            vec!["1,5".to_string(), "say \"hi\"".to_string()],
        ];
        assert_eq!(
            markdown(&rows),
            "| a\\|b | c |\n| --- | --- |\n| 1,5 | say \"hi\" |"
        );
        assert_eq!(csv(&rows), "a|b,c\n\"1,5\",\"say \"\"hi\"\"\"");
    }

    #[test]
    fn text_running_from_the_right_is_read_back_in_its_own_order() {
        // Each piece is what one glyph reads as; they come as drawn, from the left.
        let read = |drawn: &[&str]| -> Option<String> {
            reading_order(drawn).map(|order| {
                order
                    .into_iter()
                    .map(|(i, turned)| if turned { mirrored(drawn[i]) } else { drawn[i] })
                    .collect()
            })
        };
        let glyphs = |text: &'static str| -> Vec<&'static str> {
            text.char_indices()
                .map(|(i, c)| &text[i..i + c.len_utf8()])
                .collect()
        };
        assert_eq!(read(&glyphs("plain text, 12.5 (a)")), None);
        // A Hebrew word: its last letter is drawn first.
        assert_eq!(
            read(&glyphs("\u{5dd}\u{5d5}\u{5dc}\u{5e9}")).unwrap(),
            "\u{5e9}\u{5dc}\u{5d5}\u{5dd}"
        );
        // Two Hebrew words in an English sentence change places as well, and the
        // sentence around them stays.
        assert_eq!(
            read(&glyphs(
                "He said \u{5dd}\u{5dc}\u{5d5}\u{5e2} \u{5dd}\u{5d5}\u{5dc}\u{5e9}, then left."
            ))
            .unwrap(),
            "He said \u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{5e2}\u{5d5}\u{5dc}\u{5dd}, then left."
        );
        // A line that runs from the right as a whole: the Latin and the number in it
        // keep their order. The brackets are drawn as the eye expects them, so the one
        // on the right, which is read first, has the shape of a closing one.
        assert_eq!(
            read(&glyphs(
                ".\u{5d5}\u{5db}\u{5d5}\u{5ea}\u{5d1} PDF 1.7 (\u{5d8}\u{5e4}\u{5e9}\u{5de}) \u{5d4}\u{5d6}"
            ))
            .unwrap(),
            "\u{5d6}\u{5d4} (\u{5de}\u{5e9}\u{5e4}\u{5d8}) PDF 1.7 \u{5d1}\u{5ea}\u{5d5}\u{5db}\u{5d5}."
        );
        // A ligature reads as two letters and moves as one glyph.
        assert_eq!(
            read(&["\u{645}", "\u{644}\u{627}", "\u{633}"]).unwrap(),
            "\u{633}\u{644}\u{627}\u{645}"
        );
    }
}
