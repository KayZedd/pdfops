//! AcroForm commands: forms (list fields) and fill.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use clap::Args;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::doc;
use crate::ops::edit::{helvetica_width, literal, winansi};

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct FormsArgs {
    /// PDF file to inspect
    pub input: PathBuf,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct FillArgs {
    /// PDF file with form fields
    pub input: PathBuf,
    /// Where to write the filled PDF (may be the input file)
    #[arg(short, long)]
    pub output: PathBuf,
    /// Field assignment, repeatable. Checkboxes take true/false, radios and choices an option name
    #[arg(long = "set", value_name = "NAME=VALUE")]
    #[serde(skip)]
    #[schemars(skip)]
    pub set: Vec<String>,
    /// Field name to new value. Checkboxes take "true"/"false", radios and choices an option name
    #[arg(skip)]
    #[serde(default)]
    pub values: BTreeMap<String, String>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

const FLAG_READ_ONLY: i64 = 1;
const FLAG_MULTILINE: i64 = 1 << 12;
const FLAG_PASSWORD: i64 = 1 << 13;
const FLAG_RADIO: i64 = 1 << 15;
const FLAG_PUSHBUTTON: i64 = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Text,
    Checkbox,
    Radio,
    Button,
    Choice,
    Signature,
    Unknown,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Checkbox => "checkbox",
            Kind::Radio => "radio",
            Kind::Button => "button",
            Kind::Choice => "choice",
            Kind::Signature => "signature",
            Kind::Unknown => "unknown",
        }
    }
}

/// A terminal form field with its widget annotations.
pub struct Field {
    pub id: ObjectId,
    pub name: String,
    pub kind: Kind,
    pub flags: i64,
    pub value: Option<String>,
    pub options: Vec<String>,
    pub widgets: Vec<ObjectId>,
    /// Default appearance string, e.g. `/Helv 10 Tf 0 g`.
    pub appearance: Option<String>,
}

/// Field attributes that children inherit from their ancestors.
#[derive(Clone, Default)]
struct Inherited {
    name: String,
    kind: Option<Vec<u8>>,
    flags: Option<i64>,
    value: Option<String>,
    appearance: Option<String>,
}

fn acroform(d: &Document) -> Option<&Dictionary> {
    doc::resolve(d, d.catalog().ok()?.get(b"AcroForm").ok()?)
        .as_dict()
        .ok()
}

/// States a button widget can be switched on to, i.e. its appearance names except `Off`.
fn on_states(d: &Document, widget: ObjectId) -> Vec<String> {
    let normal = d
        .get_dictionary(widget)
        .ok()
        .and_then(|w| doc::resolve(d, w.get(b"AP").ok()?).as_dict().ok())
        .and_then(|ap| doc::resolve(d, ap.get(b"N").ok()?).as_dict().ok());
    normal
        .map(|n| {
            n.iter()
                .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                .filter(|k| k != "Off")
                .collect()
        })
        .unwrap_or_default()
}

