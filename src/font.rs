//! Fonts for text that pdfops draws itself (stamps and form field appearances).
//!
//! Latin-1 text uses the built-in Helvetica, which costs no space. Anything
//! else needs real glyphs, so a font file is subsetted to the characters used
//! and embedded as a composite font with a ToUnicode map.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

/// Helvetica advance widths for ASCII 32..=126, in 1/1000 em (Adobe AFM).
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Width of WinAnsi `bytes` set in Helvetica at `size` points.
pub fn helvetica_width(bytes: &[u8], size: f64) -> f64 {
    let units: u32 = bytes
        .iter()
        .map(|&b| {
            HELVETICA
                .get((b as usize).wrapping_sub(32))
                .copied()
                .unwrap_or(556) as u32
        })
        .sum();
    units as f64 * size / 1000.0
}

/// Encodes text for a WinAnsi font, if every character is Latin-1.
pub fn winansi(text: &str) -> Option<Vec<u8>> {
    text.chars()
        .map(|c| match c as u32 {
            0x20..=0x7e | 0xa0..=0xff => Some(c as u8),
            _ => None,
        })
        .collect()
}

/// A PDF literal string, escaped.
pub fn literal(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() + 2);
    s.push('(');
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                s.push('\\');
                s.push(b as char);
            }
            0x20..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\{b:03o}")),
        }
    }
    s.push(')');
    s
}

/// A font object in a document, able to encode and measure text for it.
pub struct TextFont {
    pub id: ObjectId,
    /// The embedded font's data for laying text out; `None` for a built-in font.
    embedded: Option<Embedded>,
    /// Which built-in metrics apply when nothing is embedded.
    builtin: Style,
}

/// What it takes to lay text out in an embedded font.
struct Embedded {
    data: Vec<u8>,
    index: u32,
    /// Font units to thousandths of an em.
    scale: f64,
    /// Glyph id in the font file to (glyph id in the subset, its advance in 1/1000 em).
    subset: HashMap<u16, (u16, f64)>,
    /// The glyph a character has before any shaping, as a glyph id in the font file.
    nominal: HashMap<char, u16>,
}

/// One glyph of laid out text, in the order it is drawn. Distances are in 1/1000 em.
struct Shaped {
    /// Glyph id in the subset.
    glyph: u16,
    /// How far the pen moves on, which kerning and shaping may have changed.
    advance: f64,
    /// The advance the font states for the glyph alone, which a viewer applies.
    natural: f64,
    /// Where the glyph sits relative to the pen: marks are placed this way.
    dx: f64,
    dy: f64,
}

/// A glyph as the shaper returns it, with the text it stands for.
struct RawGlyph {
    /// Glyph id in the font file.
    glyph: u16,
    /// Byte range in the text of the cluster the glyph belongs to.
    cluster: std::ops::Range<usize>,
    advance: f64,
    dx: f64,
    dy: f64,
}

/// Lays `text` out in `face`: bidirectional reordering, then shaping run by run.
///
/// Glyphs come back left to right as they are drawn, whatever the direction of the
/// script, with ligatures, contextual forms, kerning and mark positions applied.
fn shape(face: &rustybuzz::Face<'_>, text: &str) -> Vec<RawGlyph> {
    let mut out = Vec::new();
    let bidi = unicode_bidi::BidiInfo::new(text, None);
    for paragraph in &bidi.paragraphs {
        let (levels, runs) = bidi.visual_runs(paragraph, paragraph.range.clone());
        for run in runs {
            let piece = &text[run.clone()];
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(piece);
            buffer.set_direction(if levels[run.start].is_rtl() {
                rustybuzz::Direction::RightToLeft
            } else {
                rustybuzz::Direction::LeftToRight
            });
            // Script and language follow from the text itself.
            buffer.guess_segment_properties();
            let shaped = rustybuzz::shape(face, &[], buffer);
            let mut starts: Vec<usize> = shaped
                .glyph_infos()
                .iter()
                .map(|g| g.cluster as usize)
                .collect();
            starts.sort_unstable();
            starts.dedup();
            for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
                let from = info.cluster as usize;
                let to = starts
                    .iter()
                    .copied()
                    .find(|&s| s > from)
                    .unwrap_or(piece.len());
                // Line breaks and the like have no place in a line of text.
                if piece[from..to].chars().all(char::is_control) {
                    continue;
                }
                out.push(RawGlyph {
                    glyph: info.glyph_id as u16,
                    cluster: run.start + from..run.start + to,
                    advance: position.x_advance as f64,
                    dx: position.x_offset as f64,
                    dy: position.y_offset as f64,
                });
            }
        }
    }
    out
}

