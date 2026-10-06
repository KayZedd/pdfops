//! Creating a PDF from Markdown: headings, paragraphs, emphasis, links, lists,
//! quotes, code, tables and images, laid out over as many pages as needed.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use clap::{Args, ValueEnum};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::doc;
use crate::font::{Style, TextFont};
use crate::ops::css::{self, Align, Look, Size};
use crate::ops::edit::embed_image;

#[derive(ValueEnum, Deserialize, JsonSchema, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PageSize {
    A4,
    Letter,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    /// Markdown file to convert
    pub input: Option<PathBuf>,
    /// Markdown text to convert, instead of a file
    #[arg(long)]
    pub markdown: Option<String>,
    /// Where to write the PDF
    #[arg(short, long)]
    pub output: PathBuf,
    /// Document title for the metadata (default: the first heading)
    #[arg(long)]
    pub title: Option<String>,
    /// Paper size (default: a4)
    #[arg(long, value_enum)]
    pub page_size: Option<PageSize>,
    /// Page margin in points (default: 56)
    #[arg(long)]
    pub margin: Option<f64>,
    /// Body text size in points (default: 11)
    #[arg(long)]
    pub font_size: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
struct Span {
    text: String,
    style: Style,
    link: Option<String>,
    /// What styling asked for, where the text came from HTML.
    look: Look,
}

