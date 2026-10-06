//! The part of CSS that `create` follows: how text looks and where it stands.
//!
//! Colour, background, weight, slant, underline and strike-through, size and
//! alignment, from `style` attributes and from `<style>` sheets whose selectors name
//! an element by tag, class or id, alone or inside others so named. Layout, the box
//! model and selectors that go by siblings, attributes or state are not read.
//!
//! HTML reaches `create` as Markdown, which has no words for any of this. A look is
//! therefore carried through the Markdown in the text itself, between characters
//! reserved for private use, and taken out again where the text is laid out.

/// Opens a look: what follows, up to `END`, says which.
pub const OPEN: char = '\u{e000}';
/// Ends the description of a look that `OPEN` began.
pub const END: char = '\u{e001}';
/// Closes the look opened last.
pub const POP: char = '\u{e002}';

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A font size: in points, or as a multiple of the size of body text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    Points(f32),
    Times(f32),
}

/// What styling says about a piece of text. Nothing is said where a field is `None`,
/// and what encloses the text decides.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    pub color: Option<[u8; 3]>,
    /// The background of the words themselves, as a marker pen leaves it.
    pub highlight: Option<[u8; 3]>,
    /// The background of the paragraph or table cell the text stands in.
    pub fill: Option<[u8; 3]>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub align: Option<Align>,
    pub size: Option<Size>,
}

/// The colours that have names people use.
const NAMED: [(&str, [u8; 3]); 26] = [
    ("black", [0, 0, 0]),
    ("white", [255, 255, 255]),
    ("red", [255, 0, 0]),
    ("green", [0, 128, 0]),
    ("blue", [0, 0, 255]),
    ("yellow", [255, 255, 0]),
    ("orange", [255, 165, 0]),
    ("purple", [128, 0, 128]),
    ("gray", [128, 128, 128]),
    ("grey", [128, 128, 128]),
    ("silver", [192, 192, 192]),
    ("maroon", [128, 0, 0]),
    ("navy", [0, 0, 128]),
    ("teal", [0, 128, 128]),
    ("olive", [128, 128, 0]),
    ("lime", [0, 255, 0]),
    ("aqua", [0, 255, 255]),
    ("cyan", [0, 255, 255]),
    ("fuchsia", [255, 0, 255]),
    ("magenta", [255, 0, 255]),
    ("pink", [255, 192, 203]),
    ("brown", [165, 42, 42]),
    ("gold", [255, 215, 0]),
    ("darkgray", [169, 169, 169]),
    ("lightgray", [211, 211, 211]),
    ("crimson", [220, 20, 60]),
];

/// A colour as CSS writes it: a name, `#rgb`, `#rrggbb` or `rgb(r, g, b)`.
pub fn color(value: &str) -> Option<[u8; 3]> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(hex) = value.strip_prefix('#') {
        let digits: Vec<u8> = hex
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<_>>()?;
        return match digits.as_slice() {
            [r, g, b] | [r, g, b, _] => Some([r * 17, g * 17, b * 17]),
            [r1, r2, g1, g2, b1, b2] | [r1, r2, g1, g2, b1, b2, _, _] => {
                Some([r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2])
            }
            _ => None,
        };
    }
    if let Some(inner) = value
        .strip_prefix("rgba(")
        .or_else(|| value.strip_prefix("rgb("))
    {
        let parts: Vec<f32> = inner
            .trim_end_matches(')')
            .split([',', ' ', '/'])
            .filter(|part| !part.is_empty())
            .map(|part| match part.strip_suffix('%') {
                Some(share) => share.parse::<f32>().map(|v| v * 2.55),
                None => part.parse::<f32>(),
            })
            .collect::<Result<_, _>>()
            .ok()?;
        let [r, g, b, ..] = parts.as_slice() else {
            return None;
        };
        return Some([*r, *g, *b].map(|v| v.clamp(0.0, 255.0).round() as u8));
    }
    NAMED
        .iter()
        .find(|(name, _)| *name == value)
        .map(|(_, rgb)| *rgb)
}

/// A font size as CSS writes it. Pixels are three quarters of a point.
fn size(value: &str) -> Option<Size> {
    let value = value.trim().to_ascii_lowercase();
    let times = match value.as_str() {
        "xx-small" => Some(0.6),
        "x-small" => Some(0.75),
        "small" | "smaller" => Some(0.85),
        "medium" => Some(1.0),
        "large" | "larger" => Some(1.2),
        "x-large" => Some(1.5),
        "xx-large" => Some(2.0),
        _ => None,
    };
    if let Some(times) = times {
        return Some(Size::Times(times));
    }
    let digits = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(value.len());
    let number: f32 = value[..digits].parse().ok().filter(|n| *n > 0.0)?;
    match &value[digits..] {
        "pt" => Some(Size::Points(number)),
        "px" | "" => Some(Size::Points(number * 0.75)),
        "em" | "rem" => Some(Size::Times(number)),
        "%" => Some(Size::Times(number / 100.0)),
        _ => None,
    }
}