impl Embedded {
    /// `text` as the glyphs of the subset that draw it.
    ///
    /// The subset holds the glyphs that shaping the font's sample text selected. Where
    /// other text needs a glyph beyond those, the characters concerned are drawn with
    /// their plain glyphs instead, unshaped but legible.
    fn layout(&self, text: &str) -> Vec<Shaped> {
        let Some(face) = rustybuzz::Face::from_slice(&self.data, self.index) else {
            return Vec::new();
        };
        let raw = shape(&face, text);
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            let mut j = i;
            while j < raw.len() && raw[j].cluster == raw[i].cluster {
                j += 1;
            }
            let cluster = &raw[i..j];
            if cluster.iter().all(|g| self.subset.contains_key(&g.glyph)) {
                out.extend(cluster.iter().map(|g| {
                    let (glyph, natural) = self.subset[&g.glyph];
                    Shaped {
                        glyph,
                        advance: g.advance * self.scale,
                        natural,
                        dx: g.dx * self.scale,
                        dy: g.dy * self.scale,
                    }
                }));
            } else {
                for c in text[cluster[0].cluster.clone()].chars() {
                    let plain = self.nominal.get(&c).and_then(|g| self.subset.get(g));
                    let (glyph, natural) = plain.copied().unwrap_or((0, 0.0));
                    out.push(Shaped {
                        glyph,
                        advance: natural,
                        natural,
                        dx: 0.0,
                        dy: 0.0,
                    });
                }
            }
            i = j;
        }
        out
    }
}

/// The variant of a typeface.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
}

/// Helvetica-Bold advance widths for ASCII 32..=126, in 1/1000 em (Adobe AFM).
const HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

impl TextFont {
    /// Adds a font able to draw `text` to the document.
    ///
    /// With `file`, that font is embedded. Otherwise Helvetica is used when it
    /// suffices, and an installed font covering the text is embedded when not.
    /// `text` should be what will be drawn, not just its characters: an embedded
    /// font carries the ligatures and joined forms that this text calls for.
    pub fn new(doc: &mut Document, text: &str, file: Option<&Path>) -> Result<TextFont> {
        let mut wanted: Vec<char> = text.chars().filter(|c| !c.is_control()).collect();
        if file.is_none() && winansi(&wanted.iter().collect::<String>()).is_some() {
            return Ok(TextFont {
                id: doc.add_object(helvetica()),
                embedded: None,
                builtin: Style::default(),
            });
        }
        wanted.sort_unstable();
        wanted.dedup();
        let (data, index) = match file {
            Some(path) => {
                let data = std::fs::read(path)
                    .with_context(|| format!("cannot read font {}", path.display()))?;
                let missing = missing(&data, 0, &wanted)
                    .ok_or_else(|| anyhow!("{} is not a usable font", path.display()))?;
                if !missing.is_empty() {
                    bail!(
                        "font {} has no glyph for: {}",
                        path.display(),
                        missing.iter().collect::<String>()
                    );
                }
                (data, 0)
            }
            None => system_font(&wanted)?,
        };
        embed(doc, data, index, &wanted, text)
    }