#[derive(Debug, PartialEq)]
enum Block {
    /// A paragraph, or a heading when `level` is 1 to 6.
    Text {
        spans: Vec<Span>,
        level: u8,
    },
    Code(String),
    Rule,
    List {
        first: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    /// Rows of cells; the first row is the header.
    Table(Vec<Vec<Vec<Span>>>),
    Image(String),
}

/// Reads inline content up to `until`, or (for text sitting directly in a list item)
/// up to the next block. Images found on the way are returned separately.
///
/// `looks` holds the looks that are open, the innermost last. Text carries the marks
/// that open and close them, see `css`; they are taken out of it here, and a look
/// stays open across blocks until its mark closes it.
fn inline(
    events: &[Event<'_>],
    at: &mut usize,
    until: Option<TagEnd>,
    looks: &mut Vec<Look>,
) -> (Vec<Span>, Vec<String>) {
    let (mut spans, mut images) = (Vec::new(), Vec::new());
    let (mut bold, mut italic, mut struck) = (0u32, 0u32, 0u32);
    let mut links: Vec<String> = Vec::new();
    let mut push = |text: &str,
                    mono: bool,
                    (bold, italic, struck): (u32, u32, u32),
                    links: &[String],
                    looks: &mut Vec<Look>| {
        let mut rest = text;
        loop {
            let mark = rest.find([css::OPEN, css::POP]);
            let plain = &rest[..mark.unwrap_or(rest.len())];
            if !plain.is_empty() {
                let mut look = looks.last().copied().unwrap_or_default();
                if struck > 0 {
                    look.strike = Some(true);
                }
                spans.push(Span {
                    text: plain.to_string(),
                    style: Style {
                        bold: bold > 0 || look.bold == Some(true),
                        italic: italic > 0 || look.italic == Some(true),
                        mono,
                    },
                    link: links.last().cloned(),
                    look,
                });
            }
            let Some(mark) = mark else {
                break;
            };
            rest = &rest[mark..];
            if let Some(after) = rest.strip_prefix(css::POP) {
                looks.pop();
                rest = after;
            } else {
                let described = rest.find(css::END).unwrap_or(rest.len());
                let look = Look::from_marker(&rest[css::OPEN.len_utf8()..described]);
                looks.push(look.over(looks.last().copied().unwrap_or_default()));
                rest = rest.get(described + css::END.len_utf8()..).unwrap_or("");
            }
        }
    };
    while let Some(event) = events.get(*at) {
        match event {
            Event::End(end) if Some(*end) == until => {
                *at += 1;
                break;
            }
            Event::Text(t) => push(t, false, (bold, italic, struck), &links, looks),
            Event::Code(t) => push(t, true, (bold, italic, struck), &links, looks),
            Event::SoftBreak => push(" ", false, (bold, italic, struck), &links, looks),
            Event::HardBreak => push("\n", false, (bold, italic, struck), &links, looks),
            Event::Start(Tag::Strong) => bold += 1,
            Event::End(TagEnd::Strong) => bold = bold.saturating_sub(1),
            Event::Start(Tag::Emphasis) => italic += 1,
            Event::End(TagEnd::Emphasis) => italic = italic.saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) => links.push(dest_url.to_string()),
            Event::End(TagEnd::Link) => {
                links.pop();
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                images.push(dest_url.to_string());
                // The alternative text is not drawn.
                while events
                    .get(*at)
                    .is_some_and(|e| !matches!(e, Event::End(TagEnd::Image)))
                {
                    *at += 1;
                }
            }
            Event::Start(Tag::Strikethrough) => struck += 1,
            Event::End(TagEnd::Strikethrough) => struck = struck.saturating_sub(1),
            Event::Start(Tag::Superscript | Tag::Subscript)
            | Event::End(TagEnd::Superscript | TagEnd::Subscript)
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {}
            // Anything else is a block boundary.
            _ if until.is_none() => break,
            _ => {}
        }
        *at += 1;
    }
    (spans, images)
}

/// Reads blocks up to the end of the enclosing container.
fn blocks(events: &[Event<'_>], at: &mut usize, looks: &mut Vec<Look>) -> Vec<Block> {
    let mut out = Vec::new();
    let text = |out: &mut Vec<Block>, (spans, images): (Vec<Span>, Vec<String>), level: u8| {
        if spans.iter().any(|s| !s.text.trim().is_empty()) {
            out.push(Block::Text { spans, level });
        }
        out.extend(images.into_iter().map(Block::Image));
    };
    while let Some(event) = events.get(*at) {
        *at += 1;
        match event {
            Event::End(_) => break,
            Event::Rule => out.push(Block::Rule),
            Event::Start(Tag::Paragraph) => text(
                &mut out,
                inline(events, at, Some(TagEnd::Paragraph), looks),
                0,
            ),
            Event::Start(Tag::Heading { level, .. }) => text(
                &mut out,
                inline(events, at, Some(TagEnd::Heading(*level)), looks),
                *level as u8,
            ),
            Event::Start(Tag::BlockQuote(_)) => out.push(Block::Quote(blocks(events, at, looks))),
            Event::Start(Tag::CodeBlock(_)) => {
                let mut code = String::new();
                while let Some(Event::Text(t)) = events.get(*at) {
                    code.push_str(t);
                    *at += 1;
                }
                *at += 1;
                out.push(Block::Code(code));
            }
            Event::Start(Tag::List(first)) => {
                let mut items = Vec::new();
                while let Some(Event::Start(Tag::Item)) = events.get(*at) {
                    *at += 1;
                    items.push(blocks(events, at, looks));
                }
                *at += 1;
                out.push(Block::List {
                    first: *first,
                    items,
                });
            }
            Event::Start(Tag::Table(_)) => {
                let mut rows = Vec::new();
                while let Some(Event::Start(Tag::TableHead | Tag::TableRow)) = events.get(*at) {
                    *at += 1;
                    let mut cells = Vec::new();
                    while let Some(Event::Start(Tag::TableCell)) = events.get(*at) {
                        *at += 1;
                        cells.push(inline(events, at, Some(TagEnd::TableCell), looks).0);
                    }
                    *at += 1;
                    rows.push(cells);
                }
                *at += 1;
                out.push(Block::Table(rows));
            }
            // Containers that are not laid out specially still show their content.
            Event::Start(Tag::HtmlBlock) => {
                while events
                    .get(*at)
                    .is_some_and(|e| !matches!(e, Event::End(TagEnd::HtmlBlock)))
                {
                    *at += 1;
                }
                *at += 1;
            }
            Event::Start(_) => out.extend(blocks(events, at, looks)),
            // Text directly inside a list item, without a paragraph around it.
            Event::Text(_) | Event::Code(_) => {
                *at -= 1;
                text(&mut out, inline(events, at, None, looks), 0);
            }
            _ => {}
        }
    }
    out
}

/// A word (or a fragment glued to the previous one) ready to be placed.
#[derive(Clone)]
struct Token {
    text: String,
    style: Style,
    link: Option<String>,
    look: Look,
    /// A space precedes it in the source.
    spaced: bool,
    /// A hard line break precedes it.
    breaks: bool,
}

fn tokens(spans: &[Span], force_bold: bool) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let (mut spaced, mut breaks) = (false, false);
    for span in spans {
        let style = Style {
            bold: span.style.bold || force_bold,
            ..span.style
        };
        let mut word = String::new();
        let mut flush = |word: &mut String, spaced: &mut bool, breaks: &mut bool| {
            if !word.is_empty() {
                let token = Token {
                    text: std::mem::take(word),
                    style,
                    link: span.link.clone(),
                    look: span.look,
                    spaced: *spaced,
                    breaks: *breaks,
                };
                out.push(token);
                (*spaced, *breaks) = (false, false);
            }
        };
        for c in span.text.chars() {
            if c == '\n' {
                flush(&mut word, &mut spaced, &mut breaks);
                breaks = true;
            } else if c.is_whitespace() {
                flush(&mut word, &mut spaced, &mut breaks);
                spaced = true;
            } else {
                word.push(c);
            }
        }
        flush(&mut word, &mut spaced, &mut breaks);
    }
    out
}

#[derive(Default)]
struct PageBuffer {
    ops: String,
    links: Vec<([f64; 4], String)>,
}

struct Writer {
    d: Document,
    fonts: HashMap<Style, (TextFont, Vec<String>)>,
    images: HashMap<String, (ObjectId, String, f64, f64)>,
    pages: Vec<PageBuffer>,
    height: f64,
    margin: f64,
    /// Body font size.
    base: f64,
    /// Top of the next line, measured from the bottom of the page.
    y: f64,
}

const LEADING: f64 = 1.38;

impl Writer {
    fn page(&mut self) -> &mut PageBuffer {
        self.pages.last_mut().expect("a writer always has a page")
    }