impl Look {
    /// This look laid over `base`: what it says holds, and `base` fills in the rest.
    pub fn over(self, base: Look) -> Look {
        Look {
            color: self.color.or(base.color),
            highlight: self.highlight.or(base.highlight),
            fill: self.fill.or(base.fill),
            bold: self.bold.or(base.bold),
            italic: self.italic.or(base.italic),
            underline: self.underline.or(base.underline),
            strike: self.strike.or(base.strike),
            align: self.align.or(base.align),
            size: self.size.or(base.size),
        }
    }

    pub fn is_plain(&self) -> bool {
        *self == Look::default()
    }

    /// The look that declarations such as `color: red; font-weight: bold` ask for.
    /// A background belongs to the words of an inline element and to the whole of a
    /// `block` one.
    pub fn declared(declarations: &str, block: bool) -> Look {
        let mut look = Look::default();
        for declaration in declarations.split(';') {
            let Some((property, value)) = declaration.split_once(':') else {
                continue;
            };
            let value = value.replace("!important", "");
            let value = value.trim();
            let lower = value.to_ascii_lowercase();
            match property.trim().to_ascii_lowercase().as_str() {
                "color" => look.color = color(value).or(look.color),
                "background" | "background-color" => {
                    // The shorthand may name an image and a position beside the colour.
                    let found = color(value).or_else(|| value.split(' ').find_map(color));
                    if block {
                        look.fill = found.or(look.fill);
                    } else {
                        look.highlight = found.or(look.highlight);
                    }
                }
                "font-weight" => {
                    look.bold = match lower.as_str() {
                        "bold" | "bolder" => Some(true),
                        "normal" | "lighter" => Some(false),
                        number => number.parse::<u32>().ok().map(|w| w >= 600).or(look.bold),
                    }
                }
                "font-style" => {
                    look.italic = match lower.as_str() {
                        "italic" | "oblique" => Some(true),
                        "normal" => Some(false),
                        _ => look.italic,
                    }
                }
                "text-decoration" | "text-decoration-line" => {
                    if lower.contains("none") {
                        (look.underline, look.strike) = (Some(false), Some(false));
                    }
                    if lower.contains("underline") {
                        look.underline = Some(true);
                    }
                    if lower.contains("line-through") {
                        look.strike = Some(true);
                    }
                }
                "text-align" => {
                    look.align = match lower.as_str() {
                        "left" | "start" | "justify" => Some(Align::Left),
                        "center" => Some(Align::Center),
                        "right" | "end" => Some(Align::Right),
                        _ => look.align,
                    }
                }
                "font-size" => look.size = size(value).or(look.size),
                _ => {}
            }
        }
        look
    }

    /// The look written out to be carried in text: `OPEN`, what it says, `END`.
    pub fn marker(&self) -> String {
        let hex = |c: [u8; 3]| format!("{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        let flag = |on: bool| if on { "1" } else { "0" }.to_string();
        let fields = [
            ("c", self.color.map(hex)),
            ("h", self.highlight.map(hex)),
            ("f", self.fill.map(hex)),
            ("w", self.bold.map(flag)),
            ("i", self.italic.map(flag)),
            ("u", self.underline.map(flag)),
            ("s", self.strike.map(flag)),
            (
                "a",
                self.align.map(|a| {
                    match a {
                        Align::Left => "l",
                        Align::Center => "c",
                        Align::Right => "r",
                    }
                    .to_string()
                }),
            ),
            (
                "z",
                self.size.and_then(|s| match s {
                    Size::Points(points) => Some(format!("{points:.2}")),
                    Size::Times(_) => None,
                }),
            ),
            (
                "y",
                self.size.and_then(|s| match s {
                    Size::Times(times) => Some(format!("{times:.3}")),
                    Size::Points(_) => None,
                }),
            ),
        ];
        let said: Vec<String> = fields
            .into_iter()
            .filter_map(|(key, value)| Some(format!("{key}={}", value?)))
            .collect();
        format!("{OPEN}{}{END}", said.join(";"))
    }

    /// The look that the inside of a marker describes. What is not understood is
    /// passed over: text may hold these characters by chance.
    pub fn from_marker(payload: &str) -> Look {
        let mut look = Look::default();
        for field in payload.split(';') {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            let on = Some(value == "1");
            match key {
                "c" => look.color = color(&format!("#{value}")),
                "h" => look.highlight = color(&format!("#{value}")),
                "f" => look.fill = color(&format!("#{value}")),
                "w" => look.bold = on,
                "i" => look.italic = on,
                "u" => look.underline = on,
                "s" => look.strike = on,
                "a" => {
                    look.align = match value {
                        "l" => Some(Align::Left),
                        "c" => Some(Align::Center),
                        "r" => Some(Align::Right),
                        _ => None,
                    }
                }
                "z" => look.size = value.parse().ok().map(Size::Points),
                "y" => look.size = value.parse().ok().map(Size::Times),
                _ => {}
            }
        }
        look
    }
}

/// An element as selectors see it: its tag, its classes and its id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub tag: String,
    pub classes: Vec<String>,
    pub id: Option<String>,
}

