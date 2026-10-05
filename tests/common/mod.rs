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
    content_page(
        doc,
        pages_id,
        &format!("BT /F1 18 Tf 72 720 Td ({text}) Tj ET"),
    )
}

fn content_page(doc: &mut Document, pages_id: ObjectId, content: &str) -> ObjectId {
    let content = doc.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
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
    let page = content_page(
        &mut doc,
        pages_id,
        "BT /F1 18 Tf 72 720 Td (Registration form) Tj ET q 20 0 0 20 400 700 cm /Im1 Do Q",
    );

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

/// One page drawing the given image streams side by side, each 100 points wide.
pub fn with_images(dir: &Path, name: &str, images: Vec<Stream>) -> PathBuf {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut xobjects = Dictionary::new();
    let mut content = String::new();
    for (i, image) in images.into_iter().enumerate() {
        let id = doc.add_object(image);
        xobjects.set(format!("Im{i}"), id);
        content += &format!("q 100 0 0 100 {} 600 cm /Im{i} Do Q\n", 50 + i * 120);
    }
    let page = content_page(&mut doc, pages_id, &content);
    doc.get_dictionary_mut(page)
        .unwrap()
        .set("Resources", dictionary! { "XObject" => xobjects });
    skeleton(&mut doc, pages_id, vec![page.into()], Dictionary::new());
    let path = dir.join(name);
    doc.save(&path).unwrap();
    path
}

pub fn image_stream(
    width: i64,
    height: i64,
    color_space: Object,
    bits: i64,
    data: Vec<u8>,
) -> Stream {
    Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => width, "Height" => height,
            "ColorSpace" => color_space, "BitsPerComponent" => bits,
        },
        data,
    )
}

/// A page that shows page 1 of `source` as a picture only, like a scanner would produce.
pub fn scan(dir: &Path, source: &Path) -> PathBuf {
    let v = call(
        "pdf_render",
        json!({"input": source, "out_dir": dir.join("scan"), "pages": "1", "dpi": 150}),
    );
    let bytes = std::fs::read(v["files"][0]["file"].as_str().unwrap()).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let channels = buf.len() / (info.width * info.height) as usize;
    let gray: Vec<u8> = buf.chunks(channels).map(|p| p[0]).collect();

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let image = doc.add_object(image_stream(
        info.width as i64,
        info.height as i64,
        "DeviceGray".into(),
        8,
        gray,
    ));
    let page = content_page(&mut doc, pages_id, "q 612 0 0 792 0 0 cm /Im1 Do Q");
    doc.get_dictionary_mut(page).unwrap().set(
        "Resources",
        dictionary! { "XObject" => dictionary! { "Im1" => image } },
    );
    skeleton(&mut doc, pages_id, vec![page.into()], Dictionary::new());
    let path = dir.join("scan.pdf");
    doc.save(&path).unwrap();
    path
}

/// A tesseract language to test with, if the engine and any language data are installed.
pub fn ocr_lang() -> Option<String> {
    let out = std::process::Command::new("tesseract")
        .arg("--list-langs")
        .output()
        .ok()?;
    let text = String::from_utf8([out.stdout, out.stderr].concat()).ok()?;
    let langs: Vec<&str> = text
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "osd")
        .collect();
    langs
        .iter()
        .find(|l| **l == "eng")
        .or(langs.first())
        .map(|l| l.to_string())
}

/// Serves fixed responses over HTTP on a local port and returns the base URL.
pub fn serve(routes: Vec<(&'static str, Vec<u8>)>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                request.push(byte[0]);
            }
            let path = String::from_utf8_lossy(&request)
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_string();
            let reply = match routes.iter().find(|r| r.0 == path) {
                Some((_, body)) => {
                    let mut out = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    out.extend_from_slice(body);
                    out
                }
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec(),
            };
            let _ = stream.write_all(&reply);
        }
    });
    base
}

/// Dark pixel counts inside page rectangles given in PDF points, at 72 dpi.
pub fn ink<const N: usize>(
    pdf: &std::path::Path,
    dir: &std::path::Path,
    rects: [[usize; 4]; N],
) -> [usize; N] {
    let v = call(
        "pdf_render",
        json!({"input": pdf, "out_dir": dir, "dpi": 72}),
    );
    let bytes = std::fs::read(v["files"][0]["file"].as_str().unwrap()).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let (width, height) = (info.width as usize, info.height as usize);
    let channels = buf.len() / (width * height);
    rects.map(|[x0, y0, x1, y1]| {
        (height - y1..height - y0)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(y * width + x) * channels] < 128)
            .count()
    })
}

/// A document whose pages have exactly the given content streams, with Helvetica as /F1.
pub fn custom(dir: &Path, name: &str, contents: &[&str]) -> PathBuf {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let ids: Vec<Object> = contents
        .iter()
        .map(|c| content_page(&mut doc, pages_id, c).into())
        .collect();
    skeleton(&mut doc, pages_id, ids, Dictionary::new());
    let path = dir.join(name);
    doc.save(&path).unwrap();
    path
}

/// The decoded content stream of a page, for checking what is really in the file.
pub fn content_of(path: &Path, page: u32) -> String {
    let doc = Document::load(path).unwrap();
    let id = doc.get_pages()[&page];
    String::from_utf8_lossy(&doc.get_page_content(id).unwrap()).into_owned()
}

/// Words of a page with their boxes, as `layout` reports them.
pub fn words(path: &Path, page: u32) -> Vec<(String, [f64; 4])> {
    let v = call(
        "pdf_layout",
        json!({"input": path, "pages": page.to_string(), "level": "words"}),
    );
    v["pages"][0]["words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            let b: Vec<f64> = w["bbox"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap())
                .collect();
            (
                w["text"].as_str().unwrap().to_string(),
                [b[0], b[1], b[2], b[3]],
            )
        })
        .collect()
}