    fn new_page(&mut self) {
        self.pages.push(PageBuffer::default());
        self.y = self.height - self.margin;
    }

    /// Starts a new page unless `height` still fits on this one.
    fn need(&mut self, height: f64) {
        if self.y - height < self.margin && self.y < self.height - self.margin {
            self.new_page();
        }
    }

    fn gap(&mut self, height: f64) {
        // No blank space at the top of a page.
        if self.y < self.height - self.margin {
            self.y -= height;
        }
    }

    fn font(&self, style: Style) -> &(TextFont, Vec<String>) {
        self.fonts
            .get(&style)
            .or_else(|| self.fonts.get(&Style::default()))
            .expect("the regular font always exists")
    }

    fn measure(&self, token: &Token, size: f64) -> f64 {
        self.font(token.style).0.width(&token.text, size)
    }

    /// Breaks tokens into lines no wider than `width`; each token gets its x offset.
    fn wrap(&self, tokens: &[Token], size: f64, width: f64) -> Vec<Vec<(f64, Token)>> {
        let mut lines: Vec<Vec<(f64, Token)>> = vec![Vec::new()];
        let mut x = 0.0;
        for token in tokens {
            let space = if token.spaced {
                self.font(token.style).0.width(" ", size)
            } else {
                0.0
            };
            let w = self.measure(token, size);
            let line = lines.last().expect("starts with one line");
            if token.breaks || (!line.is_empty() && x + space + w > width) {
                lines.push(Vec::new());
                x = 0.0;
            }
            let line = lines.last_mut().expect("starts with one line");
            if !line.is_empty() {
                x += space;
            }
            line.push((x, token.clone()));
            x += w;
        }
        lines.retain(|l| !l.is_empty());
        // The paragraph runs the way of its first letter that has a direction.
        let from_the_right = tokens
            .iter()
            .flat_map(|t| t.text.chars())
            .find_map(|c| match unicode_bidi::bidi_class(c) {
                unicode_bidi::BidiClass::L => Some(false),
                unicode_bidi::BidiClass::R | unicode_bidi::BidiClass::AL => Some(true),
                _ => None,
            })
            .unwrap_or(false);
        for line in &mut lines {
            self.reorder(line, size, from_the_right, width);
        }
        lines
    }

    /// Puts a line holding right-to-left text in the order it is read in.
    ///
    /// The line is taken as a whole, so that a comma after a Hebrew word in an English
    /// sentence lands where the sentence continues: words are cut where the direction
    /// changes, and each stretch of one direction is laid out from its own end. A
    /// paragraph that runs from the right is set against the right edge of `width`.
    fn reorder(&self, line: &mut Vec<(f64, Token)>, size: f64, from_the_right: bool, width: f64) {
        use unicode_bidi::BidiClass::{AL, R};
        let right_to_left = |c: char| matches!(unicode_bidi::bidi_class(c), R | AL);
        if !line.iter().any(|(_, t)| t.text.chars().any(right_to_left)) {
            return;
        }
        // The line as one string, with where each word lies in it.
        let mut text = String::new();
        let mut places = Vec::with_capacity(line.len());
        for (i, (_, token)) in line.iter().enumerate() {
            if i > 0 && token.spaced {
                text.push(' ');
            }
            places.push(text.len()..text.len() + token.text.len());
            text.push_str(&token.text);
        }
        let level = if from_the_right {
            unicode_bidi::Level::rtl()
        } else {
            unicode_bidi::Level::ltr()
        };
        let bidi = unicode_bidi::BidiInfo::new(&text, Some(level));
        let Some(paragraph) = bidi.paragraphs.first() else {
            return;
        };
        let (levels, runs) = bidi.visual_runs(paragraph, paragraph.range.clone());
        // The parts of words in the order they are drawn, and the spaces between
        // words (`None`) where their own direction puts them.
        let mut parts: Vec<(usize, Option<Token>)> = Vec::with_capacity(line.len());
        for run in runs {
            let mut inside: Vec<(usize, Option<Token>)> = Vec::new();
            for (i, place) in places.iter().enumerate() {
                let token = &line[i].1;
                if i > 0 && token.spaced && run.contains(&(place.start - 1)) {
                    inside.push((i, None));
                }
                let (from, to) = (place.start.max(run.start), place.end.min(run.end));
                if from >= to {
                    continue;
                }
                inside.push((
                    i,
                    Some(Token {
                        text: text[from..to].to_string(),
                        style: token.style,
                        link: token.link.clone(),
                        look: token.look,
                        spaced: token.spaced && from == place.start,
                        breaks: token.breaks && from == place.start,
                    }),
                ));
            }
            if levels[run.start].is_rtl() {
                inside.reverse();
            }
            parts.extend(inside);
        }
        let mut x = line[0].0;
        let mut placed = Vec::with_capacity(parts.len());
        for (i, part) in parts {
            match part {
                None => x += self.font(line[i].1.style).0.width(" ", size),
                Some(part) => {
                    let wide = self.measure(&part, size);
                    placed.push((x, part));
                    x += wide;
                }
            }
        }
        if from_the_right && width.is_finite() {
            let spare = (width - x).max(0.0);
            for part in &mut placed {
                part.0 += spare;
            }
        }
        *line = placed;
    }

