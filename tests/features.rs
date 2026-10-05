mod common;

use common::{
    call, call_err, content_of, custom, form, image_stream, ink, sample, texts, with_images, words,
};
use serde_json::json;

/// A 3x3 grid of ruled cells: one text per cell, the top-left cell holding two lines.
const GRID: &str = "\
0.5 w
72 500 m 372 500 l S 72 540 m 372 540 l S 72 580 m 372 580 l S 72 640 m 372 640 l S
72 500 m 72 640 l S 172 500 m 172 640 l S 272 500 m 272 640 l S 372 500 m 372 640 l S
BT /F1 10 Tf
1 0 0 1 78 622 Tm (Part) Tj 1 0 0 1 78 608 Tm (name) Tj
1 0 0 1 178 615 Tm (Width) Tj 1 0 0 1 278 615 Tm (Use) Tj
1 0 0 1 78 555 Tm (OMEGA) Tj 1 0 0 1 178 555 Tm (1250 mm) Tj 1 0 0 1 278 555 Tm (cut, to size) Tj
1 0 0 1 78 515 Tm (Angle 2) Tj 1 0 0 1 178 515 Tm (170 mm) Tj 1 0 0 1 278 515 Tm (strip) Tj
ET";

#[test]
fn layout_reports_boxes_fonts_and_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let v = call("pdf_layout", json!({"input": pdf, "pages": "2"}));
    let page = &v["pages"][0];
    assert_eq!(
        (
            page["page"].as_u64(),
            page["width"].as_f64(),
            page["height"].as_f64()
        ),
        (Some(2), Some(612.0), Some(792.0))
    );
    let line = &page["lines"][0];
    assert_eq!(line["text"], "Page 2 of the sample. Keyword alpha-2.");
    assert_eq!(line["size"], 18.0);
    // The text is drawn at (72, 720) from the bottom-left: baseline 72 points from the top.
    let b: Vec<f64> = line["bbox"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert!(
        (b[0] - 72.0).abs() < 0.1
            && b[1] < 72.0
            && b[3] > 72.0
            && (b[3] - b[1] - 18.0).abs() < 0.01,
        "{b:?}"
    );

    let w = words(&pdf, 2);
    assert_eq!(
        w.iter().map(|w| w.0.as_str()).collect::<Vec<_>>(),
        ["Page", "2", "of", "the", "sample.", "Keyword", "alpha-2."]
    );
    // "Page" in 18 point Helvetica is 667 + 556 + 556 + 556 thousandths wide.
    assert!((w[0].1[2] - w[0].1[0] - 42.03).abs() < 0.1, "{:?}", w[0]);
    assert!(w.windows(2).all(|p| p[0].1[2] < p[1].1[0]));
}

#[test]
fn tables_follow_ruling_lines() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = custom(dir.path(), "t.pdf", &[GRID]);
    let v = call("pdf_tables", json!({"input": pdf}));
    assert_eq!(v["tables"].as_array().unwrap().len(), 1, "{v}");
    let t = &v["tables"][0];
    assert_eq!(
        (
            t["rows"].as_u64(),
            t["columns"].as_u64(),
            t["detected_by"].as_str()
        ),
        (Some(3), Some(3), Some("lines"))
    );
    assert_eq!(
        t["cells"],
        json!([
            ["Part name", "Width", "Use"],
            ["OMEGA", "1250 mm", "cut, to size"],
            ["Angle 2", "170 mm", "strip"]
        ])
    );
    assert_eq!(t["bbox"], json!([72.0, 152.0, 372.0, 292.0]));

    let v = call("pdf_tables", json!({"input": pdf, "format": "markdown"}));
    assert_eq!(
        v["tables"][0]["markdown"],
        "| Part name | Width | Use |\n| --- | --- | --- |\n| OMEGA | 1250 mm | cut, to size |\n| Angle 2 | 170 mm | strip |"
    );
    let v = call("pdf_tables", json!({"input": pdf, "format": "csv"}));
    assert_eq!(
        v["tables"][0]["csv"],
        "Part name,Width,Use\nOMEGA,1250 mm,\"cut, to size\"\nAngle 2,170 mm,strip"
    );
}

