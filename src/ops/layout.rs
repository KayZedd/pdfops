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
    InterpreterCache, InterpreterSettings, SoftMask, interpret_page,
};
use hayro::hayro_syntax::page::Page;
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
}

type FontInfo = (Option<Arc<str>>, bool, bool);

#[derive(Default)]
struct Collector {
    words: Vec<Word>,
    /// False once a space or a jump ended the current word.
    open: bool,
    rules: Vec<Rule>,
    fonts: HashMap<u128, FontInfo>,
}

impl Collector {
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
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, mode: &DrawMode) {
        let path = props.transform * path.clone();
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

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
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
            let origin = to_page * Point::new(0.0, 0.0);
            let end = to_page * Point::new(advance, 0.0);
            let up = (to_page * Point::new(0.0, 1000.0)) - origin;
            let size = up.hypot();
            let text = match glyph.as_unicode() {
                Some(BfString::Char(c)) => c.to_string(),
                Some(BfString::String(s)) => s,
                // Kept as a placeholder, so unmapped glyphs still occupy their place.
                None => "\u{fffd}".to_string(),
            };
            if size < 0.01 {
                continue;
            }
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
            let along = (end - origin) / (end - origin).hypot().max(1e-9);
            let continues = self.open
                && self.words.last().is_some_and(|w| {
                    let gap = origin - w.end;
                    (gap.x * along.x + gap.y * along.y).abs() < 0.15 * size
                        && (gap.x * along.y - gap.y * along.x).abs() < 0.4 * size
                        && (w.size - size).abs() < 0.1 * size
                });
            if continues {
                let word = self.words.last_mut().expect("checked above");
                word.parts.push((word.text.len(), bbox));
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
    let (w, h) = page.render_dimensions();
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, w as f64, h as f64),
        &cache,
        page.xref(),
        InterpreterSettings::default(),
    );
    let mut collector = Collector::default();
    interpret_page(page, &mut context, &mut collector);
    PageLayout {
        width: w as f64,
        height: h as f64,
        words: collector.words,
        rules: collector.rules,
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
/// guess is: it keeps columns, sidebars and captions together.
pub fn plain_text(words: &[Word]) -> String {
    let mut out = String::new();
    let mut previous: Option<&Word> = None;
    for word in words {
        if let Some(before) = previous {
            let drop = word.center().1 - before.center().1;
            let size = before.size.min(word.size);
            // On the same line: level with the word before, or, for text that is not
            // horizontal, starting about where that word ended.
            if drop.abs() < 0.5 * size || (word.start - before.end).hypot() < 1.2 * size {
                out.push(' ');
            } else {
                out.push('\n');
                // More than a line's worth of space separates paragraphs.
                if drop.abs() > 1.7 * before.size.max(word.size) {
                    out.push('\n');
                }
            }
        }
        out.push_str(&word.text);
        previous = Some(word);
    }
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
                    .map(|w| describe(w.text.clone(), w.bbox, w));
                v["words"] = words.collect();
            } else {
                let lines = grouped.iter().map(|line| {
                    let text = line
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    describe(text, union(line.iter().map(|w| w.bbox)), line[0])
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
        .map(|line| {
            line.iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
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

/// Tables without ruling lines, found from text that lines up in columns.
fn aligned_tables(page: &PageLayout, taken: &[[f64; 4]]) -> Vec<Table> {
    let inside = |w: &Word| {
        let (cx, cy) = w.center();
        taken
            .iter()
            .any(|b| cx >= b[0] && cx <= b[2] && cy >= b[1] && cy <= b[3])
    };
    let free: Vec<Word> = page.words.iter().filter(|w| !inside(w)).cloned().collect();
    let rows: Vec<Vec<Vec<&Word>>> = lines(&free).iter().map(|l| segments(l)).collect();

    let mut tables = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        // A block is a run of consecutive lines that each have several cells.
        let mut end = start;
        while end < rows.len() && rows[end].len() >= 2 {
            end += 1;
        }
        if end - start >= 2 {
            let block = &rows[start..end];
            // Columns are the stretches of x covered by some cell; the gaps between them separate columns.
            let mut spans: Vec<(f64, f64)> = block
                .iter()
                .flatten()
                .map(|cell| (cell[0].bbox[0], cell[cell.len() - 1].bbox[2]))
                .collect();
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut columns: Vec<(f64, f64)> = Vec::new();
            for (from, to) in spans {
                match columns.last_mut() {
                    Some(last) if from <= last.1 + 2.0 => last.1 = last.1.max(to),
                    _ => columns.push((from, to)),
                }
            }
            if columns.len() >= 2 {
                let text: Vec<Vec<String>> = block
                    .iter()
                    .map(|row| {
                        let mut cells = vec![Vec::<&Word>::new(); columns.len()];
                        for cell in row {
                            let x = cell[0].bbox[0];
                            let col = columns
                                .iter()
                                .position(|c| x >= c.0 - 2.0 && x <= c.1 + 2.0)
                                .unwrap_or(0);
                            cells[col].extend(cell.iter().copied());
                        }
                        cells
                            .iter()
                            .map(|c| {
                                c.iter()
                                    .map(|w| w.text.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .collect()
                    })
                    .collect();
                tables.push(Table {
                    bbox: union(block.iter().flatten().flatten().map(|w| w.bbox)),
                    rows: text,
                    method: "alignment",
                });
            }
        }
        start = end.max(start + 1);
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
}