    fn draw_line(&mut self, line: &[(f64, Token)], x0: f64, baseline: f64, size: f64) {
        let paint = |c: [u8; 3]| {
            format!(
                "{:.3} {:.3} {:.3}",
                c[0] as f64 / 255.0,
                c[1] as f64 / 255.0,
                c[2] as f64 / 255.0
            )
        };
        // Where the word before ended, and how it looked.
        let mut before: Option<(f64, Look)> = None;
        for (offset, token) in line {
            let (font, names) = self.font(token.style);
            let (text, width) = (
                font.show_named(names, &token.text, size),
                font.width(&token.text, size),
            );
            let x = x0 + offset;
            let look = token.look;
            // A mark under, through or behind words takes in the space between them.
            let from = |marked: bool| match before {
                Some((end, last)) if marked && last == look && end <= x => end,
                _ => x,
            };
            let mut ops = String::new();
            if let Some(color) = look.highlight {
                let from = from(true);
                ops += &format!(
                    "{} rg\n{from:.2} {:.2} {:.2} {:.2} re\nf\n",
                    paint(color),
                    baseline - size * 0.25,
                    x + width - from,
                    size * 1.1
                );
            }
            let color = match (look.color, &token.link) {
                (Some(color), _) => paint(color),
                (None, Some(_)) => "0.05 0.3 0.75".to_string(),
                (None, None) => "0 0 0".to_string(),
            };
            ops += &format!("BT\n{color} rg\n1 0 0 1 {x:.2} {baseline:.2} Tm\n{text}\nET\n");
            for (drawn, at) in [
                (look.underline == Some(true), -0.12),
                (look.strike == Some(true), 0.27),
            ] {
                if drawn {
                    ops += &format!(
                        "{color} rg\n{:.2} {:.2} {:.2} {:.2} re\nf\n",
                        from(true),
                        baseline + size * at,
                        x + width - from(true),
                        size * 0.06
                    );
                }
            }
            before = Some((x + width, look));
            let link = token.link.clone();
            let page = self.page();
            page.ops += &ops;
            if let Some(url) = link {
                page.links.push((
                    [x, baseline - size * 0.25, x + width, baseline + size * 0.8],
                    url,
                ));
            }
        }
    }

    /// How far a line moves to the right to stand where `align` puts it in `width`.
    fn aligned(&self, line: &[(f64, Token)], size: f64, width: f64, align: Option<Align>) -> f64 {
        let left = line.iter().map(|(x, _)| *x).fold(f64::INFINITY, f64::min);
        let right = line
            .iter()
            .map(|(x, token)| x + self.measure(token, size))
            .fold(0.0, f64::max);
        match align {
            Some(Align::Right) if left.is_finite() => width - right,
            Some(Align::Center) if left.is_finite() => (width - right - left) / 2.0,
            _ => 0.0,
        }
    }

    /// Lays out and draws a run of text, breaking pages between lines.
    ///
    /// The paragraph stands and is filled as its first words say: text taken from HTML
    /// brings along how its paragraph is aligned and what lies behind it.
    fn text(&mut self, spans: &[Span], size: f64, x0: f64, width: f64, bold: bool) {
        let look = spans.first().map(|s| s.look).unwrap_or_default();
        for line in self.wrap(&tokens(spans, bold), size, width) {
            self.need(size * LEADING);
            if let Some(fill) = look.fill {
                let bottom = self.y - size * (LEADING + 0.12);
                self.page().ops += &format!(
                    "{:.3} {:.3} {:.3} rg\n{x0:.2} {bottom:.2} {width:.2} {:.2} re\nf\n",
                    fill[0] as f64 / 255.0,
                    fill[1] as f64 / 255.0,
                    fill[2] as f64 / 255.0,
                    size * LEADING
                );
            }
            let shift = self.aligned(&line, size, width, look.align);
            self.draw_line(&line, x0 + shift, self.y - size, size);
            self.y -= size * LEADING;
        }
    }

    /// The size of a block of text: what its styling asks for, or `usual`.
    fn sized(&self, spans: &[Span], usual: f64) -> f64 {
        match spans.first().and_then(|s| s.look.size) {
            Some(Size::Points(points)) => (points as f64).clamp(4.0, 96.0),
            Some(Size::Times(times)) => (self.base * times as f64).clamp(4.0, 96.0),
            None => usual,
        }
    }