#[test]
fn tables_are_found_from_aligned_columns_and_plain_text_is_not_a_table() {
    let dir = tempfile::tempdir().unwrap();
    let columns = "BT /F1 10 Tf 1 0 0 1 72 700 Tm (Item) Tj 1 0 0 1 250 700 Tm (Qty) Tj \
        1 0 0 1 72 686 Tm (Bolt M8) Tj 1 0 0 1 250 686 Tm (40) Tj \
        1 0 0 1 72 672 Tm (Washer) Tj 1 0 0 1 250 672 Tm (80) Tj ET";
    let v = call(
        "pdf_tables",
        json!({"input": custom(dir.path(), "c.pdf", &[columns])}),
    );
    assert_eq!(v["tables"][0]["detected_by"], "alignment");
    assert_eq!(
        v["tables"][0]["cells"],
        json!([["Item", "Qty"], ["Bolt M8", "40"], ["Washer", "80"]])
    );

    let v = call(
        "pdf_tables",
        json!({"input": sample(dir.path(), "a.pdf", 2)}),
    );
    assert_eq!(v["tables"], json!([]));
}

fn png_with_alpha(path: &std::path::Path) {
    // 4x4: opaque dark blue on the left half, fully transparent on the right.
    let pixels: Vec<u8> = (0..16)
        .flat_map(|i| {
            if i % 4 < 2 {
                [20, 0, 90, 255]
            } else {
                [0, 0, 0, 0]
            }
        })
        .collect();
    image::RgbaImage::from_raw(4, 4, pixels)
        .unwrap()
        .save(path)
        .unwrap();
}

#[test]
fn stamp_places_images_with_transparency() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let png = dir.path().join("mark.png");
    png_with_alpha(&png);
    let out = dir.path().join("out.pdf");
    // A 100x100 point box whose top-left corner is 300 points from the left and 400 from the top.
    let v = call(
        "pdf_stamp",
        json!({"input": pdf, "output": out, "image": png, "x": 300, "y": 400, "width": 100, "pages": "1"}),
    );
    assert_eq!(v["stamped_pages"], json!([1]));
    let [left, right, elsewhere] = ink(
        &out,
        dir.path(),
        [
            [300, 292, 350, 392],
            [350, 292, 400, 392],
            [300, 100, 400, 200],
        ],
    );
    // The opaque half paints; the transparent half lets the white page through.
    assert!(
        left > 4000 && right == 0 && elsewhere == 0,
        "{left} {right} {elsewhere}"
    );
    assert!(texts(&out)[0].contains("alpha-1"));

    let img = call(
        "pdf_images",
        json!({"input": out, "out_dir": dir.path().join("img")}),
    );
    assert_eq!(
        (
            img["images"][0]["width"].as_u64(),
            img["images"][0]["height"].as_u64()
        ),
        (Some(4), Some(4))
    );

    // By anchor, and a JPEG is embedded as it is.
    let jpg = dir.path().join("photo.jpg");
    image::RgbImage::from_pixel(8, 8, image::Rgb([10, 10, 10]))
        .save(&jpg)
        .unwrap();
    call(
        "pdf_stamp",
        json!({"input": pdf, "output": out, "image": jpg, "anchor": "top-left", "width": 50, "margin": 10}),
    );
    assert!(ink(&out, dir.path(), [[10, 732, 60, 782]])[0] > 2400);
    let img = call(
        "pdf_images",
        json!({"input": out, "out_dir": dir.path().join("img2")}),
    );
    let exported = std::fs::read(img["images"][0]["file"].as_str().unwrap()).unwrap();
    assert_eq!(exported, std::fs::read(&jpg).unwrap());

    let e = call_err(
        "pdf_stamp",
        json!({"input": pdf, "output": out, "image": pdf}),
    );
    assert!(e.contains("not a PNG or JPEG"), "{e}");
    let e = call_err(
        "pdf_stamp",
        json!({"input": pdf, "output": out, "image": png, "text": "x"}),
    );
    assert!(e.contains("exactly one"), "{e}");
    assert!(
        call_err(
            "pdf_stamp",
            json!({"input": pdf, "output": out, "image": png, "x": 5})
        )
        .contains("both x and y")
    );
}

