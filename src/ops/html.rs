//! The HTML that Markdown documents carry, turned into the Markdown that says the same.
//!
//! Markdown lets HTML through as it is. `create` has no HTML engine, so the common
//! tags are rewritten into what it does understand: headings, paragraphs, emphasis,
//! links, images, lists, quotes, code and tables. How the text is to look, as far as
//! `css` reads it, goes along inside the text; see there.

use pulldown_cmark::{Event, Options, Parser};

use super::css::{self, Align, Element, Look, Sheet, Size};

/// What is being converted, across the fragments Markdown hands over one at a time.
#[derive(Default)]
struct Converter {
    /// Open lists: the number of the next item of an ordered one, `None` for bullets.
    lists: Vec<Option<u32>>,
    /// The rows of the table being read, and the cell being filled.
    table: Option<Vec<Vec<String>>>,
    cell: Option<String>,
    /// Where open links lead.
    links: Vec<String>,
    /// Inside `pre`: text is kept as it is written.
    verbatim: bool,
    /// Inside an element whose text is not part of the document, up to this closing tag.
    skipping: Option<String>,
    /// Tags that mean nothing here and were dropped; their text stays.
    unknown: usize,
    /// The rules of the style sheets met so far, and the text of the one being read.
    sheet: Sheet,
    styles: Option<String>,
    /// The elements that are open, each with whether a look was opened for it.
    open: Vec<(Element, bool)>,
    /// The looks of the table and of the row being read, which their cells take on.
    table_look: Look,
    row_look: Look,
}

/// Elements that stand on lines of their own: a background fills them, not their words.
const BLOCKS: [&str; 22] = [
    "p",
    "div",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "td",
    "th",
    "tr",
    "table",
    "li",
    "ul",
    "ol",
    "blockquote",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "body",
];

/// Elements that hold nothing and are never closed.
const VOID: [&str; 9] = [
    "br", "img", "hr", "meta", "link", "input", "col", "wbr", "source",
];

/// The character an entity such as `&amp;` or `&#233;` stands for.
fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    })
}

fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let known = rest[1..]
            .find(';')
            .filter(|end| *end <= 10)
            .and_then(|end| Some((entity(&rest[1..1 + end])?, end + 2)));
        match known {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out + rest
}

/// The value of an attribute in the inside of a tag, quoted or not.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let at = from + at;
        from = at + name.len();
        let before = lower[..at].chars().next_back();
        let after = tag[from..].trim_start();
        if before.is_some_and(|c| !c.is_whitespace()) || !after.starts_with('=') {
            continue;
        }
        let value = after[1..].trim_start();
        let value = match value.chars().next()? {
            quote @ ('"' | '\'') => value[1..].split(quote).next()?,
            _ => value.split_whitespace().next()?,
        };
        return Some(decode(value));
    }
    None
}

impl Converter {
    /// Adds to the cell being filled, or to the text.
    fn put(&mut self, out: &mut String, text: &str) {
        match &mut self.cell {
            Some(cell) => cell.push_str(text),
            None if self.table.is_some() => {}
            None => out.push_str(text),
        }
    }

    fn text(&mut self, out: &mut String, text: &str) {
        if let Some(styles) = &mut self.styles {
            styles.push_str(text);
            return;
        }
        if self.skipping.is_some() {
            return;
        }
        let text = decode(text);
        if self.verbatim {
            self.put(out, &text);
            return;
        }
        // Line breaks and runs of spaces in HTML are one space.
        let mut flat = String::with_capacity(text.len());
        for word in text.split_whitespace() {
            if !flat.is_empty() {
                flat.push(' ');
            }
            flat.push_str(word);
        }
        let starts = text.starts_with(char::is_whitespace);
        let ends = text.ends_with(char::is_whitespace);
        let fresh = match &self.cell {
            Some(cell) => cell.is_empty(),
            None => out.is_empty() || out.ends_with('\n'),
        };
        if flat.is_empty() {
            if !fresh {
                self.put(out, " ");
            }
            return;
        }
        if starts && !fresh {
            self.put(out, " ");
        }
        self.put(out, &flat);
        if ends {
            self.put(out, " ");
        }
    }

