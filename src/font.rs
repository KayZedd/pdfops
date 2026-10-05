//! Fonts for text that pdfops draws itself (stamps and form field appearances).
//!
//! Latin-1 text uses the built-in Helvetica, which costs no space. Anything
//! else needs real glyphs, so a font file is subsetted to the characters used
//! and embedded as a composite font with a ToUnicode map.

use std::collections::BTreeMap;
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
    /// Character to (glyph id in the subset, advance in 1/1000 em); `None` for a built-in font.
    glyphs: Option<BTreeMap<char, (u16, f64)>>,
    /// Which built-in metrics apply when nothing is embedded.
    builtin: Style,
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
    /// Adds a font able to draw every character of `chars` to the document.
    ///
    /// With `file`, that font is embedded. Otherwise Helvetica is used when it
    /// suffices, and an installed font covering the text is embedded when not.
    pub fn new(doc: &mut Document, chars: &str, file: Option<&Path>) -> Result<TextFont> {
        let mut wanted: Vec<char> = chars.chars().filter(|c| !c.is_control()).collect();
        if file.is_none() && winansi(&wanted.iter().collect::<String>()).is_some() {
            return Ok(TextFont {
                id: doc.add_object(helvetica()),
                glyphs: None,
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
        embed(doc, &data, index, &wanted)
    }

    /// The string operand for a `Tj` showing `text`.
    pub fn encode(&self, text: &str) -> String {
        match &self.glyphs {
            None => literal(&winansi(text).unwrap_or_default()),
            Some(glyphs) => {
                let hex: String = text
                    .chars()
                    .map(|c| format!("{:04X}", glyphs.get(&c).map_or(0, |g| g.0)))
                    .collect();
                format!("<{hex}>")
            }
        }
    }

    /// Adds a font of the given style for `chars`: a built-in one (Helvetica or Courier)
    /// when the text is Latin-1, otherwise an installed font of that style, embedded.
    pub fn styled(doc: &mut Document, chars: &str, style: Style) -> Result<TextFont> {
        let mut wanted: Vec<char> = chars.chars().filter(|c| !c.is_control()).collect();
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
                glyphs: None,
                builtin: style,
            });
        }
        wanted.sort_unstable();
        wanted.dedup();
        let (data, index) = styled_system_font(&wanted, style)?;
        embed(doc, &data, index, &wanted)
    }

    /// The string object showing `text`, for content built as operations.
    pub fn operand(&self, text: &str) -> Object {
        match &self.glyphs {
            None => Object::String(
                winansi(text).unwrap_or_default(),
                lopdf::StringFormat::Literal,
            ),
            Some(glyphs) => Object::String(
                text.chars()
                    .flat_map(|c| glyphs.get(&c).map_or(0, |g| g.0).to_be_bytes())
                    .collect(),
                lopdf::StringFormat::Hexadecimal,
            ),
        }
    }

    /// Whether this is an embedded font rather than the built-in Helvetica.
    pub fn is_embedded(&self) -> bool {
        self.glyphs.is_some()
    }

    pub fn width(&self, text: &str, size: f64) -> f64 {
        match &self.glyphs {
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
            Some(glyphs) => {
                text.chars()
                    .map(|c| glyphs.get(&c).map_or(0.0, |g| g.1))
                    .sum::<f64>()
                    * size
                    / 1000.0
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

/// Embeds the glyphs for `wanted` as a Type0 font with an identity encoding.
fn embed(doc: &mut Document, data: &[u8], index: u32, wanted: &[char]) -> Result<TextFont> {
    let face =
        ttf_parser::Face::parse(data, index).map_err(|e| anyhow!("cannot parse font: {e}"))?;
    let scale = 1000.0 / face.units_per_em() as f64;
    let mut remapper = subsetter::GlyphRemapper::new();
    let mut glyphs = BTreeMap::new();
    for &c in wanted {
        if let Some(gid) = face.glyph_index(c) {
            let advance = face.glyph_hor_advance(gid).unwrap_or(0) as f64 * scale;
            glyphs.insert(c, (remapper.remap(gid.0), advance));
        }
    }
    let subset = subsetter::subset(data, index, &remapper)
        .map_err(|e| anyhow!("cannot subset font: {e}"))?;

    // CFF outlines are embedded as a bare CFF table, TrueType outlines as a font file.
    let cff_tag = ttf_parser::Tag::from_bytes(b"CFF ");
    let is_cff = face.raw_face().table(cff_tag).is_some();
    let program = if is_cff {
        let raw = ttf_parser::RawFace::parse(&subset, 0)
            .map_err(|e| anyhow!("cannot read font subset: {e}"))?;
        raw.table(cff_tag)
            .ok_or_else(|| anyhow!("font subset has no CFF table"))?
            .to_vec()
    } else {
        subset
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
    let hash = wanted.iter().fold(0xcbf29ce484222325u64, |h, &c| {
        (h ^ c as u64).wrapping_mul(0x100000001b3)
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

    let mut by_gid: Vec<(u16, f64, char)> = glyphs
        .iter()
        .map(|(&c, &(gid, adv))| (gid, adv, c))
        .collect();
    by_gid.sort_by_key(|g| g.0);
    by_gid.dedup_by_key(|g| g.0);
    let mut widths = Vec::with_capacity(by_gid.len() * 2);
    for &(gid, advance, _) in &by_gid {
        widths.push(Object::Integer(gid as i64));
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

    let mut to_unicode = Stream::new(Dictionary::new(), to_unicode(&by_gid).into_bytes());
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
        glyphs: Some(glyphs),
        builtin: Style::default(),
    })
}

/// A CMap from glyph ids back to text, so stamped text stays searchable.
fn to_unicode(glyphs: &[(u16, f64, char)]) -> String {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    // A bfchar block holds at most 100 entries.
    for chunk in glyphs.chunks(100) {
        cmap += &format!("{} beginbfchar\n", chunk.len());
        for &(gid, _, c) in chunk {
            let utf16: String = c
                .encode_utf16(&mut [0; 2])
                .iter()
                .map(|u| format!("{u:04X}"))
                .collect();
            cmap += &format!("<{gid:04X}> <{utf16}>\n");
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
        assert_eq!(font.encode("a(b)\u{e9}"), "(a\\(b\\)\\351)");
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
    fn to_unicode_maps_glyphs_including_astral_characters() {
        let cmap = to_unicode(&[(1, 500.0, 'ż'), (2, 500.0, '😀')]);
        assert!(cmap.contains("2 beginbfchar\n<0001> <017C>\n<0002> <D83DDE00>\nendbfchar"));
    }
}