fn walk(
    d: &Document,
    id: ObjectId,
    mut inh: Inherited,
    seen: &mut HashSet<ObjectId>,
    out: &mut Vec<Field>,
) {
    // `seen` guards against cyclic /Kids.
    let Ok(dict) = d.get_dictionary(id) else {
        return;
    };
    if !seen.insert(id) {
        return;
    }
    let get = |key: &[u8]| dict.get(key).ok().map(|o| doc::resolve(d, o));
    if let Some(part) = get(b"T").and_then(doc::text) {
        inh.name = if inh.name.is_empty() {
            part
        } else {
            format!("{}.{part}", inh.name)
        };
    }
    if let Some(kind) = get(b"FT").and_then(|o| o.as_name().ok()) {
        inh.kind = Some(kind.to_vec());
    }
    if let Some(flags) = get(b"Ff").and_then(|o| o.as_i64().ok()) {
        inh.flags = Some(flags);
    }
    if let Some(value) = get(b"V") {
        inh.value = doc::text(value);
    }
    if let Some(da) = get(b"DA").and_then(doc::text) {
        inh.appearance = Some(da);
    }

    let kids: Vec<ObjectId> = get(b"Kids")
        .and_then(|k| k.as_array().ok())
        .map(|k| k.iter().filter_map(|o| o.as_reference().ok()).collect())
        .unwrap_or_default();
    // Kids with a name are sub-fields; kids without one are this field's widgets.
    let named = |kid: &ObjectId| d.get_dictionary(*kid).is_ok_and(|k| k.has(b"T"));
    if kids.iter().any(named) {
        for kid in kids {
            walk(d, kid, inh.clone(), seen, out);
        }
        return;
    }

    let flags = inh.flags.unwrap_or(0);
    let kind = match inh.kind.as_deref() {
        Some(b"Tx") => Kind::Text,
        Some(b"Ch") => Kind::Choice,
        Some(b"Sig") => Kind::Signature,
        Some(b"Btn") if flags & FLAG_PUSHBUTTON != 0 => Kind::Button,
        Some(b"Btn") if flags & FLAG_RADIO != 0 => Kind::Radio,
        Some(b"Btn") => Kind::Checkbox,
        _ => Kind::Unknown,
    };
    let widgets = if kids.is_empty() { vec![id] } else { kids };
    let mut options = Vec::new();
    match kind {
        Kind::Choice => {
            for opt in get(b"Opt")
                .and_then(|o| o.as_array().ok())
                .into_iter()
                .flatten()
            {
                // An option is either a string or an [export, display] pair.
                let opt = doc::resolve(d, opt);
                let export = opt
                    .as_array()
                    .ok()
                    .and_then(|pair| pair.first())
                    .unwrap_or(opt);
                options.extend(doc::text(doc::resolve(d, export)));
            }
        }
        Kind::Checkbox | Kind::Radio => {
            for &w in &widgets {
                for state in on_states(d, w) {
                    if !options.contains(&state) {
                        options.push(state);
                    }
                }
            }
        }
        _ => {}
    }
    out.push(Field {
        id,
        name: inh.name,
        kind,
        flags,
        value: inh.value,
        options,
        widgets,
        appearance: inh.appearance,
    });
}

/// All terminal fields of the document's interactive form.
pub fn collect(d: &Document) -> Vec<Field> {
    let mut out = Vec::new();
    let Some(form) = acroform(d) else { return out };
    let root = Inherited {
        appearance: form.get(b"DA").ok().and_then(doc::text),
        ..Default::default()
    };
    let mut seen = HashSet::new();
    for field in form
        .get(b"Fields")
        .ok()
        .and_then(|f| doc::resolve(d, f).as_array().ok())
        .into_iter()
        .flatten()
    {
        if let Ok(id) = field.as_reference() {
            walk(d, id, root.clone(), &mut seen, &mut out);
        }
    }
    out
}

pub fn forms(a: FormsArgs) -> Result<Value> {
    let d = doc::load(&a.input, a.password.as_deref())?;
    let mut widget_page = HashMap::new();
    for (n, id) in d.get_pages() {
        let annots = d
            .get_dictionary(id)
            .ok()
            .and_then(|p| doc::resolve(&d, p.get(b"Annots").ok()?).as_array().ok());
        for annot in annots
            .into_iter()
            .flatten()
            .filter_map(|o| o.as_reference().ok())
        {
            widget_page.insert(annot, n);
        }
    }
    let fields: Vec<Value> = collect(&d)
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "type": f.kind.name(),
                "value": f.value,
                "options": f.options,
                "read_only": f.flags & FLAG_READ_ONLY != 0,
                "page": f.widgets.iter().find_map(|w| widget_page.get(w)),
            })
        })
        .collect();
    Ok(json!({"file": a.input, "fields": fields}))
}

fn truthy(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "x" | "checked" => Some(true),
        "false" | "no" | "off" | "0" | "" | "unchecked" => Some(false),
        _ => None,
    }
}