#[test]
fn stamp_draws_qr_codes() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    call(
        "pdf_stamp",
        json!({"input": sample(dir.path(), "a.pdf", 1), "output": out, "qr": "https://example.com/doc/42", "x": 100, "y": 100, "width": 116}),
    );
    let content = content_of(&out, 1);
    // A white square of the requested size, 100 points from the left and from the top.
    assert!(
        content.contains("100.00 576.00 116.00 116.00 re f"),
        "{content}"
    );
    // Inside it, four quiet modules away from the corner, the top-left finder pattern
    // begins with a bar seven modules long.
    let modules = qrcode::QrCode::new("https://example.com/doc/42")
        .unwrap()
        .width();
    let module = 116.0 / (modules + 8) as f64;
    let finder = format!(
        "{:.3} {:.3} {:.3} {:.3} re",
        100.0 + 4.0 * module,
        576.0 + 116.0 - 5.0 * module,
        7.0 * module,
        module
    );
    assert!(content.contains(&finder), "{finder} not in {content}");
    // The code has ink; its quiet margin (here the bottom three points of the square) has none.
    let [code, quiet] = ink(
        &out,
        dir.path(),
        [[100, 576, 216, 692], [100, 576, 216, 579]],
    );
    assert!(code > 1500 && quiet == 0, "{code} {quiet}");
}

#[test]
fn redact_removes_text_from_the_content_and_keeps_the_rest_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let out = dir.path().join("out.pdf");
    let before = words(&pdf, 1);
    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "texts": ["ALPHA-1"]}),
    );
    assert_eq!(
        (v["verified"].as_bool(), v["text_matches"].as_u64()),
        (Some(true), Some(1))
    );
    assert_eq!(v["pages"][0]["glyphs_removed"], 7);

    // Gone from the file itself, not merely hidden.
    assert!(
        !content_of(&out, 1).contains("alpha"),
        "{}",
        content_of(&out, 1)
    );
    assert!(content_of(&out, 2).contains("alpha-2"));
    let page = &texts(&out)[0];
    assert!(
        page.contains("Page 1 of the sample. Keyword") && !page.contains("alpha"),
        "{page}"
    );
    assert_eq!(
        call("pdf_search", json!({"input": out, "query": "alpha-1"}))["total_matches"],
        0
    );

    // Every surviving word is exactly where it was, including the full stop after the redaction.
    let after = words(&out, 1);
    assert_eq!(after.len(), before.len());
    for (a, b) in after.iter().zip(&before).take(6) {
        assert_eq!(a, b);
    }
    assert_eq!(after[6].0, ".");
    assert!(
        (after[6].1[2] - before[6].1[2]).abs() < 0.1,
        "{:?} {:?}",
        after[6],
        before[6]
    );
    // And a black bar covers the place.
    let b = before[6].1;
    assert!(
        ink(
            &out,
            dir.path(),
            [[
                b[0] as usize + 2,
                792 - b[3] as usize + 2,
                b[2] as usize - 12,
                792 - b[1] as usize - 2
            ]]
        )[0] > 500
    );
}

#[test]
fn redact_matches_inside_words_without_taking_the_whole_word() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = custom(
        dir.path(),
        "id.pdf",
        &["BT /F1 12 Tf 72 700 Td (Account PL61109010140000071219812874 closed) Tj ET"],
    );
    let out = dir.path().join("out.pdf");
    call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "texts": [r"\d{26}"], "regex": true}),
    );
    let text = &texts(&out)[0];
    assert!(
        text.contains("Account PL") && text.contains("closed") && !text.contains("6110"),
        "{text}"
    );
}

#[test]
fn redact_blanks_image_pixels_and_drops_drawings_and_annotations() {
    let dir = tempfile::tempdir().unwrap();
    // A grey 64x64 image drawn at (50,600)-(150,700), i.e. 92-192 points from the top.
    let pdf = with_images(
        dir.path(),
        "i.pdf",
        vec![image_stream(
            64,
            64,
            "DeviceRGB".into(),
            8,
            vec![200; 64 * 64 * 3],
        )],
    );
    let out = dir.path().join("out.pdf");
    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "rects": ["1:50,92,100,192"]}),
    );
    assert_eq!(v["pages"][0]["images_blanked"], 1);
    let img = call(
        "pdf_images",
        json!({"input": out, "out_dir": dir.path().join("img")}),
    );
    let rgb = image::open(img["images"][0]["file"].as_str().unwrap())
        .unwrap()
        .to_rgb8();
    // The left half of the pixel data is destroyed; the right half is untouched.
    assert!((0..64).all(|y| (0..32).all(|x| rgb.get_pixel(x, y).0 == [0, 0, 0])));
    assert!((0..64).all(|y| (32..64).all(|x| rgb.get_pixel(x, y).0 == [200, 200, 200])));

    // A small drawing inside the area goes; the page frame that only crosses it stays.
    let drawing = "10 10 592 772 re S 100 100 50 50 re f BT /F1 12 Tf 72 700 Td (kept) Tj ET";
    let pdf = custom(dir.path(), "d.pdf", &[drawing]);
    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "rects": ["1:90,632,160,702"]}),
    );
    assert_eq!(v["pages"][0]["paths_removed"], 1);
    let content = content_of(&out, 1);
    assert!(
        !content.contains("100 100 50 50") && content.contains("10 10 592 772"),
        "{content}"
    );

    // A form field under the area is removed with its value.
    let filled = dir.path().join("filled.pdf");
    call(
        "pdf_fill",
        json!({"input": form(dir.path()), "output": filled, "values": {"name": "Ada Lovelace"}}),
    );
    let v = call(
        "pdf_redact",
        json!({"input": filled, "output": out, "rects": ["1:70,170,302,194"]}),
    );
    assert_eq!(v["pages"][0]["annotations_removed"], 1);
    assert!(
        !std::fs::read(&out)
            .unwrap()
            .windows(12)
            .any(|w| w == b"Ada Lovelace")
    );
}