    /// One tag, given without its angle brackets.
    fn tag(&mut self, out: &mut String, inside: &str) {
        let closing = inside.starts_with('/');
        let body = inside.trim_start_matches('/').trim_end_matches('/');
        let name: String = body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        // A style sheet is read wherever it stands, in the head as a rule.
        if name == "style" {
            match self.styles.take() {
                Some(styles) if closing => self.sheet.add(&css::without_comments(&styles)),
                _ if !closing => self.styles = Some(String::new()),
                _ => {}
            }
        }
        if let Some(until) = &self.skipping {
            if closing && *until == name {
                self.skipping = None;
            }
            return;
        }
        if closing {
            self.close(out, &[name.as_str()], &[]);
        } else {
            // Elements whose end tag is left out end where the next of their kind begins.
            match name.as_str() {
                "li" => self.close(out, &["li"], &["ul", "ol"]),
                "td" | "th" => self.close(out, &["td", "th"], &["tr", "table"]),
                "tr" => self.close(out, &["tr"], &["table"]),
                "p" => self.close(out, &["p"], &BLOCKS[1..]),
                _ => {}
            }
        }
        self.tag_itself(out, &name, closing, body);
        if !closing && !VOID.contains(&name.as_str()) && !inside.ends_with('/') {
            let element = Element {
                classes: attribute(body, "class")
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(str::to_string)
                    .collect(),
                id: attribute(body, "id"),
                tag: name.clone(),
            };
            let look = self.look_of(&element, body);
            let said = !look.is_plain() && !self.verbatim && self.skipping.is_none();
            match name.as_str() {
                // Nothing is written between the cells of a table: they take the look on.
                "table" => self.table_look = look,
                "tr" => self.row_look = look,
                _ if said => self.put(out, &look.marker()),
                _ => {}
            }
            let carried = said && !matches!(name.as_str(), "table" | "tr");
            self.open.push((element, carried));
        }
    }

    /// Ends the innermost open element that is one of `names`, and what is open inside
    /// it, unless one of `stops` encloses it more closely: the looks opened for them end.
    fn close(&mut self, out: &mut String, names: &[&str], stops: &[&str]) {
        let found = self
            .open
            .iter()
            .rposition(|(element, _)| {
                names.contains(&element.tag.as_str()) || stops.contains(&element.tag.as_str())
            })
            .filter(|&at| names.contains(&self.open[at].0.tag.as_str()));
        if let Some(at) = found {
            for (element, carried) in self.open.split_off(at).into_iter().rev() {
                if carried {
                    self.put(out, &css::POP.to_string());
                }
                match element.tag.as_str() {
                    "table" => self.table_look = Look::default(),
                    "tr" => self.row_look = Look::default(),
                    _ => {}
                }
            }
        }
    }

    /// The look an element asks for: by what it is, by the style sheets, by its
    /// attributes, and last by its own `style`.
    fn look_of(&self, element: &Element, body: &str) -> Look {
        let name = element.tag.as_str();
        let block = BLOCKS.contains(&name);
        let mut look = Look::default();
        match name {
            "u" | "ins" => look.underline = Some(true),
            "s" | "del" | "strike" => look.strike = Some(true),
            "mark" => look.highlight = Some([255, 255, 0]),
            "center" => look.align = Some(Align::Center),
            "small" => look.size = Some(Size::Times(0.85)),
            "big" => look.size = Some(Size::Times(1.2)),
            "td" | "th" => look = self.row_look.over(self.table_look),
            _ => {}
        }
        let around: Vec<Element> = self.open.iter().map(|(e, _)| e.clone()).collect();
        look = self.sheet.look(element, &around, block).over(look);
        let asked = Look {
            color: attribute(body, "color").and_then(|c| css::color(&c)),
            fill: attribute(body, "bgcolor").and_then(|c| css::color(&c)),
            align: match attribute(body, "align").as_deref() {
                Some("left") => Some(Align::Left),
                Some("center") => Some(Align::Center),
                Some("right") => Some(Align::Right),
                _ => None,
            },
            ..Default::default()
        };
        look = asked.over(look);
        let own = attribute(body, "style").unwrap_or_default();
        Look::declared(&own, block).over(look)
    }

