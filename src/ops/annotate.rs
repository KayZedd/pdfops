//! Annotations: listing what a document carries, and marking it up with
//! highlights, underlines, strike-outs, boxes, notes and links.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::{Args, ValueEnum};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::ops::edit::visual_space;
use crate::ops::layout;
use crate::ops::redact::{Area, apply, bounds, compile, invert, parse_rect, text_areas};
use crate::{doc, pagespec};

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct AnnotationsArgs {
    /// PDF file to inspect
    pub input: PathBuf,
    /// Pages to list, e.g. "1-5" (default: all)
    #[arg(short, long)]
    pub pages: Option<String>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(ValueEnum, Deserialize, JsonSchema, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mark {
    /// A translucent band over the text
    Highlight,
    /// A line under the text
    Underline,
    /// A line through the text
    Strikeout,
    /// A rectangle around the area
    Box,
    /// A note icon that opens the comment
    Note,
    /// A clickable area that opens a URL
    Link,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct AnnotateArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Where to write the result (may be the input file)
    #[arg(short, long)]
    pub output: PathBuf,
    /// What to add (default: highlight)
    #[arg(long, value_enum)]
    pub kind: Option<Mark>,
    /// Text to mark wherever it occurs
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
    /// Area to mark as "page:x0,y0,x1,y1" in the coordinates layout reports
    #[arg(long = "rect")]
    #[serde(default)]
    pub rects: Vec<String>,
    /// Comment shown when the annotation is opened; required for a note
    #[arg(long)]
    pub comment: Option<String>,
    /// Address a link opens; required for a link
    #[arg(long)]
    pub url: Option<String>,
    /// Colour as RRGGBB hex (default: yellow for highlight and note, red otherwise)
    #[arg(long)]
    pub color: Option<String>,
    /// Author recorded on the annotation
    #[arg(long)]
    pub author: Option<String>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

fn numbers(doc: &Document, object: &Object) -> Vec<f64> {
    doc::resolve(doc, object)
        .as_array()
        .map(|a| a.iter().filter_map(|o| doc::number(doc, o)).collect())
        .unwrap_or_default()
}

pub fn annotations(a: AnnotationsArgs) -> Result<Value> {
    let (d, _) = doc::read(&a.input, a.password.as_deref())?;
    let pages = d.get_pages();
    let numbers_of: HashMap<ObjectId, u32> = pages.iter().map(|(n, id)| (*id, *n)).collect();
    let wanted = pagespec::parse_or_all(a.pages.as_deref(), pages.len() as u32)?;
    let mut found = Vec::new();
    for n in wanted {
        let id = pages[&n];
        let (to_user, _, visual_height) =
            visual_space(doc::page_box(&d, id), doc::rotation(&d, id));
        let to_layout = invert(to_user);
        let annots = d
            .get_dictionary(id)
            .ok()
            .and_then(|p| p.get(b"Annots").ok())
            .map(|o| doc::resolve(&d, o));
        for annot in annots.and_then(|o| o.as_array().ok()).into_iter().flatten() {
            let Ok(dict) = doc::resolve(&d, annot).as_dict() else {
                continue;
            };
            let kind = dict
                .get(b"Subtype")
                .ok()
                .and_then(|s| s.as_name().ok())
                .unwrap_or(b"");
            // Form fields are listed by `forms`; a popup only holds its parent's comment window.
            if kind == b"Widget" || kind == b"Popup" {
                continue;
            }
            let text = |key: &[u8]| {
                dict.get(key)
                    .ok()
                    .and_then(|o| doc::text(doc::resolve(&d, o)))
            };
            let mut entry =
                json!({"page": n, "type": String::from_utf8_lossy(kind).to_lowercase()});
            let rect = dict
                .get(b"Rect")
                .ok()
                .map(|r| numbers(&d, r))
                .unwrap_or_default();
            if let (Some(to_layout), &[x0, y0, x1, y1]) = (to_layout, &rect[..]) {
                // Reported like everything else: top-left origin, y down.
                let b = bounds([(x0, y0), (x1, y1)].map(|(x, y)| {
                    let (vx, vy) = apply(to_layout, x, y);
                    (vx, visual_height - vy)
                }));
                entry["rect"] = json!(b.map(|v| (v * 10.0).round() / 10.0));
            }
            for (name, key) in [("comment", &b"Contents"[..]), ("author", b"T")] {
                if let Some(value) = text(key).filter(|v| !v.is_empty()) {
                    entry[name] = json!(value);
                }
            }
            let action = dict
                .get(b"A")
                .ok()
                .and_then(|o| doc::resolve(&d, o).as_dict().ok());
            if let Some(url) = action
                .and_then(|act| act.get(b"URI").ok())
                .and_then(|u| doc::text(doc::resolve(&d, u)))
            {
                entry["url"] = json!(url);
            }
            // A link inside the document: a destination array whose first element is the page.
            let dest = dict
                .get(b"Dest")
                .ok()
                .or_else(|| action.and_then(|act| act.get(b"D").ok()));
            let target = dest
                .and_then(|o| doc::resolve(&d, o).as_array().ok())
                .and_then(|arr| arr.first()?.as_reference().ok())
                .and_then(|page| numbers_of.get(&page));
            if let Some(target) = target {
                entry["target_page"] = json!(target);
            }
            found.push(entry);
        }
    }
    Ok(json!({"file": a.input, "annotations": found}))
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

/// The annotation's appearance: a form drawn in page coordinates over `rect`.
///
/// Viewers draw their own for the standard kinds, but renderers (ours included)
/// show only what the file supplies.
fn appearance(d: &mut Document, kind: Mark, rect: Area, [r, g, b]: [f64; 3]) -> ObjectId {
    let [x0, y0, x1, y1] = rect;
    let (w, h) = (x1 - x0, y1 - y0);
    let mut resources = Dictionary::new();
    let body = match kind {
        Mark::Highlight => {
            // Multiply keeps the text under the band readable.
            let mut state = Dictionary::new();
            state.set("BM", Object::Name(b"Multiply".to_vec()));
            let mut states = Dictionary::new();
            states.set("Mark", state);
            resources.set("ExtGState", states);
            format!("/Mark gs\n{r:.3} {g:.3} {b:.3} rg\n{x0:.2} {y0:.2} {w:.2} {h:.2} re\nf\n")
        }
        Mark::Underline | Mark::Strikeout => {
            let y = if kind == Mark::Underline {
                y0 + h * 0.08
            } else {
                y0 + h * 0.42
            };
            let width = (h * 0.07).max(0.5);
            format!(
                "{r:.3} {g:.3} {b:.3} RG\n{width:.2} w\n{x0:.2} {y:.2} m\n{x1:.2} {y:.2} l\nS\n"
            )
        }
        Mark::Box => format!(
            "{r:.3} {g:.3} {b:.3} RG\n1.5 w\n{:.2} {:.2} {:.2} {:.2} re\nS\n",
            x0 + 0.75,
            y0 + 0.75,
            (w - 1.5).max(0.0),
            (h - 1.5).max(0.0)
        ),
        Mark::Note | Mark::Link => format!(
            "{r:.3} {g:.3} {b:.3} rg\n0.3 G\n0.8 w\n{:.2} {:.2} {:.2} {:.2} re\nB\n",
            x0 + 0.4,
            y0 + 0.4,
            (w - 0.8).max(0.0),
            (h - 0.8).max(0.0)
        ),
    };
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Form".to_vec()));
    dict.set("BBox", rect.map(|v| Object::Real(v as f32)).to_vec());
    dict.set("Resources", resources);
    d.add_object(Stream::new(dict, body.into_bytes()))
}

pub fn annotate(a: AnnotateArgs) -> Result<Value> {
    let kind = a.kind.unwrap_or(Mark::Highlight);
    if a.rects.is_empty() && a.texts.is_empty() {
        bail!("nothing to mark: give texts to find or rects (page:x0,y0,x1,y1)");
    }
    if kind == Mark::Link && a.url.as_deref().is_none_or(str::is_empty) {
        bail!("a link needs a url");
    }
    if kind == Mark::Note && a.comment.as_deref().is_none_or(str::is_empty) {
        bail!("a note needs a comment");
    }
    let yellow = matches!(kind, Mark::Highlight | Mark::Note);
    let color =
        parse_color(
            a.color
                .as_deref()
                .unwrap_or(if yellow { "FFE94D" } else { "E53935" }),
        )?;

    let (pdf, _) = doc::open_lazy(&a.input, a.password.as_deref())?;
    let total = pdf.pages().len() as u32;
    let mut areas: Vec<(u32, Area)> = Vec::new();
    for spec in &a.rects {
        areas.push(parse_rect(spec, total)?);
    }
    if !a.texts.is_empty() {
        let patterns = compile(&a.texts, a.regex, a.case_sensitive)?;
        for n in pagespec::parse_or_all(a.pages.as_deref(), total)? {
            let found = text_areas(&layout::scan(&pdf.pages()[n as usize - 1]), &patterns);
            areas.extend(found.into_iter().map(|(area, ..)| (n, area)));
        }
    }
    if areas.is_empty() {
        bail!("none of the texts were found; nothing was written");
    }

    let mut d = doc::load(&a.input, a.password.as_deref())?;
    let ids = doc::page_ids(&d);
    let mut added: HashMap<u32, usize> = HashMap::new();
    for (n, area) in &areas {
        let page = *ids
            .get(*n as usize - 1)
            .with_context(|| format!("page {n} is missing"))?;
        let (to_user, _, visual_height) =
            visual_space(doc::page_box(&d, page), doc::rotation(&d, page));
        let mut rect = bounds(
            [(area[0], area[1]), (area[2], area[3])]
                .map(|(x, y)| apply(to_user, x, visual_height - y)),
        );
        if kind == Mark::Note {
            // The icon sits at the area's top-left corner, at a fixed size.
            rect = [rect[0], rect[3] - 18.0, rect[0] + 18.0, rect[3]];
        }
        let [x0, y0, x1, y1] = rect;

        let mut annot = Dictionary::new();
        annot.set("Type", Object::Name(b"Annot".to_vec()));
        let subtype: &[u8] = match kind {
            Mark::Highlight => b"Highlight",
            Mark::Underline => b"Underline",
            Mark::Strikeout => b"StrikeOut",
            Mark::Box => b"Square",
            Mark::Note => b"Text",
            Mark::Link => b"Link",
        };
        annot.set("Subtype", Object::Name(subtype.to_vec()));
        annot.set("Rect", rect.map(|v| Object::Real(v as f32)).to_vec());
        annot.set("P", page);
        // Printed with the page.
        annot.set("F", 4);
        annot.set("C", color.map(|v| Object::Real(v as f32)).to_vec());
        if let Some(comment) = a.comment.as_deref().filter(|c| !c.is_empty()) {
            annot.set("Contents", lopdf::text_string(comment));
        }
        if let Some(author) = a.author.as_deref().filter(|t| !t.is_empty()) {
            annot.set("T", lopdf::text_string(author));
        }
        match kind {
            Mark::Highlight | Mark::Underline | Mark::Strikeout => {
                // The marked quadrilateral: top-left, top-right, bottom-left, bottom-right.
                let quad = [x0, y1, x1, y1, x0, y0, x1, y0];
                annot.set("QuadPoints", quad.map(|v| Object::Real(v as f32)).to_vec());
            }
            Mark::Note => annot.set("Name", Object::Name(b"Comment".to_vec())),
            Mark::Link => {
                let mut action = Dictionary::new();
                action.set("S", Object::Name(b"URI".to_vec()));
                action.set(
                    "URI",
                    Object::string_literal(a.url.clone().unwrap_or_default()),
                );
                annot.set("A", action);
                annot.set("Border", vec![0.into(), 0.into(), 0.into()]);
            }
            Mark::Box => {}
        }
        // A link is an invisible hot area; everything else brings its own picture.
        if kind != Mark::Link {
            let stream = appearance(&mut d, kind, rect, color);
            let mut ap = Dictionary::new();
            ap.set("N", stream);
            annot.set("AP", ap);
        }
        let annot = d.add_object(annot);
        let mut list = d
            .get_dictionary(page)?
            .get(b"Annots")
            .ok()
            .and_then(|o| doc::resolve(&d, o).as_array().ok())
            .cloned()
            .unwrap_or_default();
        list.push(Object::Reference(annot));
        d.get_dictionary_mut(page)?.set("Annots", list);
        *added.entry(*n).or_default() += 1;
    }
    let size = doc::save(&mut d, &a.output)?;
    let mut pages: Vec<(u32, usize)> = added.into_iter().collect();
    pages.sort_unstable();
    Ok(json!({
        "output": a.output,
        "added": areas.len(),
        "pages": pages.iter().map(|(page, count)| json!({"page": page, "added": count})).collect::<Vec<_>>(),
        "size_bytes": size,
    }))
}