#[test]
fn redact_reaches_into_forms_without_touching_other_pages() {
    use lopdf::{Document, Object, Stream, dictionary};
    let dir = tempfile::tempdir().unwrap();
    let pdf = custom(dir.path(), "f.pdf", &["/Fm1 Do", "/Fm1 Do"]);
    // Both pages draw the same form, which holds the text.
    let mut doc = Document::load(&pdf).unwrap();
    let font = doc.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let form = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        },
        b"BT /F1 12 Tf 72 700 Td (Secret 1234 here) Tj ET".to_vec(),
    ));
    for id in doc.get_pages().into_values() {
        let res = dictionary! { "XObject" => dictionary! { "Fm1" => Object::Reference(form) } };
        doc.get_dictionary_mut(id).unwrap().set("Resources", res);
    }
    doc.save(&pdf).unwrap();

    let out = dir.path().join("out.pdf");
    call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "texts": ["1234"], "pages": "1"}),
    );
    let t = texts(&out);
    assert!(t[0].contains("Secret") && !t[0].contains("1234"), "{t:?}");
    assert!(t[1].contains("Secret 1234 here"), "{t:?}");
}

#[test]
fn redact_refuses_bad_requests() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 1);
    let out = dir.path().join("out.pdf");
    assert!(
        call_err("pdf_redact", json!({"input": pdf, "output": out})).contains("nothing to redact")
    );
    assert!(
        call_err(
            "pdf_redact",
            json!({"input": pdf, "output": out, "texts": ["absent"]})
        )
        .contains("nothing was written")
    );
    assert!(
        call_err(
            "pdf_redact",
            json!({"input": pdf, "output": out, "rects": ["3:1,1,9,9"]})
        )
        .contains("out of range")
    );
    assert!(
        call_err(
            "pdf_redact",
            json!({"input": pdf, "output": out, "rects": ["1:1,1"]})
        )
        .contains("invalid rect")
    );
    assert!(!out.exists());
}

#[test]
fn replace_swaps_text_and_keeps_what_follows_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let out = dir.path().join("out.pdf");
    let before = words(&pdf, 1);
    let v = call(
        "pdf_replace",
        json!({"input": pdf, "output": out, "find": "sample", "with": "plan", "pages": "1"}),
    );
    assert_eq!(v["replacements"], 1);
    let page = &v["pages"][0];
    assert_eq!(
        page["in_original_font"].as_u64().unwrap() + page["in_substitute_font"].as_u64().unwrap(),
        1
    );
    let t = texts(&out);
    assert!(
        t[0].contains("Page 1 of the plan") && !t[0].contains("sample"),
        "{t:?}"
    );
    assert!(t[1].contains("sample"));
    // "Keyword" comes after the replaced word and must not have moved, though "plan" is shorter.
    let after = words(&out, 1);
    let keyword = |w: &[(String, [f64; 4])]| w.iter().find(|w| w.0 == "Keyword").unwrap().1;
    assert_eq!(keyword(&after), keyword(&before));

    // Capture groups, on every page.
    call(
        "pdf_replace",
        json!({"input": pdf, "output": out, "find": r"alpha-(\d)", "with": "id$1", "regex": true}),
    );
    let t = texts(&out);
    assert!(
        t[0].contains("Keyword id1") && t[1].contains("Keyword id2"),
        "{t:?}"
    );

    assert!(
        call_err(
            "pdf_replace",
            json!({"input": pdf, "output": out, "find": "absent", "with": "x"})
        )
        .contains("not found")
    );
}