    /// What a tag becomes in Markdown.
    fn tag_itself(&mut self, out: &mut String, name: &str, closing: bool, body: &str) {
        let mark = |open: &'static str| open;
        match (name, closing) {
            ("b" | "strong", _) => self.put(out, mark("**")),
            ("i" | "em" | "cite" | "var", _) => self.put(out, mark("*")),
            ("code" | "kbd" | "tt" | "samp", _) if !self.verbatim => self.put(out, mark("`")),
            ("br", _) => {
                let line_break = if self.cell.is_some() { " " } else { "\\\n" };
                self.put(out, line_break);
            }
            ("p" | "div" | "section" | "article" | "header" | "footer" | "main", _) => {
                self.put(out, "\n\n")
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                let level = name[1..].parse().unwrap_or(1);
                self.put(out, &format!("\n\n{} ", "#".repeat(level)));
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => self.put(out, "\n\n"),
            ("a", false) => {
                self.links.push(attribute(body, "href").unwrap_or_default());
                self.put(out, "[");
            }
            ("a", true) => {
                if let Some(to) = self.links.pop() {
                    self.put(out, &format!("]({to})"));
                }
            }
            ("img", _) => {
                let (alt, src) = (attribute(body, "alt"), attribute(body, "src"));
                self.put(
                    out,
                    &format!(
                        "![{}]({})",
                        alt.unwrap_or_default(),
                        src.unwrap_or_default()
                    ),
                );
            }
            ("hr", _) => self.put(out, "\n\n---\n\n"),
            ("ul", false) => self.lists.push(None),
            ("ol", false) => self.lists.push(Some(1)),
            ("ul" | "ol", true) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.put(out, "\n\n");
                }
            }
            ("li", false) => {
                let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                let bullet = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        *number += 1;
                        format!("{}. ", *number - 1)
                    }
                    _ => "- ".to_string(),
                };
                self.put(out, &format!("\n{indent}{bullet}"));
            }
            ("blockquote", false) => self.put(out, "\n\n> "),
            ("blockquote", true) => self.put(out, "\n\n"),
            ("pre", false) => {
                self.put(out, "\n\n```\n");
                self.verbatim = true;
            }
            ("pre", true) => {
                self.verbatim = false;
                self.put(out, "\n```\n\n");
            }
            ("table", false) => self.table = Some(Vec::new()),
            ("tr", false) => {
                if let Some(rows) = &mut self.table {
                    rows.push(Vec::new());
                }
            }
            ("td" | "th", false) if self.table.is_some() => self.cell = Some(String::new()),
            ("td" | "th", true) => {
                if let (Some(cell), Some(row)) = (
                    self.cell.take(),
                    self.table.as_mut().and_then(|rows| rows.last_mut()),
                ) {
                    row.push(cell.trim().replace('|', "\\|").replace('\n', " "));
                }
            }
            ("table", true) => {
                self.cell = None;
                let rows = self.table.take().unwrap_or_default();
                let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
                if columns > 0 {
                    out.push_str("\n\n");
                    for (i, row) in rows.iter().filter(|row| !row.is_empty()).enumerate() {
                        let cells: Vec<&str> = (0..columns)
                            .map(|c| row.get(c).map_or("", String::as_str))
                            .collect();
                        out.push_str(&format!("| {} |\n", cells.join(" | ")));
                        if i == 0 {
                            out.push_str(&format!("|{}\n", " --- |".repeat(columns)));
                        }
                    }
                    out.push('\n');
                }
            }
            ("script" | "style" | "title" | "head", false) if self.skipping.is_none() => {
                self.skipping = Some(name.to_string())
            }
            // Containers and styling that change nothing here.
            (
                "li" | "tr" | "tbody" | "thead" | "tfoot" | "span" | "u" | "font" | "small" | "big"
                | "s" | "del" | "strike" | "ins" | "mark" | "sup" | "sub" | "center" | "html"
                | "body" | "nav" | "label" | "abbr" | "figure" | "figcaption" | "caption"
                | "colgroup" | "col" | "code" | "kbd" | "tt" | "samp" | "script" | "style"
                | "title" | "head" | "td" | "th",
                _,
            ) => {}
            (_, false) => self.unknown += 1,
            (_, true) => {}
        }
    }

    /// One fragment of HTML as Markdown.
    fn feed(&mut self, fragment: &str) -> String {
        let mut out = String::new();
        let mut rest = fragment;
        while !rest.is_empty() {
            if let Some(comment) = rest.strip_prefix("<!--") {
                rest = comment.split_once("-->").map_or("", |(_, after)| after);
            } else if rest.starts_with('<')
                && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '/')
                && let Some(end) = rest.find('>')
            {
                self.tag(&mut out, &rest[1..end]);
                rest = &rest[end + 1..];
            } else {
                let end = rest[1..].find('<').map_or(rest.len(), |at| at + 1);
                self.text(&mut out, &rest[..end]);
                rest = &rest[end..];
            }
        }
        out
    }
}