    /// Adds a font of the given style for `text`: a built-in one (Helvetica or Courier)
    /// when the text is Latin-1, otherwise an installed font of that style, embedded.
    pub fn styled(doc: &mut Document, text: &str, style: Style) -> Result<TextFont> {
        let mut wanted: Vec<char> = text.chars().filter(|c| !c.is_control()).collect();
        if winansi(&wanted.iter().collect::<String>()).is_some() {
            let family = if style.mono { "Courier" } else { "Helvetica" };
            let variant = match (style.bold, style.italic) {
                (false, false) => "",
                (true, false) => "-Bold",
                (false, true) => "-Oblique",
                (true, true) => "-BoldOblique",
            };
            let mut font = helvetica();
            font.set(
                "BaseFont",
                Object::Name(format!("{family}{variant}").into_bytes()),
            );
            return Ok(TextFont {
                id: doc.add_object(font),
                embedded: None,
                builtin: style,
            });
        }
        wanted.sort_unstable();
        wanted.dedup();
        let (data, index) = styled_system_font(&wanted, style)?;
        embed(doc, data, index, &wanted, text)
    }

    /// The elements of a `TJ` array showing `text`: strings of glyphs, and between them
    /// the movements that kerning and shaping ask for.
    ///
    /// A `TJ` array cannot raise or lower a glyph, so marks that the font places above
    /// or below their base stay on the baseline here; `show` places them.
    pub fn elements(&self, text: &str) -> Vec<Object> {
        let Some(embedded) = &self.embedded else {
            return vec![Object::String(
                winansi(text).unwrap_or_default(),
                lopdf::StringFormat::Literal,
            )];
        };
        let mut out = Vec::new();
        let mut run: Vec<u8> = Vec::new();
        // How far the pen is from where the next glyph belongs.
        let mut owed = 0.0f64;
        for glyph in embedded.layout(text) {
            owed += glyph.dx;
            if owed.abs() >= 0.5 {
                if !run.is_empty() {
                    out.push(Object::String(
                        std::mem::take(&mut run),
                        lopdf::StringFormat::Hexadecimal,
                    ));
                }
                // A positive number in the array moves the pen back.
                out.push(Object::Real(-owed as f32));
                owed = 0.0;
            }
            run.extend(glyph.glyph.to_be_bytes());
            owed += glyph.advance - glyph.dx - glyph.natural;
        }
        if !run.is_empty() {
            out.push(Object::String(run, lopdf::StringFormat::Hexadecimal));
        }
        if owed.abs() >= 0.5 {
            out.push(Object::Real(-owed as f32));
        }
        out
    }

    /// The operators that show `text` at `size` points: one `Tj` for plain text, a `TJ`
    /// with kerning where the font has any, and changes of text rise around marks that
    /// sit above or below their base.
    pub fn show(&self, text: &str, size: f64) -> String {
        let Some(embedded) = &self.embedded else {
            return format!("{} Tj", literal(&winansi(text).unwrap_or_default()));
        };
        let hex = |bytes: &[u8]| -> String { bytes.iter().map(|b| format!("{b:02X}")).collect() };
        let mut out: Vec<String> = Vec::new();
        // The array being built: finished elements, and the glyphs of the current string.
        let mut array: Vec<String> = Vec::new();
        let mut run: Vec<u8> = Vec::new();
        let mut owed = 0.0f64;
        let mut rise = 0.0f64;
        let flush = |array: &mut Vec<String>, run: &mut Vec<u8>, out: &mut Vec<String>| {
            if !run.is_empty() {
                array.push(format!("<{}>", hex(run)));
                run.clear();
            }
            match array.as_slice() {
                [] => {}
                [only] if only.starts_with('<') => out.push(format!("{only} Tj")),
                many => out.push(format!("[{}] TJ", many.join(" "))),
            }
            array.clear();
        };
        for glyph in embedded.layout(text) {
            if (glyph.dy - rise).abs() >= 0.5 {
                // The movement owed so far belongs before the rise changes.
                if owed.abs() >= 0.5 {
                    if !run.is_empty() {
                        array.push(format!("<{}>", hex(&run)));
                        run.clear();
                    }
                    array.push(format!("{:.0}", -owed));
                    owed = 0.0;
                }
                flush(&mut array, &mut run, &mut out);
                rise = glyph.dy;
                out.push(format!("{:.2} Ts", rise * size / 1000.0));
            }
            owed += glyph.dx;
            if owed.abs() >= 0.5 {
                if !run.is_empty() {
                    array.push(format!("<{}>", hex(&run)));
                    run.clear();
                }
                array.push(format!("{:.0}", -owed));
                owed = 0.0;
            }
            run.extend(glyph.glyph.to_be_bytes());
            owed += glyph.advance - glyph.dx - glyph.natural;
        }
        if owed.abs() >= 0.5 {
            if !run.is_empty() {
                array.push(format!("<{}>", hex(&run)));
                run.clear();
            }
            array.push(format!("{:.0}", -owed));
        }
        flush(&mut array, &mut run, &mut out);
        if rise != 0.0 {
            out.push("0 Ts".to_string());
        }
        if out.is_empty() {
            return "<> Tj".to_string();
        }
        out.join("\n")
    }