#[test]
fn create_lays_out_markdown() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    let markdown = "# Quarterly report\n\nRevenue grew **12%** in *Q3*, see [the sheet](https://example.com/q3).\n\n\
        - first point\n- second point\n\n| Item | Qty |\n| --- | --- |\n| Bolt M8 | 40 |\n| Washer | 80 |\n\n```\nlet x = 1;\n```\n";
    let v = call("pdf_create", json!({"markdown": markdown, "output": out}));
    assert_eq!(
        (v["pages"].as_u64(), v["title"].as_str()),
        (Some(1), Some("Quarterly report"))
    );
    assert_eq!(
        call("pdf_info", json!({"input": out}))["metadata"]["title"],
        "Quarterly report"
    );

    let text = &texts(&out)[0];
    for expected in [
        "Quarterly report",
        "Revenue grew 12% in Q3, see the sheet.",
        "first point",
        "second point",
        "let x = 1;",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in {text}");
    }
    // The table is drawn with rules, so it reads back as the same table.
    let t = call("pdf_tables", json!({"input": out}));
    let table = t["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["detected_by"] == "lines")
        .expect("a ruled table");
    assert_eq!(
        table["cells"],
        json!([["Item", "Qty"], ["Bolt M8", "40"], ["Washer", "80"]])
    );

    // Emphasis uses the matching typefaces, and the heading is larger than the body.
    let layout = call("pdf_layout", json!({"input": out, "level": "words"}));
    let words = layout["pages"][0]["words"].as_array().unwrap();
    let size = |text: &str| {
        words.iter().find(|w| w["text"] == text).unwrap()["size"]
            .as_f64()
            .unwrap()
    };
    assert!(size("Quarterly") > size("Revenue") * 1.5);
    let content = content_of(&out, 1);
    assert!(content.contains("Tf"), "{content}");

    // The link is clickable.
    let doc = lopdf::Document::load(&out).unwrap();
    let page = doc.get_dictionary(doc.get_pages()[&1]).unwrap();
    let annots = page.get(b"Annots").unwrap().as_array().unwrap();
    assert_eq!(annots.len(), 2, "one annotation per word of the link text");
    let action = doc
        .get_dictionary(annots[0].as_reference().unwrap())
        .unwrap()
        .get(b"A")
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(
        action.get(b"URI").unwrap().as_str().unwrap(),
        b"https://example.com/q3"
    );
}

#[test]
fn create_breaks_pages_and_reads_files() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("long.md");
    let body: String = (1..=120)
        .map(|i| format!("Paragraph number {i} with enough words to fill a line.\n\n"))
        .collect();
    std::fs::write(&source, format!("# Long\n\n{body}")).unwrap();
    let out = dir.path().join("out.pdf");
    let v = call(
        "pdf_create",
        json!({"input": source, "output": out, "page_size": "letter"}),
    );
    assert!(v["pages"].as_u64().unwrap() >= 4, "{v}");
    let info = call("pdf_info", json!({"input": out}));
    assert_eq!(
        info["page_size_pt"],
        json!({"width": 612.0, "height": 792.0})
    );
    let all = texts(&out).join(" ");
    assert!(all.contains("Paragraph number 1 ") && all.contains("Paragraph number 120 "));

    assert!(
        call_err("pdf_create", json!({"output": out}))
            .contains("either a Markdown file or markdown text")
    );
    assert!(
        call_err(
            "pdf_create",
            json!({"markdown": "![x](https://e.com/a.png)", "output": out})
        )
        .contains("remote")
    );
}

#[test]
fn create_embeds_fonts_for_text_outside_latin1() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    let made = pdfops::tools::call(
        "pdf_create",
        json!({"markdown": "# Zażółć\n\nGęślą **jaźń** i *źdźbło* oraz `kod żółw`.", "output": out}),
    );
    if let Err(e) = &made {
        assert!(e.to_string().contains("no installed font"), "{e:#}");
        eprintln!("skipped: {e}");
        return;
    }
    let text = &texts(&out)[0];
    assert!(
        text.contains("Zażółć") && text.contains("Gęślą jaźń i źdźbło oraz kod żółw."),
        "{text}"
    );
}
