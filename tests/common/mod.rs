//! PDF fixtures built in memory, so no binary files live in the repository.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use serde_json::{Value, json};

pub fn call(tool: &str, args: Value) -> Value {
    pdfops::tools::call(tool, args).unwrap_or_else(|e| panic!("{tool} failed: {e:#}"))
}

pub fn call_err(tool: &str, args: Value) -> String {
    format!(
        "{:#}",
        pdfops::tools::call(tool, args).expect_err("expected an error")
    )
}

pub fn page_count(path: &Path) -> u64 {
    call("pdf_info", json!({"input": path}))["pages"]
        .as_u64()
        .unwrap()
}

/// Text of every page, in order.
pub fn texts(path: &Path) -> Vec<String> {
    let v = call("pdf_text", json!({"input": path}));
    v["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["text"].as_str().unwrap().to_string())
        .collect()
}

fn skeleton(doc: &mut Document, pages_id: ObjectId, kids: Vec<Object>, extra: Dictionary) {
    let font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding",
    });
    // MediaBox and Resources sit on the tree node, so pages inherit them.
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Count" => kids.len() as i64,
            "Kids" => kids,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        }),
    );
    let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => pages_id };
    catalog.extend(&extra);
    let catalog = doc.add_object(catalog);
    doc.trailer.set("Root", catalog);
    let info = doc.add_object(dictionary! { "Title" => Object::string_literal("Sample"), "Author" => Object::string_literal("Ada") });
    doc.trailer.set("Info", info);
}

fn text_page(doc: &mut Document, pages_id: ObjectId, text: &str) -> ObjectId {
    let content = format!("BT /F1 18 Tf 72 720 Td ({text}) Tj ET");
    let content = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
    doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => content })
}

/// `pages` pages reading "Page N of the sample. Keyword alpha-N.", with an outline entry for page 2.
pub fn sample(dir: &Path, name: &str, pages: u32) -> PathBuf {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let ids: Vec<ObjectId> = (1..=pages)
        .map(|n| {
            text_page(
                &mut doc,
                pages_id,
                &format!("Page {n} of the sample. Keyword alpha-{n}."),
            )
        })
        .collect();
    let mut extra = Dictionary::new();
    if pages >= 2 {
        let outlines = doc.new_object_id();
        let item = doc.add_object(dictionary! {
            "Title" => Object::string_literal("Second chapter"),
            "Parent" => outlines,
            "Dest" => vec![ids[1].into(), "Fit".into()],
        });
        doc.objects.insert(
            outlines,
            Object::Dictionary(
                dictionary! { "Type" => "Outlines", "First" => item, "Last" => item, "Count" => 1 },
            ),
        );
        extra.set("Outlines", outlines);
    }
    skeleton(
        &mut doc,
        pages_id,
        ids.into_iter().map(Object::from).collect(),
        extra,
    );
    let path = dir.join(name);
    doc.save(&path).unwrap();
    path
}

/// One page with a text field `name`, a checkbox `agree`, a choice `color`, a multiline
/// text field `notes` and a 2x2 RGB image.
pub fn form(dir: &Path) -> PathBuf {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let page = text_page(&mut doc, pages_id, "Registration form");

    let image = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 2, "Height" => 2,
            "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
        },
        vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
    ));
    let font = doc.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let on = doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 12.into(), 12.into()] }, b"0 0 12 12 re f".to_vec()));
    let off = doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 12.into(), 12.into()] }, Vec::new()));

    let widget = |ft: &str, name: &str, rect: [i64; 4]| {
        dictionary! {
            "Type" => "Annot", "Subtype" => "Widget", "FT" => ft, "T" => Object::string_literal(name), "P" => page,
            "Rect" => rect.iter().map(|&v| v.into()).collect::<Vec<Object>>(),
        }
    };
    let name = doc.add_object(widget("Tx", "name", [72, 600, 300, 620]));
    let mut agree = widget("Btn", "agree", [72, 560, 84, 572]);
    agree.set("V", "Off");
    agree.set("AS", "Off");
    agree.set(
        "AP",
        dictionary! { "N" => dictionary! { "Yes" => on, "Off" => off } },
    );
    let agree = doc.add_object(agree);
    let mut color = widget("Ch", "color", [72, 520, 300, 540]);
    color.set(
        "Opt",
        vec![
            Object::string_literal("red"),
            Object::string_literal("green"),
        ],
    );
    let color = doc.add_object(color);

    let mut notes = widget("Tx", "notes", [72, 400, 300, 500]);
    notes.set("Ff", 4096);
    let notes = doc.add_object(notes);

    let fields: Vec<Object> = vec![name.into(), agree.into(), color.into(), notes.into()];
    doc.get_dictionary_mut(page)
        .unwrap()
        .set("Annots", fields.clone());
    doc.get_dictionary_mut(page).unwrap().set(
        "Resources",
        dictionary! { "Font" => dictionary! { "F1" => font }, "XObject" => dictionary! { "Im1" => image } },
    );
    let mut extra = Dictionary::new();
    extra.set(
        "AcroForm",
        dictionary! {
            "Fields" => fields,
            "DA" => Object::string_literal("/Helv 10 Tf 0 g"),
            "DR" => dictionary! { "Font" => dictionary! { "Helv" => font } },
        },
    );
    skeleton(&mut doc, pages_id, vec![page.into()], extra);
    let path = dir.join("form.pdf");
    doc.save(&path).unwrap();
    path
}