    fn block(&mut self, block: &Block, x0: f64, width: f64) {
        let base = self.base;
        match block {
            Block::Text { spans, level: 0 } => {
                let size = self.sized(spans, base);
                self.text(spans, size, x0, width, false);
                self.gap(base * 0.6);
            }
            Block::Text { spans, level } => {
                let usual = base * [1.0, 1.9, 1.5, 1.25, 1.1, 1.0, 1.0][(*level as usize).min(6)];
                let size = self.sized(spans, usual);
                self.gap(size * 0.6);
                // A heading stays with the line that follows it.
                self.need(size * LEADING + base * LEADING);
                self.text(spans, size, x0, width, true);
                self.gap(size * 0.35);
            }
            Block::Rule => {
                self.need(base);
                let y = self.y - base * 0.3;
                self.page().ops += &format!(
                    "0.6 G\n0.6 w\n{x0:.2} {y:.2} m\n{:.2} {y:.2} l\nS\n",
                    x0 + width
                );
                self.y -= base;
            }
            Block::Code(code) => {
                let size = base * 0.86;
                let line_height = size * 1.3;
                let style = Style {
                    mono: true,
                    ..Default::default()
                };
                let fit = ((width - 12.0) / self.font(style).0.width("M", size).max(0.1))
                    .floor()
                    .max(1.0) as usize;
                let lines: Vec<String> = code
                    .trim_end_matches('\n')
                    .lines()
                    .map(|l| l.replace('\t', "    "))
                    // Long lines continue on the next line rather than leaving the page.
                    .flat_map(|l| {
                        let chars: Vec<char> = l.chars().collect();
                        if chars.is_empty() {
                            vec![String::new()]
                        } else {
                            chars.chunks(fit).map(|c| c.iter().collect()).collect()
                        }
                    })
                    .collect();
                for line in &lines {
                    self.need(line_height);
                    let top = self.y;
                    let (font, names) = self.font(style);
                    let text = font.show_named(names, line, size);
                    self.page().ops += &format!(
                        "0.95 g\n{x0:.2} {:.2} {width:.2} {line_height:.2} re\nf\nBT\n0 g\n1 0 0 1 {:.2} {:.2} Tm\n{text}\nET\n",
                        top - line_height,
                        x0 + 6.0,
                        top - size
                    );
                    self.y -= line_height;
                }
                self.gap(base * 0.7);
            }
            Block::List { first, items } => {
                let indent = base * 1.7;
                for (i, item) in items.iter().enumerate() {
                    self.need(base * LEADING);
                    let baseline = self.y - base;
                    match first {
                        Some(n) => {
                            let label = Span {
                                text: format!("{}.", n + i as u64),
                                style: Style::default(),
                                link: None,
                                look: Look::default(),
                            };
                            let line = self.wrap(&tokens(&[label], false), base, indent);
                            if let Some(line) = line.first() {
                                self.draw_line(line, x0, baseline, base);
                            }
                        }
                        // A bullet drawn as a dot needs no glyph, so it works with every font.
                        None => {
                            let (cx, cy, r) = (x0 + base * 0.5, baseline + base * 0.3, base * 0.14);
                            let k = r * 0.5523;
                            self.page().ops += &format!(
                                "0 g\n{:.2} {cy:.2} m\n{:.2} {:.2} {:.2} {:.2} {cx:.2} {:.2} c\n{:.2} {:.2} {:.2} {:.2} {:.2} {cy:.2} c\n{:.2} {:.2} {:.2} {:.2} {cx:.2} {:.2} c\n{:.2} {:.2} {:.2} {:.2} {:.2} {cy:.2} c\nf\n",
                                cx + r,
                                cx + r,
                                cy + k,
                                cx + k,
                                cy + r,
                                cy + r,
                                cx - k,
                                cy + r,
                                cx - r,
                                cy + k,
                                cx - r,
                                cx - r,
                                cy - k,
                                cx - k,
                                cy - r,
                                cy - r,
                                cx + k,
                                cy - r,
                                cx + r,
                                cy - k,
                                cx + r
                            );
                        }
                    }
                    let before = self.pages.len();
                    for inner in item {
                        self.block(inner, x0 + indent, width - indent);
                    }
                    // Items sit closer together than paragraphs.
                    if self.pages.len() == before {
                        self.y += base * 0.3;
                    }
                }
                self.gap(base * 0.4);
            }
            Block::Quote(inner) => {
                let (page, top) = (self.pages.len(), self.y);
                for b in inner {
                    self.block(b, x0 + base * 1.3, width - base * 1.3);
                }
                // The bar runs beside the quote; after a page break, from the top of the new page.
                let top = if self.pages.len() == page {
                    top
                } else {
                    self.height - self.margin
                };
                let bottom = self.y + base * 0.5;
                self.page().ops += &format!(
                    "0.75 g\n{:.2} {bottom:.2} 2.5 {:.2} re\nf\n",
                    x0 + base * 0.3,
                    (top - bottom).max(0.0)
                );
            }
            Block::Table(rows) => self.table(rows, x0, width),
            Block::Image(source) => {
                let Some((_, name, natural, ratio)) = self.images.get(source).cloned() else {
                    return;
                };
                let room = self.height - 2.0 * self.margin;
                let mut w = natural.min(width);
                if w * ratio > room {
                    w = room / ratio;
                }
                let h = w * ratio;
                self.need(h);
                let y = self.y - h;
                self.page().ops +=
                    &format!("q\n{w:.2} 0 0 {h:.2} {x0:.2} {y:.2} cm\n/{name} Do\nQ\n");
                self.y -= h;
                self.gap(base * 0.7);
            }
        }
    }

