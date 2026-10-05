//! The HTML that Markdown documents carry, turned into the Markdown that says the same.
//!
//! Markdown lets HTML through as it is. `create` has no HTML engine, so the common
//! tags are rewritten into what it does understand: headings, paragraphs, emphasis,
//! links, images, lists, quotes, code and tables. Styling is not read: there is no CSS.

use pulldown_cmark::{Event, Options, Parser};

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
}

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
        if let Some(until) = &self.skipping {
            if closing && *until == name {
                self.skipping = None;
            }
            return;
        }
        let mark = |open: &'static str| open;
        match (name.as_str(), closing) {
            ("b" | "strong", _) => self.put(out, mark("**")),
            ("i" | "em" | "cite" | "var", _) => self.put(out, mark("*")),
            ("s" | "del" | "strike", _) => self.put(out, mark("~~")),
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
            ("script" | "style" | "title" | "head", false) => self.skipping = Some(name),
            // Containers and styling that change nothing here.
            (
                "li" | "tr" | "tbody" | "thead" | "tfoot" | "span" | "u" | "font" | "small" | "big"
                | "mark" | "sup" | "sub" | "center" | "html" | "body" | "nav" | "label" | "abbr"
                | "figure" | "figcaption" | "caption" | "colgroup" | "col" | "code" | "kbd" | "tt"
                | "samp" | "script" | "style" | "title" | "head" | "td" | "th",
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