/// What a selector asks of one element: its tag, a class, an id, or several of these.
#[derive(Debug, Default, PartialEq)]
struct Named {
    tag: Option<String>,
    class: Option<String>,
    id: Option<String>,
}

/// A selector: the element it picks, named last, after any it has to stand inside.
#[derive(Debug, Default, PartialEq)]
struct Selector(Vec<Named>);

impl Selector {
    /// A selector such as `p`, `.note`, `td.sum`, `#total` or `.total td`. One that
    /// goes by siblings, attributes or state is not read.
    fn parse(text: &str) -> Option<Selector> {
        let names: Vec<Named> = text
            .split_whitespace()
            .map(Named::parse)
            .collect::<Option<_>>()?;
        (!names.is_empty()).then_some(Selector(names))
    }

    /// Whether the selector picks `element`, which stands inside `around`, outermost first.
    fn picks(&self, element: &Element, around: &[Element]) -> bool {
        let Some((subject, outer)) = self.0.split_last() else {
            return false;
        };
        let mut around = around.iter();
        subject.names(element) && outer.iter().all(|named| around.any(|e| named.names(e)))
    }

    /// How narrowly the selector picks, as CSS ranks it: ids, then classes, then tags.
    fn weight(&self) -> (usize, usize, usize) {
        let count = |has: fn(&Named) -> bool| self.0.iter().filter(|n| has(n)).count();
        (
            count(|n| n.id.is_some()),
            count(|n| n.class.is_some()),
            count(|n| n.tag.is_some()),
        )
    }
}

impl Named {
    fn names(&self, element: &Element) -> bool {
        self.tag.as_deref().is_none_or(|t| t == element.tag)
            && self
                .class
                .as_deref()
                .is_none_or(|c| element.classes.iter().any(|own| own == c))
            && self
                .id
                .as_deref()
                .is_none_or(|i| Some(i) == element.id.as_deref())
    }

    fn parse(text: &str) -> Option<Named> {
        let simple =
            |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '#' | '*');
        if text.is_empty() || !text.chars().all(simple) {
            return None;
        }
        let mut selector = Named::default();
        let mut rest = text;
        while !rest.is_empty() {
            let kind = rest.chars().next().filter(|c| matches!(c, '.' | '#'));
            let body = &rest[kind.map_or(0, char::len_utf8)..];
            let end = body.find(['.', '#']).unwrap_or(body.len());
            let name = body[..end].to_string();
            match kind {
                Some('.') if selector.class.is_none() => selector.class = Some(name),
                Some('#') => selector.id = Some(name),
                None if name != "*" => selector.tag = Some(name.to_ascii_lowercase()),
                None => {}
                // Two classes at once: more than is read here.
                _ => return None,
            }
            rest = &body[end..];
        }
        Some(selector)
    }
}

/// The rules of the style sheets of a document, in the order they were written.
#[derive(Default)]
pub struct Sheet {
    rules: Vec<(Selector, String)>,
}

impl Sheet {
    /// Takes in the rules of a style sheet. Rules for other media and at-rules of any
    /// kind are left out, braces and all.
    pub fn add(&mut self, css: &str) {
        let mut rest = css;
        while let Some(open) = rest.find('{') {
            let selectors = rest[..open].rsplit(['}', ';']).next().unwrap_or("").trim();
            let body = &rest[open + 1..];
            if selectors.starts_with('@') {
                // Skip to the brace that closes this one.
                let mut depth = 1;
                let end = body
                    .char_indices()
                    .find(|(_, c)| {
                        depth += match c {
                            '{' => 1,
                            '}' => -1,
                            _ => 0,
                        };
                        depth == 0
                    })
                    .map_or(body.len(), |(at, _)| at + 1);
                rest = &body[end..];
                continue;
            }
            let end = body.find('}').unwrap_or(body.len());
            let declarations = &body[..end];
            for selector in selectors.split(',').filter_map(Selector::parse) {
                self.rules.push((selector, declarations.to_string()));
            }
            rest = body.get(end + 1..).unwrap_or("");
        }
    }