    /// Whether this is an embedded font rather than the built-in Helvetica.
    pub fn is_embedded(&self) -> bool {
        self.embedded.is_some()
    }

    pub fn width(&self, text: &str, size: f64) -> f64 {
        match &self.embedded {
            None => {
                let bytes = winansi(text).unwrap_or_default();
                if self.builtin.mono {
                    // Every Courier glyph is 600 units wide.
                    bytes.len() as f64 * 0.6 * size
                } else if self.builtin.bold {
                    let units: u32 = bytes
                        .iter()
                        .map(|&b| {
                            HELVETICA_BOLD
                                .get((b as usize).wrapping_sub(32))
                                .copied()
                                .unwrap_or(556) as u32
                        })
                        .sum();
                    units as f64 * size / 1000.0
                } else {
                    helvetica_width(&bytes, size)
                }
            }
            Some(embedded) => {
                embedded.layout(text).iter().map(|g| g.advance).sum::<f64>() * size / 1000.0
            }
        }
    }
}

pub fn helvetica() -> Dictionary {
    let mut font = Dictionary::new();
    font.set("Type", Object::Name(b"Font".to_vec()));
    font.set("Subtype", Object::Name(b"Type1".to_vec()));
    font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
    font.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    font
}

/// Characters of `wanted` the face cannot draw, or `None` if it does not parse.
fn missing(data: &[u8], index: u32, wanted: &[char]) -> Option<Vec<char>> {
    let face = ttf_parser::Face::parse(data, index).ok()?;
    Some(
        wanted
            .iter()
            .copied()
            .filter(|&c| face.glyph_index(c).is_none())
            .collect(),
    )
}

/// Where common distributions and systems keep a broad-coverage sans-serif.
const USUAL_FONTS: [&str; 9] = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "C:\\Windows\\Fonts\\arial.ttf",
];

/// Finds an installed font of the given style covering `wanted`.
///
/// Falls back to any covering font: wrong emphasis beats missing letters.
fn styled_system_font(wanted: &[char], style: Style) -> Result<(Vec<u8>, u32)> {
    if style == Style::default() {
        return system_font(wanted);
    }
    let covers =
        |data: &[u8], index: u32| missing(data, index, wanted).is_some_and(|m| m.is_empty());
    // The usual families name their variants predictably, which avoids indexing all fonts.
    let variant = match (style.bold, style.italic) {
        (false, false) => ["", "-Regular"],
        (true, false) => ["-Bold", "-Bold"],
        (false, true) => ["-Oblique", "-Italic"],
        (true, true) => ["-BoldOblique", "-BoldItalic"],
    };
    for path in USUAL_FONTS {
        let path = Path::new(path);
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let family = match (style.mono, stem.trim_end_matches("-Regular")) {
            (true, "LiberationSans") => "LiberationMono".to_string(),
            // DejaVuSans has DejaVuSansMono, NotoSans has NotoSansMono.
            (true, family) => format!("{family}Mono"),
            (false, family) => family.to_string(),
        };
        for suffix in variant {
            let candidate = path.with_file_name(format!("{family}{suffix}.ttf"));
            if let Ok(data) = std::fs::read(&candidate)
                && covers(&data, 0)
            {
                return Ok((data, 0));
            }
        }
    }
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let query = fontdb::Query {
        families: &[if style.mono {
            fontdb::Family::Monospace
        } else {
            fontdb::Family::SansSerif
        }],
        weight: if style.bold {
            fontdb::Weight::BOLD
        } else {
            fontdb::Weight::NORMAL
        },
        style: if style.italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        },
        ..Default::default()
    };
    let found = db
        .query(&query)
        .and_then(|id| {
            db.with_face_data(id, |data, index| {
                covers(data, index).then(|| (data.to_vec(), index))
            })
        })
        .flatten();
    match found {
        Some(hit) => Ok(hit),
        None => system_font(wanted),
    }
}