/// Switches a checkbox or radio group to `state`, updating every widget's visible state.
fn set_button(d: &mut Document, field: &Field, state: &str) -> Result<()> {
    d.get_dictionary_mut(field.id)?
        .set("V", Object::Name(state.as_bytes().to_vec()));
    for &w in &field.widgets {
        let shown = if on_states(d, w).iter().any(|s| s == state) {
            state
        } else {
            "Off"
        };
        d.get_dictionary_mut(w)?
            .set("AS", Object::Name(shown.as_bytes().to_vec()));
    }
    Ok(())
}

/// Breaks `value` into lines no wider than `width` points.
///
/// Widths are Helvetica's, which is what form fields nearly always use; with
/// another font the breaks are approximate and the clip keeps text in the field.
fn wrap(value: &str, width: f64, size: f64) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in value.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            let fits = winansi(&candidate).is_ok_and(|b| helvetica_width(&b, size) <= width);
            if fits || line.is_empty() {
                line = candidate;
            } else {
                lines.push(std::mem::replace(&mut line, word.to_string()));
            }
        }
        lines.push(line);
    }
    lines
}

/// Draws `value` into a text widget, so viewers that do not rebuild appearances
/// (and our own renderer) still show it.
fn text_appearance(
    d: &mut Document,
    field: &Field,
    widget: ObjectId,
    value: &str,
    fallback_font: ObjectId,
) -> Result<bool> {
    let multiline = field.flags & FLAG_MULTILINE != 0;
    let rect: Vec<f64> = d
        .get_dictionary(widget)?
        .get(b"Rect")
        .ok()
        .and_then(|r| doc::resolve(d, r).as_array().ok())
        .map(|r| r.iter().filter_map(|o| doc::number(d, o)).collect())
        .unwrap_or_default();
    let [x0, y0, x1, y1] = rect[..] else {
        return Ok(false);
    };
    let (w, h) = ((x1 - x0).abs(), (y1 - y0).abs());

    // Default appearance: "/Font size Tf" plus colour operators we keep as they are.
    let da = field.appearance.as_deref().unwrap_or("/Helv 0 Tf 0 g");
    let tokens: Vec<&str> = da.split_whitespace().collect();
    let tf = tokens.iter().position(|t| *t == "Tf").filter(|&i| i >= 2);
    let (font, size, color) = match tf {
        Some(i) => (
            tokens[i - 2].trim_start_matches('/').to_string(),
            tokens[i - 1].parse::<f64>().unwrap_or(0.0),
            [&tokens[..i - 2], &tokens[i + 1..]].concat().join(" "),
        ),
        None => ("Helv".to_string(), 0.0, "0 g".to_string()),
    };
    // Size 0 means "fit to the field".
    let size = if size > 0.0 {
        size
    } else {
        (h - 4.0).clamp(4.0, 12.0)
    };

    let mut fonts = acroform(d)
        .and_then(|f| doc::resolve(d, f.get(b"DR").ok()?).as_dict().ok())
        .and_then(|dr| doc::resolve(d, dr.get(b"Font").ok()?).as_dict().ok())
        .cloned()
        .unwrap_or_default();
    if !fonts.has(font.as_bytes()) {
        fonts.set(font.as_str(), fallback_font);
    }
    let mut resources = Dictionary::new();
    resources.set("Font", fonts);

    // A field too short for a second line is laid out as a single centred line.
    let leading = size * 1.15;
    let (lines, baseline) = if multiline && h >= 2.0 * leading + 4.0 {
        (wrap(value, w - 4.0, size), h - 2.0 - size * 0.9)
    } else {
        (
            vec![value.split_whitespace().collect::<Vec<_>>().join(" ")],
            (h - size) / 2.0 + size * 0.22,
        )
    };
    let mut shown = Vec::with_capacity(lines.len());
    for line in &lines {
        let Ok(bytes) = winansi(line) else {
            return Ok(false);
        };
        shown.push(format!("{} Tj", literal(&bytes)));
    }
    let content = format!(
        "/Tx BMC\nq\n1 1 {:.2} {:.2} re W n\nBT\n/{font} {size:.2} Tf\n{color}\n{leading:.2} TL\n2 {baseline:.2} Td\n{}\nET\nQ\nEMC\n",
        (w - 2.0).max(0.0),
        (h - 2.0).max(0.0),
        shown.join("\nT*\n")
    );
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Form".to_vec()));
    dict.set(
        "BBox",
        vec![
            0.into(),
            0.into(),
            Object::Real(w as f32),
            Object::Real(h as f32),
        ],
    );
    dict.set("Resources", resources);
    let stream = d.add_object(Stream::new(dict, content.into_bytes()));
    let mut ap = Dictionary::new();
    ap.set("N", stream);
    d.get_dictionary_mut(widget)?.set("AP", ap);
    Ok(true)
}