    /// The look the sheets give an element. Rules that pick more narrowly win, and
    /// among equals the later one.
    pub fn look(&self, element: &Element, around: &[Element], block: bool) -> Look {
        let mut matching: Vec<&(Selector, String)> = self
            .rules
            .iter()
            .filter(|(selector, _)| selector.picks(element, around))
            .collect();
        matching.sort_by_key(|(selector, _)| selector.weight());
        matching
            .iter()
            .fold(Look::default(), |look, (_, declarations)| {
                Look::declared(declarations, block).over(look)
            })
    }
}

/// Strips the comments of a style sheet.
pub fn without_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(at) = rest.find("/*") {
        out.push_str(&rest[..at]);
        rest = rest[at + 2..]
            .split_once("*/")
            .map_or("", |(_, after)| after);
    }
    out + rest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_sizes_are_read_in_the_ways_they_are_written() {
        assert_eq!(color("#f00"), Some([255, 0, 0]));
        assert_eq!(color("#1a2B3c"), Some([26, 43, 60]));
        assert_eq!(color("rgb(10, 20, 30)"), Some([10, 20, 30]));
        assert_eq!(color("rgb(100% 0% 50%)"), Some([255, 0, 128]));
        assert_eq!(color(" Navy "), Some([0, 0, 128]));
        assert_eq!(color("url(x.png)"), None);
        assert_eq!(size("16px"), Some(Size::Points(12.0)));
        assert_eq!(size("14pt"), Some(Size::Points(14.0)));
        assert_eq!(size("1.5em"), Some(Size::Times(1.5)));
        assert_eq!(size("80%"), Some(Size::Times(0.8)));
        assert_eq!(size("thick"), None);
    }

    #[test]
    fn declarations_make_a_look_and_a_marker_carries_it() {
        let look = Look::declared(
            "color: #c00; background: yellow url(x.png); font-weight: 700; \
             text-decoration: underline line-through; text-align: right; font-size: 20px !important",
            false,
        );
        assert_eq!(
            look,
            Look {
                color: Some([204, 0, 0]),
                highlight: Some([255, 255, 0]),
                bold: Some(true),
                underline: Some(true),
                strike: Some(true),
                align: Some(Align::Right),
                size: Some(Size::Points(15.0)),
                ..Default::default()
            }
        );
        let marker = look.marker();
        assert!(marker.starts_with(OPEN) && marker.ends_with(END));
        assert_eq!(Look::from_marker(marker.trim_matches([OPEN, END])), look);
        // The same background on a block fills it.
        assert_eq!(
            Look::declared("background-color: #eee", true).fill,
            Some([238, 238, 238])
        );
        // What is laid over something else keeps what it does not speak of.
        let inner = Look::declared("font-style: italic; font-weight: normal", false).over(look);
        assert_eq!(
            (inner.italic, inner.bold, inner.color),
            (Some(true), Some(false), Some([204, 0, 0]))
        );
    }

    #[test]
    fn a_sheet_gives_an_element_the_rules_that_name_it() {
        let mut sheet = Sheet::default();
        sheet.add(&without_comments(
            "/* totals */ td.sum, #grand { color: red; font-weight: bold }\n\
             @media print { p { color: blue } }\n\
             p { color: green }\n\
             .note { color: gray; font-style: italic }\n\
             p.note { color: navy }\n\
             .total td { background: #eee }\n\
             div > p, a:hover { color: pink }",
        ));
        let element = |tag: &str, classes: &[&str], id: Option<&str>| Element {
            tag: tag.to_string(),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            id: id.map(str::to_string),
        };
        let plain = sheet.look(&element("p", &[], None), &[], true);
        assert_eq!(plain.color, Some([0, 128, 0]));
        // The narrower rule wins whatever the order, and the wider one still adds to it.
        let note = sheet.look(&element("p", &["note", "wide"], None), &[], true);
        assert_eq!((note.color, note.italic), (Some([0, 0, 128]), Some(true)));
        assert_eq!(
            sheet.look(&element("td", &["sum"], None), &[], true).bold,
            Some(true)
        );
        assert_eq!(
            sheet
                .look(&element("span", &[], Some("grand")), &[], false)
                .color,
            Some([255, 0, 0])
        );
        assert!(sheet.look(&element("td", &[], None), &[], true).is_plain());
        // An element is picked by what it stands in, however deep.
        let table = [element("table", &[], None), element("tr", &["total"], None)];
        assert_eq!(
            sheet.look(&element("td", &[], None), &table, true).fill,
            Some([238, 238, 238])
        );
        assert!(
            sheet
                .look(&element("td", &[], None), &table[..1], true)
                .is_plain()
        );
    }
}