/// Finds an installed font covering `wanted`, preferring a regular sans-serif.
fn system_font(wanted: &[char]) -> Result<(Vec<u8>, u32)> {
    // Indexing every installed font takes seconds on a cold cache, so the usual
    // candidates are tried first; the full search below remains the fallback.
    for path in USUAL_FONTS {
        if let Ok(data) = std::fs::read(path)
            && missing(&data, 0, wanted).is_some_and(|m| m.is_empty())
        {
            return Ok((data, 0));
        }
    }
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let preferred = db.query(&fontdb::Query {
        families: &[fontdb::Family::SansSerif],
        ..Default::default()
    });
    let mut faces: Vec<&fontdb::FaceInfo> = db.faces().collect();
    // Plain upright text faces first, so a stamp does not come out bold, italic or monospaced.
    faces.sort_by_key(|f| {
        (
            Some(f.id) != preferred,
            f.style != fontdb::Style::Normal,
            f.weight != fontdb::Weight::NORMAL,
            f.stretch != fontdb::Stretch::Normal,
            f.monospaced,
            f.post_script_name.clone(),
        )
    });
    for face in faces {
        let found = db.with_face_data(face.id, |data, index| {
            missing(data, index, wanted)
                .is_some_and(|m| m.is_empty())
                .then(|| (data.to_vec(), index))
        });
        if let Some(Some(hit)) = found {
            return Ok(hit);
        }
    }
    bail!(
        "no installed font can draw '{}'; pass a font file that can",
        wanted
            .iter()
            .filter(|c| winansi(&c.to_string()).is_none())
            .collect::<String>()
    )
}