    fn table(&mut self, rows: &[Vec<Vec<Span>>], x0: f64, width: f64) {
        let size = self.base * 0.92;
        let pad = 5.0;
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        // Each column wants the width of its widest cell and needs that of its longest word.
        let mut wanted = vec![0.0f64; columns];
        let mut needed = vec![0.0f64; columns];
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let words = tokens(cell, r == 0);
                let line = self
                    .wrap(&words, size, f64::INFINITY)
                    .iter()
                    .map(|l| l.last().map_or(0.0, |(x, t)| x + self.measure(t, size)))
                    .fold(0.0, f64::max);
                let word = words
                    .iter()
                    .map(|t| self.measure(t, size))
                    .fold(0.0, f64::max);
                wanted[c] = wanted[c].max(line + 2.0 * pad);
                needed[c] = needed[c].max(word + 2.0 * pad);
            }
        }
        let mut widths = wanted.clone();
        if wanted.iter().sum::<f64>() > width {
            let least: f64 = needed.iter().sum();
            if least <= width {
                // Words stay whole; the room left over goes to the columns that wrap.
                let extra: f64 = wanted
                    .iter()
                    .zip(&needed)
                    .map(|(w, n)| w - n)
                    .sum::<f64>()
                    .max(1e-9);
                widths = wanted
                    .iter()
                    .zip(&needed)
                    .map(|(w, n)| n + (w - n) / extra * (width - least))
                    .collect();
            } else {
                widths = needed.iter().map(|n| n / least * width).collect();
            }
        }
        for (r, row) in rows.iter().enumerate() {
            let cells: Vec<Vec<Vec<(f64, Token)>>> = (0..columns)
                .map(|c| {
                    self.wrap(
                        &tokens(row.get(c).map_or(&[][..], Vec::as_slice), r == 0),
                        size,
                        widths[c] - 2.0 * pad,
                    )
                })
                .collect();
            let lines = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
            let row_height = lines as f64 * size * LEADING + 2.0 * pad - size * (LEADING - 1.0);
            self.need(row_height);
            let top = self.y;
            let mut x = x0;
            for (c, cell) in cells.iter().enumerate() {
                let look = row
                    .get(c)
                    .and_then(|cell| cell.first())
                    .map(|span| span.look)
                    .unwrap_or_default();
                let fill = match look.fill {
                    Some(fill) => format!(
                        "{:.3} {:.3} {:.3} rg\n",
                        fill[0] as f64 / 255.0,
                        fill[1] as f64 / 255.0,
                        fill[2] as f64 / 255.0
                    ),
                    None if r == 0 => "0.92 g\n".to_string(),
                    None => "1 g\n".to_string(),
                };
                self.page().ops += &format!(
                    "{fill}0.55 G\n0.5 w\n{x:.2} {:.2} {:.2} {row_height:.2} re\nB\n",
                    top - row_height,
                    widths[c]
                );
                for (i, line) in cell.iter().enumerate() {
                    let shift = self.aligned(line, size, widths[c] - 2.0 * pad, look.align);
                    self.draw_line(
                        line,
                        x + pad + shift,
                        top - pad - size * 0.82 - i as f64 * size * LEADING,
                        size,
                    );
                }
                x += widths[c];
            }
            self.y -= row_height;
        }
        self.gap(self.base * 0.8);
    }
}

/// Collects the characters each style has to draw, and the images used.
fn survey(
    blocks: &[Block],
    chars: &mut BTreeMap<(bool, bool, bool), String>,
    images: &mut Vec<String>,
) {
    fn add(chars: &mut BTreeMap<(bool, bool, bool), String>, style: Style, text: &str) {
        let set = chars
            .entry((style.bold, style.italic, style.mono))
            .or_default();
        // Every font needs the space: words in it are set one by one, with measured gaps.
        set.push(' ');
        set.push_str(text);
    }
    for block in blocks {
        match block {
            Block::Text { spans, level } => {
                for s in spans {
                    add(
                        chars,
                        Style {
                            bold: s.style.bold || *level > 0,
                            ..s.style
                        },
                        &s.text,
                    );
                }
            }
            Block::Code(code) => add(
                chars,
                Style {
                    mono: true,
                    ..Default::default()
                },
                &format!("{code}M "),
            ),
            Block::List { items, .. } => {
                add(chars, Style::default(), "0123456789.");
                items.iter().for_each(|i| survey(i, chars, images));
            }
            Block::Quote(inner) => survey(inner, chars, images),
            Block::Table(rows) => {
                for (r, row) in rows.iter().enumerate() {
                    for s in row.iter().flatten() {
                        add(
                            chars,
                            Style {
                                bold: s.style.bold || r == 0,
                                ..s.style
                            },
                            &s.text,
                        );
                    }
                }
            }
            Block::Image(source) => images.push(source.clone()),
            Block::Rule => {}
        }
    }
}