pub fn fill(a: FillArgs) -> Result<Value> {
    let mut values = a.values.clone();
    for pair in &a.set {
        let (name, value) = pair
            .split_once('=')
            .ok_or_else(|| anyhow!("expected NAME=VALUE, got '{pair}'"))?;
        values.insert(name.to_string(), value.to_string());
    }
    if values.is_empty() {
        bail!("no field values given");
    }
    let mut d = doc::load(&a.input, a.password.as_deref())?;
    let fields = collect(&d);
    if fields.is_empty() {
        bail!("{} has no form fields", a.input.display());
    }

    // Resolve every assignment before touching the document, so a bad name changes nothing.
    let mut plan = Vec::new();
    for (name, value) in &values {
        let field = fields.iter().find(|f| &f.name == name).ok_or_else(|| {
            let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
            anyhow!("no field named '{name}'; available: {}", names.join(", "))
        })?;
        let one_of = |value: &str| {
            field
                .options
                .iter()
                .find(|o| *o == value)
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "'{value}' is not an option of '{name}'; options: {}",
                        field.options.join(", ")
                    )
                })
        };
        let resolved = match field.kind {
            Kind::Text => value.clone(),
            Kind::Choice if field.options.is_empty() => value.clone(),
            Kind::Choice => one_of(value)?,
            Kind::Checkbox => match (truthy(value), one_of(value)) {
                (_, Ok(state)) => state,
                (Some(true), _) => field
                    .options
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "Yes".to_string()),
                (Some(false), _) => "Off".to_string(),
                (None, Err(_)) => bail!("checkbox '{name}' takes true or false, got '{value}'"),
            },
            Kind::Radio if truthy(value) == Some(false) => "Off".to_string(),
            Kind::Radio => one_of(value)?,
            Kind::Button | Kind::Signature | Kind::Unknown => {
                bail!(
                    "field '{name}' is a {} and cannot be filled",
                    field.kind.name()
                )
            }
        };
        plan.push((field, resolved));
    }

    let mut helvetica = Dictionary::new();
    helvetica.set("Type", Object::Name(b"Font".to_vec()));
    helvetica.set("Subtype", Object::Name(b"Type1".to_vec()));
    helvetica.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
    helvetica.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    let helvetica = d.add_object(helvetica);

    let mut filled = Vec::new();
    for (field, value) in plan {
        match field.kind {
            Kind::Checkbox | Kind::Radio => set_button(&mut d, field, &value)?,
            _ => {
                d.get_dictionary_mut(field.id)?
                    .set("V", lopdf::text_string(&value));
                // Password fields must not show their value.
                let visible = field.kind == Kind::Text && field.flags & FLAG_PASSWORD == 0;
                for &w in &field.widgets {
                    if !(visible && text_appearance(&mut d, field, w, &value, helvetica)?) {
                        // A stale appearance would keep showing the old value.
                        d.get_dictionary_mut(w)?.remove(b"AP");
                    }
                }
            }
        }
        filled.push(json!({"name": field.name, "value": value}));
    }
    let catalog = doc::catalog_id(&d)?;
    let form = doc::ensure_indirect_dict(&mut d, Some(catalog), b"AcroForm")?;
    d.get_dictionary_mut(form)?.set("NeedAppearances", true);
    let size = doc::save(&mut d, &a.output)?;
    Ok(json!({"output": a.output, "filled": filled, "size_bytes": size}))
}