/// `source` with the HTML in it rewritten as Markdown, and how many tags were dropped
/// for meaning nothing here.
///
/// Only what Markdown itself takes for HTML is touched, so a tag written inside a code
/// block stays as it is written.
pub fn to_markdown(source: &str, options: Options) -> (String, usize) {
    let mut converter = Converter::default();
    let mut out = String::with_capacity(source.len());
    let mut copied = 0;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        if !matches!(event, Event::Html(_) | Event::InlineHtml(_)) || range.start < copied {
            continue;
        }
        out.push_str(&source[copied..range.start]);
        out.push_str(&converter.feed(&source[range.clone()]));
        copied = range.end;
    }
    out.push_str(&source[copied..]);
    (out, converter.unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn converted(source: &str) -> String {
        to_markdown(
            source,
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
        )
        .0
    }

    #[test]
    fn inline_tags_become_markdown_where_they_stand() {
        assert_eq!(
            converted(
                "a <b>bold</b> and <em>slanted</em> word, <a href=\"https://x.test/?a=1&amp;b=2\">a link</a><br>next"
            ),
            "a **bold** and *slanted* word, [a link](https://x.test/?a=1&b=2)\\\nnext"
        );
        // What Markdown takes for code is not HTML.
        assert_eq!(
            converted("`<b>` and\n\n    <i>kept</i>\n"),
            "`<b>` and\n\n    <i>kept</i>\n"
        );
    }

    #[test]
    fn blocks_lists_and_tables_become_markdown() {
        let page = "<h2>Parts &amp; prices</h2>\n<p>First\nparagraph.</p>\n<ul>\n<li>one</li>\n<li>two\n<ol><li>inner</li></ol></li>\n</ul>\n<table>\n<tr><th>Part</th><th>Price</th></tr>\n<tr><td>Bolt</td><td>2 | 3</td></tr>\n</table>\n<pre>keep   this\n  as is</pre>\n<img src=\"a.png\" alt=\"A\"><hr>";
        let markdown = converted(page);
        for expected in [
            "## Parts & prices\n",
            "First paragraph.",
            "\n- one \n- two \n  1. inner",
            "| Part | Price |\n| --- | --- |\n| Bolt | 2 \\| 3 |\n",
            "```\nkeep   this\n  as is\n```",
            "![A](a.png)",
            "\n---\n",
        ] {
            assert!(
                markdown.contains(expected),
                "{expected:?} not in {markdown:?}"
            );
        }
    }

    #[test]
    fn looks_are_carried_in_the_text_and_closed_where_their_element_ends() {
        let look = |css: &str, block: bool| Look::declared(css, block).marker();
        let pop = css::POP;
        // An inline element: the look opens after what the tag itself becomes.
        assert_eq!(
            converted("a <b style=\"color: red\">b</b> c"),
            format!("a **{}b{pop}** c", look("color: red", false))
        );
        // A sheet in the head, a rule by class and one by what the element stands in.
        let page = "<head><style>.n { color: #00f } div p { text-align: right }</style></head>\n\
                    <div>\n<p class=\"n\">one\n<p>two</p>\n</div>\n<p>three</p>\n";
        let (one, two) = (
            look("color: #00f; text-align: right", true),
            look("text-align: right", true),
        );
        let out = converted(page);
        // The paragraph left open ends where the next one begins.
        assert!(out.contains(&format!("{one}one {pop}")), "{out:?}");
        assert!(out.contains(&format!("{two}two{pop}")), "{out:?}");
        assert!(out.contains("three") && !out.contains("color"), "{out:?}");
        assert_eq!(out.matches(css::OPEN).count(), out.matches(pop).count());
        // Cells take the look of their row on, and elements that only mean a look say it.
        let table = converted(
            "<table><tr bgcolor=\"#eee\"><td align=\"right\">1</td><td><u>2</u></td></tr></table>\n",
        );
        let cell = Look {
            fill: Some([238, 238, 238]),
            align: Some(Align::Right),
            ..Default::default()
        };
        assert!(
            table.contains(&format!("{}1{pop}", cell.marker())),
            "{table:?}"
        );
        let underlined = Look {
            underline: Some(true),
            ..Default::default()
        };
        assert!(
            table.contains(&format!("{}2{pop}", underlined.marker())),
            "{table:?}"
        );
        // Nothing is opened where there is nothing to say, nor inside code.
        assert_eq!(converted("<span class=\"x\">plain</span>"), "plain");
        assert!(!converted("<pre><span style=\"color:red\">x</span></pre>\n").contains(css::OPEN));
    }

    #[test]
    fn scripts_go_and_unknown_tags_are_counted() {
        let (markdown, unknown) = to_markdown(
            "<div>\n<script>alert(1)</script><style>p{}</style><marquee>shown</marquee> &#233;&nosuch;\n</div>",
            Options::empty(),
        );
        assert!(
            !markdown.contains("alert") && !markdown.contains("p{}"),
            "{markdown}"
        );
        assert!(markdown.contains("shown \u{e9}&nosuch;"), "{markdown}");
        assert_eq!(unknown, 1);
    }
}