/// Embeds the glyphs that `text` needs as a Type0 font with an identity encoding.
///
/// Those are the plain glyph of every character in `wanted`, and whatever shaping
/// `text` selects on top: ligatures, joined and contextual forms, positioned marks.
fn embed(
    doc: &mut Document,
    data: Vec<u8>,
    index: u32,
    wanted: &[char],
    text: &str,
) -> Result<TextFont> {
    let face =
        ttf_parser::Face::parse(&data, index).map_err(|e| anyhow!("cannot parse font: {e}"))?;
    let scale = 1000.0 / face.units_per_em() as f64;
    let mut remapper = subsetter::GlyphRemapper::new();
    let advance = |glyph: u16| {
        face.glyph_hor_advance(ttf_parser::GlyphId(glyph))
            .unwrap_or(0) as f64
            * scale
    };
    let mut subset: HashMap<u16, (u16, f64)> = HashMap::new();
    let mut nominal = HashMap::new();
    // What each glyph of the subset reads as, for the map back to text.
    let mut reads: BTreeMap<u16, String> = BTreeMap::new();
    for &c in wanted {
        if let Some(glyph) = face.glyph_index(c) {
            let new = remapper.remap(glyph.0);
            subset.insert(glyph.0, (new, advance(glyph.0)));
            nominal.insert(c, glyph.0);
            reads.entry(new).or_insert_with(|| c.to_string());
        }
    }
    if let Some(shaper) = rustybuzz::Face::from_slice(&data, index) {
        // Shaped line by line: a line break never takes part in a ligature.
        for line in text.split(char::is_control) {
            let shaped = shape(&shaper, line);
            let mut i = 0;
            while i < shaped.len() {
                let mut j = i;
                while j < shaped.len() && shaped[j].cluster == shaped[i].cluster {
                    j += 1;
                }
                // One cluster: the glyphs that together draw a stretch of characters.
                // A glyph that is the plain glyph of one of those characters reads as
                // it. The other characters are shared out, in order, among the glyphs
                // shaping put in: a ligature reads as all it replaced. Read in drawing
                // order, the cluster then gives back its characters.
                let mut rest: Vec<char> = line[shaped[i].cluster.clone()].chars().collect();
                let mut put_in = Vec::new();
                for glyph in &shaped[i..j] {
                    let new = remapper.remap(glyph.glyph);
                    subset.insert(glyph.glyph, (new, advance(glyph.glyph)));
                    match rest
                        .iter()
                        .position(|c| nominal.get(c) == Some(&glyph.glyph))
                    {
                        Some(at) => {
                            rest.remove(at);
                        }
                        None => put_in.push(new),
                    }
                }
                for (k, new) in put_in.iter().enumerate() {
                    let to = if k + 1 == put_in.len() {
                        rest.len()
                    } else {
                        (k + 1).min(rest.len())
                    };
                    let part: String = rest[k.min(rest.len())..to].iter().collect();
                    if !part.is_empty() {
                        reads.entry(*new).or_insert(part);
                    }
                }
                i = j;
            }
        }
    }
    let program = subsetter::subset(&data, index, &remapper)
        .map_err(|e| anyhow!("cannot subset font: {e}"))?;

    // CFF outlines are embedded as a bare CFF table, TrueType outlines as a font file.
    let cff_tag = ttf_parser::Tag::from_bytes(b"CFF ");
    let is_cff = face.raw_face().table(cff_tag).is_some();
    let program = if is_cff {
        let raw = ttf_parser::RawFace::parse(&program, 0)
            .map_err(|e| anyhow!("cannot read font subset: {e}"))?;
        raw.table(cff_tag)
            .ok_or_else(|| anyhow!("font subset has no CFF table"))?
            .to_vec()
    } else {
        program
    };

    let base = face
        .names()
        .into_iter()
        .find(|n| n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
        .and_then(|n| n.to_string())
        .map(|n| {
            n.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>()
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Font".to_string());
    // Subset fonts carry a six letter tag, derived here from the glyph set so output is reproducible.
    let mut glyph_set: Vec<u16> = subset.keys().copied().collect();
    glyph_set.sort_unstable();
    let hash = glyph_set.iter().fold(0xcbf29ce484222325u64, |h, &g| {
        (h ^ g as u64).wrapping_mul(0x100000001b3)
    });
    let tag: String = (0..6)
        .map(|i| (b'A' + ((hash >> (i * 8)) % 26) as u8) as char)
        .collect();
    let name = Object::Name(format!("{tag}+{base}").into_bytes());

    let mut file = Dictionary::new();
    if is_cff {
        file.set("Subtype", Object::Name(b"CIDFontType0C".to_vec()));
    }
    let mut file = Stream::new(file, program);
    let _ = file.compress();
    let file = doc.add_object(file);

    let bbox = face.global_bounding_box();
    let mut descriptor = Dictionary::new();
    descriptor.set("Type", Object::Name(b"FontDescriptor".to_vec()));
    descriptor.set("FontName", name.clone());
    descriptor.set("Flags", 4);
    descriptor.set(
        "FontBBox",
        [bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max]
            .map(|v| Object::Real((v as f64 * scale) as f32))
            .to_vec(),
    );
    descriptor.set("ItalicAngle", 0);
    descriptor.set(
        "Ascent",
        Object::Real((face.ascender() as f64 * scale) as f32),
    );
    descriptor.set(
        "Descent",
        Object::Real((face.descender() as f64 * scale) as f32),
    );
    descriptor.set(
        "CapHeight",
        Object::Real((face.capital_height().unwrap_or(face.ascender()) as f64 * scale) as f32),
    );
    descriptor.set("StemV", 80);
    descriptor.set(if is_cff { "FontFile3" } else { "FontFile2" }, file);
    let descriptor = doc.add_object(descriptor);

    let mut by_glyph: Vec<(u16, f64)> = subset.values().copied().collect();
    by_glyph.sort_by_key(|g| g.0);
    let mut widths = Vec::with_capacity(by_glyph.len() * 2);
    for &(glyph, advance) in &by_glyph {
        widths.push(Object::Integer(glyph as i64));
        widths.push(Object::Array(vec![Object::Real(advance as f32)]));
    }

    let mut system = Dictionary::new();
    system.set("Registry", Object::string_literal("Adobe"));
    system.set("Ordering", Object::string_literal("Identity"));
    system.set("Supplement", 0);
    let mut cid = Dictionary::new();
    cid.set("Type", Object::Name(b"Font".to_vec()));
    cid.set(
        "Subtype",
        Object::Name(if is_cff {
            b"CIDFontType0".to_vec()
        } else {
            b"CIDFontType2".to_vec()
        }),
    );
    cid.set("BaseFont", name.clone());
    cid.set("CIDSystemInfo", system);
    cid.set("FontDescriptor", descriptor);
    cid.set("DW", 1000);
    cid.set("W", widths);
    if !is_cff {
        cid.set("CIDToGIDMap", Object::Name(b"Identity".to_vec()));
    }
    let cid = doc.add_object(cid);

    let reads: Vec<(u16, String)> = reads.into_iter().collect();
    let mut to_unicode = Stream::new(Dictionary::new(), to_unicode(&reads).into_bytes());
    let _ = to_unicode.compress();
    let to_unicode = doc.add_object(to_unicode);

    let mut font = Dictionary::new();
    font.set("Type", Object::Name(b"Font".to_vec()));
    font.set("Subtype", Object::Name(b"Type0".to_vec()));
    font.set("BaseFont", name);
    font.set("Encoding", Object::Name(b"Identity-H".to_vec()));
    font.set("DescendantFonts", vec![Object::Reference(cid)]);
    font.set("ToUnicode", to_unicode);
    Ok(TextFont {
        id: doc.add_object(font),
        embedded: Some(Embedded {
            data,
            index,
            scale,
            subset,
            nominal,
        }),
        builtin: Style::default(),
    })
}

/// A CMap from glyph ids back to text, so stamped text stays searchable.
///
/// A glyph may read as several characters: a ligature does.
fn to_unicode(glyphs: &[(u16, String)]) -> String {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    // A bfchar block holds at most 100 entries.
    for chunk in glyphs.chunks(100) {
        cmap += &format!("{} beginbfchar\n", chunk.len());
        for (glyph, text) in chunk {
            let utf16: String = text.encode_utf16().map(|u| format!("{u:04X}")).collect();
            cmap += &format!("<{glyph:04X}> <{utf16}>\n");
        }
        cmap += "endbfchar\n";
    }
    cmap + "endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_text_uses_helvetica() {
        let mut doc = Document::with_version("1.7");
        let font = TextFont::new(&mut doc, "Caf\u{e9} (1)", None).unwrap();
        assert_eq!(font.show("a(b)\u{e9}", 10.0), "(a\\(b\\)\\351) Tj");
        // "Hi": 722 + 222 thousandths of an em.
        assert!((font.width("Hi", 10.0) - 9.44).abs() < 1e-9);
        assert_eq!(
            doc.get_dictionary(font.id)
                .unwrap()
                .get(b"BaseFont")
                .unwrap()
                .as_name()
                .unwrap(),
            b"Helvetica"
        );
    }

    #[test]
    fn to_unicode_maps_glyphs_including_astral_characters_and_ligatures() {
        let cmap = to_unicode(&[
            (1, "ż".to_string()),
            (2, "😀".to_string()),
            (3, "fi".to_string()),
        ]);
        assert!(cmap.contains(
            "3 beginbfchar\n<0001> <017C>\n<0002> <D83DDE00>\n<0003> <00660069>\nendbfchar"
        ));
    }

    /// A font embedded for `text`, or nothing where no installed font can draw it.
    fn embedded(text: &str) -> Option<(Document, TextFont)> {
        let mut doc = Document::with_version("1.7");
        match TextFont::new(&mut doc, text, None) {
            Ok(font) => Some((doc, font)),
            Err(e) => {
                assert!(e.to_string().contains("no installed font"), "{e:#}");
                eprintln!("skipped: {e}");
                None
            }
        }
    }

    /// What each glyph of `text` reads as, in the order the glyphs are drawn.
    fn drawn(doc: &Document, font: &TextFont, text: &str) -> Vec<String> {
        let map = doc
            .get_dictionary(font.id)
            .unwrap()
            .get(b"ToUnicode")
            .unwrap();
        let map = doc
            .get_object(map.as_reference().unwrap())
            .unwrap()
            .as_stream()
            .unwrap();
        let map = String::from_utf8(map.decompressed_content().unwrap()).unwrap();
        font.embedded
            .as_ref()
            .unwrap()
            .layout(text)
            .iter()
            .map(|g| {
                let line = map
                    .lines()
                    .find(|l| l.starts_with(&format!("<{:04X}> ", g.glyph)));
                let hex = line.map_or("", |l| l[8..].trim_matches(|c| c == '<' || c == '>'));
                let units: Vec<u16> = (0..hex.len() / 4)
                    .map(|i| u16::from_str_radix(&hex[i * 4..i * 4 + 4], 16).unwrap())
                    .collect();
                String::from_utf16(&units).unwrap()
            })
            .collect()
    }

    #[test]
    fn right_to_left_text_is_drawn_from_its_right_end() {
        // Hebrew does not join, so each letter keeps its glyph and only the order changes.
        let Some((doc, font)) = embedded("\u{5e9}\u{5dc}\u{5d5}\u{5dd} abc") else {
            return;
        };
        assert_eq!(
            drawn(&doc, &font, "\u{5e9}\u{5dc}\u{5d5}\u{5dd}"),
            ["\u{5dd}", "\u{5d5}", "\u{5dc}", "\u{5e9}"]
        );
        // Latin text in the same line keeps its own direction.
        assert_eq!(
            drawn(&doc, &font, "abc \u{5e9}\u{5dc}\u{5d5}\u{5dd}"),
            [
                "a", "b", "c", " ", "\u{5dd}", "\u{5d5}", "\u{5dc}", "\u{5e9}"
            ]
        );
    }

    #[test]
    fn arabic_is_joined_and_its_ligature_reads_as_both_letters() {
        // seen, lam, alef, meem: the lam and alef form one ligature in every Arabic font.
        let word = "\u{633}\u{644}\u{627}\u{645}";
        let Some((doc, font)) = embedded(word) else {
            return;
        };
        let embedded = font.embedded.as_ref().unwrap();
        let face = ttf_parser::Face::parse(&embedded.data, embedded.index).unwrap();
        if face
            .raw_face()
            .table(ttf_parser::Tag::from_bytes(b"GSUB"))
            .is_none()
        {
            eprintln!("skipped: the installed font has no shaping tables");
            return;
        }
        assert_eq!(
            drawn(&doc, &font, word),
            ["\u{645}", "\u{644}\u{627}", "\u{633}"]
        );
        // The seen is drawn in the form that joins to what follows, not as it stands alone.
        let glyphs = embedded.layout(word);
        let alone = embedded.subset[&embedded.nominal[&'\u{633}']].0;
        assert_ne!(glyphs[2].glyph, alone);
        // Text the font was not made for falls back to plain glyphs rather than to nothing.
        assert_eq!(embedded.layout("\u{645}\u{633}").len(), 2);
    }

    #[test]
    fn kerning_narrows_text_and_is_written_as_movements() {
        let Some((_, font)) = embedded("\u{17c} AVAVAV") else {
            return;
        };
        let apart = 3.0 * (font.width("A", 100.0) + font.width("V", 100.0));
        let together = font.width("AVAVAV", 100.0);
        if (apart - together).abs() < 1e-6 {
            eprintln!("skipped: the installed font does not kern A and V");
            return;
        }
        assert!(together < apart, "{together} {apart}");
        let shown = font.show("AVAVAV", 100.0);
        assert!(shown.starts_with('[') && shown.ends_with("] TJ"), "{shown}");
        // The movements written add up to the difference in width.
        let moved: f64 = shown
            .trim_matches(|c| c == '[' || c == ']' || c == 'T' || c == 'J' || c == ' ')
            .split(' ')
            .filter_map(|part| part.parse::<f64>().ok())
            .sum();
        assert!(
            (moved / 10.0 - (apart - together)).abs() < 0.6,
            "{moved} {apart} {together}"
        );
        assert_eq!(font.elements("AVAVAV").len(), shown.split(' ').count() - 1);
    }
}