fn first_heading(blocks: &[Block]) -> Option<String> {
    blocks.iter().find_map(|b| match b {
        Block::Text { spans, level } if *level > 0 => {
            Some(spans.iter().map(|s| s.text.as_str()).collect::<String>())
        }
        _ => None,
    })
}

pub fn create(a: CreateArgs) -> Result<Value> {
    let (source, folder) = match (&a.input, &a.markdown) {
        (Some(path), None) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| anyhow!("cannot read {}: {e}", path.display()))?;
            (
                text,
                path.parent().map(Path::to_path_buf).unwrap_or_default(),
            )
        }
        (None, Some(text)) => (text.clone(), PathBuf::new()),
        _ => bail!("give either a Markdown file or markdown text"),
    };
    let base = a.font_size.unwrap_or(11.0);
    let margin = a.margin.unwrap_or(56.0);
    let (width, height) = match a.page_size.unwrap_or(PageSize::A4) {
        PageSize::A4 => (595.28, 841.89),
        PageSize::Letter => (612.0, 792.0),
    };
    if !(4.0..=72.0).contains(&base) || margin < 0.0 || width - 2.0 * margin < 100.0 {
        bail!(
            "font size must be 4-72 points and the margin must leave at least 100 points of width"
        );
    }

    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    // HTML in the text is rewritten as the Markdown that says the same; tags that
    // mean nothing here are dropped and counted.
    let (source, html) = crate::ops::html::to_markdown(&source, options);
    let events: Vec<Event<'_>> = Parser::new_ext(&source, options).collect();
    let content = blocks(&events, &mut 0, &mut Vec::new());

    let mut chars = BTreeMap::new();
    let mut sources = Vec::new();
    survey(&content, &mut chars, &mut sources);
    chars.entry((false, false, false)).or_default();

    let mut d = Document::with_version("1.7");
    let mut fonts = HashMap::new();
    let mut resources_fonts = Dictionary::new();
    for (i, ((bold, italic, mono), text)) in chars.iter().enumerate() {
        let style = Style {
            bold: *bold,
            italic: *italic,
            mono: *mono,
        };
        let font = TextFont::styled(&mut d, text, style)?;
        // The font of a style, and behind it those standing in where it has no glyph.
        let names: Vec<String> = font
            .ids()
            .into_iter()
            .enumerate()
            .map(|(k, id)| {
                let name = if k == 0 {
                    format!("F{i}")
                } else {
                    format!("F{i}S{k}")
                };
                resources_fonts.set(name.as_str(), id);
                name
            })
            .collect();
        fonts.insert(style, (font, names));
    }
    let mut images = HashMap::new();
    let mut xobjects = Dictionary::new();
    for source in sources {
        if images.contains_key(&source) {
            continue;
        }
        if source.contains("://") {
            bail!("image '{source}' is remote; only local image files can be embedded");
        }
        // An image path comes from the document text, not from the caller's arguments.
        crate::sandbox::check(&folder.join(&source))?;
        let (id, ratio) = embed_image(&mut d, &folder.join(&source))?;
        let pixels = d
            .get_object(id)?
            .as_stream()?
            .dict
            .get(b"Width")?
            .as_i64()? as f64;
        let name = format!("Im{}", images.len());
        xobjects.set(name.as_str(), id);
        // Screen images are authored at 96 pixels per inch.
        images.insert(source, (id, name, pixels * 0.75, ratio));
    }

    let mut writer = Writer {
        d,
        fonts,
        images,
        pages: Vec::new(),
        height,
        margin,
        base,
        y: 0.0,
    };
    writer.new_page();
    for block in &content {
        writer.block(block, margin, width - 2.0 * margin);
    }

    let Writer { mut d, pages, .. } = writer;
    let mut resources = Dictionary::new();
    resources.set("Font", resources_fonts);
    resources.set("XObject", xobjects);
    let resources = d.add_object(resources);
    let pages_id = d.new_object_id();
    let mut kids = Vec::with_capacity(pages.len());
    for page in &pages {
        let mut stream = Stream::new(Dictionary::new(), page.ops.clone().into_bytes());
        let _ = stream.compress();
        let contents = d.add_object(stream);
        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"Page".to_vec()));
        dict.set("Parent", pages_id);
        dict.set(
            "MediaBox",
            vec![
                0.into(),
                0.into(),
                Object::Real(width as f32),
                Object::Real(height as f32),
            ],
        );
        dict.set("Resources", resources);
        dict.set("Contents", contents);
        let annots: Vec<Object> = page
            .links
            .iter()
            .map(|(rect, url)| {
                let mut action = Dictionary::new();
                action.set("S", Object::Name(b"URI".to_vec()));
                action.set("URI", Object::string_literal(url.as_str()));
                let mut annot = Dictionary::new();
                annot.set("Type", Object::Name(b"Annot".to_vec()));
                annot.set("Subtype", Object::Name(b"Link".to_vec()));
                annot.set("Rect", rect.map(|v| Object::Real(v as f32)).to_vec());
                annot.set("Border", vec![0.into(), 0.into(), 0.into()]);
                annot.set("A", action);
                Object::Reference(d.add_object(annot))
            })
            .collect();
        if !annots.is_empty() {
            dict.set("Annots", annots);
        }
        kids.push(Object::Reference(d.add_object(dict)));
    }
    let mut tree = Dictionary::new();
    tree.set("Type", Object::Name(b"Pages".to_vec()));
    tree.set("Count", kids.len() as i64);
    tree.set("Kids", kids);
    d.objects.insert(pages_id, Object::Dictionary(tree));
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", pages_id);
    let catalog = d.add_object(catalog);
    d.trailer.set("Root", catalog);

    let title = a.title.clone().or_else(|| first_heading(&content));
    let mut info = Dictionary::new();
    if let Some(title) = &title {
        info.set("Title", lopdf::text_string(title));
    }
    info.set(
        "Producer",
        Object::string_literal(concat!("pdfops ", env!("CARGO_PKG_VERSION"))),
    );
    let info = d.add_object(info);
    d.trailer.set("Info", info);

    let size = doc::save(&mut d, &a.output)?;
    Ok(json!({
        "output": a.output,
        "pages": pages.len(),
        "title": title,
        "html_fragments_ignored": html,
        "size_bytes": size,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(markdown: &str) -> Vec<Block> {
        let events: Vec<Event<'_>> = Parser::new_ext(markdown, Options::ENABLE_TABLES).collect();
        blocks(&events, &mut 0, &mut Vec::new())
    }

    fn plain(text: &str) -> Span {
        Span {
            text: text.to_string(),
            style: Style::default(),
            link: None,
            look: Look::default(),
        }
    }

    #[test]
    fn parses_inline_styles_and_links() {
        let parsed = parse("# Title\n\nSome **bold** and *it* `code` [go](http://x.y).");
        assert_eq!(
            parsed[0],
            Block::Text {
                spans: vec![plain("Title")],
                level: 1
            }
        );
        let Block::Text { spans, level: 0 } = &parsed[1] else {
            panic!("{parsed:?}")
        };
        let styles: Vec<(&str, bool, bool, bool, bool)> = spans
            .iter()
            .map(|s| {
                (
                    s.text.as_str(),
                    s.style.bold,
                    s.style.italic,
                    s.style.mono,
                    s.link.is_some(),
                )
            })
            .collect();
        assert_eq!(
            styles,
            [
                ("Some ", false, false, false, false),
                ("bold", true, false, false, false),
                (" and ", false, false, false, false),
                ("it", false, true, false, false),
                (" ", false, false, false, false),
                ("code", false, false, true, false),
                (" ", false, false, false, false),
                ("go", false, false, false, true),
                (".", false, false, false, false),
            ]
        );
    }

    #[test]
    fn parses_lists_tables_and_code() {
        let parsed = parse(
            "- a\n- b\n  1. c\n\n| h | i |\n|---|---|\n| 1 | 2 |\n\n```\nx\n```\n\n> q\n\n---\n",
        );
        let Block::List { first: None, items } = &parsed[0] else {
            panic!("{parsed:?}")
        };
        assert_eq!(
            items[0],
            vec![Block::Text {
                spans: vec![plain("a")],
                level: 0
            }]
        );
        assert!(
            matches!(&items[1][1], Block::List { first: Some(1), .. }),
            "{items:?}"
        );
        assert_eq!(
            parsed[1],
            Block::Table(vec![
                vec![vec![plain("h")], vec![plain("i")]],
                vec![vec![plain("1")], vec![plain("2")]]
            ])
        );
        assert_eq!(parsed[2], Block::Code("x\n".to_string()));
        assert_eq!(
            parsed[3],
            Block::Quote(vec![Block::Text {
                spans: vec![plain("q")],
                level: 0
            }])
        );
        assert_eq!(parsed[4], Block::Rule);
    }

    #[test]
    fn tokens_keep_glue_between_adjacent_spans() {
        let spans = [
            plain("pre"),
            Span {
                text: "fix word".into(),
                style: Style {
                    bold: true,
                    ..Default::default()
                },
                link: None,
                look: Look::default(),
            },
        ];
        let t = tokens(&spans, false);
        let seen: Vec<(&str, bool)> = t.iter().map(|t| (t.text.as_str(), t.spaced)).collect();
        // "pre" and "fix" touch in the source, so no space may appear between them.
        assert_eq!(seen, [("pre", false), ("fix", false), ("word", true)]);
    }
}
