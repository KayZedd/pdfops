mod common;

use common::{
    call, call_err, content_of, custom, form, image_stream, ink, sample, texts, with_images, words,
};
use lopdf::dictionary;
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
fn replace_swaps_text_and_moves_what_follows_along() {
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
    // "plan" is 1333 thousandths narrower than "sample": at 18 points, what follows on the
    // line closes up by 24 points, and what stands before it stays.
    let after = words(&out, 1);
    let at = |w: &[(String, [f64; 4])], word: &str| w.iter().find(|w| w.0 == word).unwrap().1;
    assert!(
        (at(&before, "Keyword")[0] - at(&after, "Keyword")[0] - 24.0).abs() < 0.1,
        "{before:?} {after:?}"
    );
    assert!((at(&before, "alpha-1.")[0] - at(&after, "alpha-1.")[0] - 24.0).abs() < 0.1);
    assert_eq!(at(&before, "the"), at(&after, "the"));
    // The full stop still follows its word directly.
    assert!(after.iter().any(|w| w.0 == "plan."), "{after:?}");

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

#[test]
fn annotate_marks_text_and_lists_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let out = dir.path().join("out.pdf");
    let word = words(&pdf, 1)
        .into_iter()
        .find(|w| w.0 == "alpha-1.")
        .unwrap()
        .1;

    let v = call(
        "pdf_annotate",
        json!({"input": pdf, "output": out, "texts": ["alpha-1"], "comment": "check this", "author": "Ada", "pages": "1"}),
    );
    assert_eq!(v["added"], 1);
    let listed = call("pdf_annotations", json!({"input": out}));
    let a = &listed["annotations"][0];
    assert_eq!(
        (
            a["page"].as_u64(),
            a["type"].as_str(),
            a["comment"].as_str(),
            a["author"].as_str()
        ),
        (Some(1), Some("highlight"), Some("check this"), Some("Ada"))
    );
    // The mark sits on the matched characters: the word without its full stop.
    let r: Vec<f64> = a["rect"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert!(
        (r[0] - word[0]).abs() < 0.2
            && r[2] < word[2]
            && r[2] > word[2] - 8.0
            && (r[1] - word[1]).abs() < 0.2,
        "{r:?} {word:?}"
    );
    // The page text is untouched.
    assert!(texts(&out)[0].contains("Keyword alpha-1."));

    // A dark underline shows up where the word is when the page is rendered.
    call(
        "pdf_annotate",
        json!({"input": pdf, "output": out, "kind": "underline", "texts": ["the"], "color": "000080"}),
    );
    let s = words(&pdf, 1).into_iter().find(|w| w.0 == "the").unwrap().1;
    // "the" has no descenders, so the strip under its baseline starts out empty.
    let below = [
        s[0] as usize + 2,
        792 - s[3] as usize,
        s[2] as usize - 2,
        792 - s[3] as usize + 3,
    ];
    assert_eq!(ink(&pdf, dir.path(), [below])[0], 0);
    assert!(ink(&out, dir.path(), [below])[0] > 20);
    assert_eq!(
        call("pdf_annotations", json!({"input": out}))["annotations"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "one on each page"
    );

    // The other kinds, and what each requires.
    call(
        "pdf_annotate",
        json!({"input": pdf, "output": out, "kind": "link", "rects": ["1:72,50,200,80"], "url": "https://example.com/x"}),
    );
    call(
        "pdf_annotate",
        json!({"input": out, "output": out, "kind": "note", "rects": ["2:100,100,120,120"], "comment": "remember"}),
    );
    call(
        "pdf_annotate",
        json!({"input": out, "output": out, "kind": "box", "rects": ["2:50,50,150,90"]}),
    );
    let listed = call("pdf_annotations", json!({"input": out}));
    let kinds: Vec<(u64, &str)> = listed["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["page"].as_u64().unwrap(), a["type"].as_str().unwrap()))
        .collect();
    assert_eq!(kinds, [(1, "link"), (2, "text"), (2, "square")]);
    assert_eq!(listed["annotations"][0]["url"], "https://example.com/x");
    assert_eq!(
        listed["annotations"][0]["rect"],
        json!([72.0, 50.0, 200.0, 80.0])
    );
    assert_eq!(listed["annotations"][1]["comment"], "remember");

    assert!(
        call_err(
            "pdf_annotate",
            json!({"input": pdf, "output": out, "kind": "link", "texts": ["sample"]})
        )
        .contains("needs a url")
    );
    assert!(
        call_err(
            "pdf_annotate",
            json!({"input": pdf, "output": out, "kind": "note", "texts": ["sample"]})
        )
        .contains("needs a comment")
    );
    assert!(
        call_err(
            "pdf_annotate",
            json!({"input": pdf, "output": out, "texts": ["absent"]})
        )
        .contains("nothing was written")
    );
    assert!(
        call_err("pdf_annotate", json!({"input": pdf, "output": out})).contains("nothing to mark")
    );
}

#[test]
fn annotations_report_links_between_pages() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 3);
    let mut doc = lopdf::Document::load(&pdf).unwrap();
    let pages: Vec<_> = doc.get_pages().into_values().collect();
    use lopdf::dictionary;
    let link = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![72.into(), 700.into(), 172.into(), 720.into()],
        "Dest" => vec![lopdf::Object::Reference(pages[2]), "Fit".into()],
    });
    doc.get_dictionary_mut(pages[0])
        .unwrap()
        .set("Annots", vec![lopdf::Object::Reference(link)]);
    doc.save(&pdf).unwrap();
    let v = call("pdf_annotations", json!({"input": pdf}));
    assert_eq!(
        v["annotations"],
        json!([{"page": 1, "type": "link", "rect": [72.0, 72.0, 172.0, 92.0], "target_page": 3}])
    );
}

#[test]
fn sign_and_verify_including_tampering_and_countersigning() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let (cert, key) = common::identity(dir.path(), "Ada Signer");
    let signed = dir.path().join("signed.pdf");

    assert_eq!(
        call("pdf_signatures", json!({"input": pdf}))["signatures"],
        0
    );
    let v = call(
        "pdf_sign",
        json!({"input": pdf, "output": signed, "cert": cert, "key": key, "reason": "Approval", "location": "Kraków"}),
    );
    assert_eq!(
        (v["signer"].as_str(), v["earlier_signatures"].as_u64()),
        (Some("Ada Signer"), Some(0))
    );
    // The original bytes are still there, untouched, at the start of the signed file.
    let (before, after) = (
        std::fs::read(&pdf).unwrap(),
        std::fs::read(&signed).unwrap(),
    );
    assert_eq!(&after[..before.len()], &before[..]);

    let v = call("pdf_signatures", json!({"input": signed}));
    assert_eq!(
        (v["signatures"].as_u64(), v["valid"].as_u64()),
        (Some(1), Some(1))
    );
    let s = &v["details"][0];
    for (key, expected) in [
        ("valid", json!(true)),
        ("document_unchanged", json!(true)),
        ("signature_genuine", json!(true)),
        ("covers_whole_document", json!(true)),
        ("signer", json!("Ada Signer")),
        ("reason", json!("Approval")),
        ("location", json!("Kraków")),
        ("field", json!("Signature1")),
    ] {
        assert_eq!(s[key], expected, "{key}: {s}");
    }
    assert_eq!(s["certificate"]["self_signed"], true);
    assert!(texts(&signed)[1].contains("alpha-2"));

    // One changed character in the signed content is enough.
    let mut forged = after.clone();
    let at = forged.windows(7).position(|w| w == b"alpha-1").unwrap();
    forged[at + 6] = b'7';
    let forged_path = dir.path().join("forged.pdf");
    std::fs::write(&forged_path, forged).unwrap();
    let v = call("pdf_signatures", json!({"input": forged_path}));
    let s = &v["details"][0];
    assert_eq!(
        (
            v["valid"].as_u64(),
            s["document_unchanged"].as_bool(),
            s["signature_genuine"].as_bool()
        ),
        (Some(0), Some(false), Some(true))
    );

    // A second signer appends; the first signature stays valid for the part it covers.
    let (cert2, key2) = common::identity(dir.path(), "Bob Countersigner");
    let twice = dir.path().join("twice.pdf");
    let v = call(
        "pdf_sign",
        json!({"input": signed, "output": twice, "cert": cert2, "key": key2}),
    );
    assert_eq!(
        (
            v["earlier_signatures"].as_u64(),
            v["earlier_signatures_kept"].as_bool()
        ),
        (Some(1), Some(true))
    );
    let v = call("pdf_signatures", json!({"input": twice}));
    assert_eq!(
        (v["signatures"].as_u64(), v["valid"].as_u64()),
        (Some(2), Some(2))
    );
    let summary: Vec<(&str, bool)> = v["details"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["signer"].as_str().unwrap(),
                s["covers_whole_document"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [("Ada Signer", false), ("Bob Countersigner", true)]
    );

    // The key must belong to the certificate, and one identity must be given.
    let e = call_err(
        "pdf_sign",
        json!({"input": pdf, "output": signed, "cert": cert, "key": key2}),
    );
    assert!(
        e.contains("none of the certificates belongs to the private key"),
        "{e}"
    );
    assert!(
        call_err("pdf_sign", json!({"input": pdf, "output": signed}))
            .contains("either a PKCS #12 file")
    );
}

#[test]
fn sign_accepts_rsa_keys_and_pkcs12_files() {
    use rsa::pkcs8::EncodePrivateKey;
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 1);
    // An RSA identity, as certificate authorities commonly issue them.
    let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let pem = private.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
    let pair = rcgen::KeyPair::from_pkcs8_pem_and_sign_algo(&pem, &rcgen::PKCS_RSA_SHA256).unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Rsa Signer");
    let cert = params.self_signed(&pair).unwrap();
    let (cert_path, key_path) = (dir.path().join("rsa.crt"), dir.path().join("rsa.key"));
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, pem.as_bytes()).unwrap();

    let out = dir.path().join("rsa.pdf");
    let v = call(
        "pdf_sign",
        json!({"input": pdf, "output": out, "cert": cert_path, "key": key_path}),
    );
    assert_eq!(v["algorithm"], "RSA with SHA-256");
    let v = call("pdf_signatures", json!({"input": out}));
    assert_eq!(
        (v["valid"].as_u64(), v["details"][0]["signer"].as_str()),
        (Some(1), Some("Rsa Signer"))
    );

    // The same identity packed into a password-protected PKCS #12 file.
    let chain = p12_keystore::PrivateKeyChain::new(
        [7u8; 20].to_vec(),
        p12_keystore::PrivateKey::from_der(private.to_pkcs8_der().unwrap().as_bytes()).unwrap(),
        [p12_keystore::Certificate::from_der(cert.der()).unwrap()],
    );
    let mut store = p12_keystore::KeyStore::new();
    store.add_entry("id", p12_keystore::KeyStoreEntry::PrivateKeyChain(chain));
    let p12 = dir.path().join("id.p12");
    std::fs::write(&p12, store.writer("s3cret").write().unwrap()).unwrap();

    let v = call(
        "pdf_sign",
        json!({"input": pdf, "output": out, "p12": p12, "p12_password": "s3cret"}),
    );
    assert_eq!(v["signer"], "Rsa Signer");
    assert_eq!(call("pdf_signatures", json!({"input": out}))["valid"], 1);
    let e = call_err(
        "pdf_sign",
        json!({"input": pdf, "output": out, "p12": p12, "p12_password": "wrong"}),
    );
    assert!(e.contains("wrong password"), "{e}");
}

#[test]
fn dry_run_reports_the_plan_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let out = dir.path().join("out.pdf");
    let w = words(&pdf, 1);

    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "texts": ["alpha-1"], "rects": ["2:10,10,20,20"], "dry_run": true}),
    );
    assert_eq!(
        (&v["dry_run"], &v["verified"]),
        (&json!(true), &json!(true))
    );
    assert_eq!(v["pages"][0]["glyphs_removed"], 7);
    let target = &v["pages"][0]["targets"][0];
    assert_eq!(target["text"], "alpha-1");
    // The box is that of the matched glyphs, as layout reports the word minus its full stop.
    let b = w[6].1;
    assert!(
        (target["bbox"][0].as_f64().unwrap() - b[0]).abs() < 0.1
            && target["bbox"][2].as_f64().unwrap() < b[2],
        "{target} {b:?}"
    );
    assert_eq!(
        v["pages"][1]["targets"],
        json!([{"bbox": [10.0, 10.0, 20.0, 20.0]}])
    );
    assert!(!out.exists());

    let v = call(
        "pdf_replace",
        json!({"input": pdf, "output": out, "find": "sample", "with": "template for all", "pages": "1", "dry_run": true}),
    );
    assert_eq!(
        (&v["dry_run"], &v["replacements"]),
        (&json!(true), &json!(1))
    );
    let m = &v["pages"][0]["matches"][0];
    assert_eq!(
        (&m["old"], &m["new"], &m["font"]),
        (
            &json!("sample"),
            &json!("template for all"),
            &json!("original")
        )
    );
    // "template for all" is 3335 thousandths wider than "sample": 60 points at 18 point
    // Helvetica, which the line has room for.
    assert!(
        (m["width_change_pt"].as_f64().unwrap() - 60.0).abs() < 0.2,
        "{m}"
    );
    assert_eq!(m["overflow_pt"], 0.0, "{m}");
    assert!(!out.exists());

    let v = call(
        "pdf_annotate",
        json!({"input": pdf, "output": out, "texts": ["keyword"], "dry_run": true}),
    );
    assert_eq!((&v["dry_run"], &v["added"]), (&json!(true), &json!(2)));
    assert_eq!(
        (&v["targets"][1]["page"], &v["targets"][1]["text"]),
        (&json!(2), &json!("Keyword"))
    );
    assert!(!out.exists());

    let v = call(
        "pdf_stamp",
        json!({"input": pdf, "output": out, "text": "DRAFT", "dry_run": true}),
    );
    assert_eq!(
        (&v["dry_run"], &v["stamped_pages"]),
        (&json!(true), &json!([1, 2]))
    );
    assert!(v["size_bytes"].as_u64().unwrap() > 0 && !out.exists());

    // Without the flag the same call writes, and says so.
    let v = call(
        "pdf_stamp",
        json!({"input": pdf, "output": out, "text": "DRAFT"}),
    );
    assert_eq!(v["dry_run"], false);
    assert!(out.exists());
}

#[test]
fn right_to_left_text_is_shaped_and_laid_out_from_the_right() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    // "shalom olam" in a footer, and an Arabic paragraph whose letters join.
    let stamped = pdfops::tools::call(
        "pdf_stamp",
        json!({"input": sample(dir.path(), "a.pdf", 1), "output": out, "text": "\u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{5e2}\u{5d5}\u{5dc}\u{5dd}", "position": "footer"}),
    );
    if let Err(e) = &stamped {
        assert!(e.to_string().contains("no installed font"), "{e:#}");
        eprintln!("skipped: {e}");
        return;
    }
    // The text reads back as it was given, though its glyphs are drawn from the left,
    // the last letter of the last word first.
    let text = &texts(&out)[0];
    assert!(
        text.contains("\u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{5e2}\u{5d5}\u{5dc}\u{5dd}"),
        "{text}"
    );
    let footer = words(&out, 1);
    let (olam, shalom) = (&footer[footer.len() - 2], &footer[footer.len() - 1]);
    assert!(
        olam.0.starts_with('\u{5e2}') && olam.1[2] < shalom.1[0],
        "{footer:?}"
    );
    // So it is found, marked and replaced by the words a person types.
    let found = call(
        "pdf_search",
        json!({"input": out, "query": "\u{5e2}\u{5d5}\u{5dc}\u{5dd}"}),
    );
    assert_eq!(found["total_matches"], 1, "{found}");
    let gone = dir.path().join("gone.pdf");
    let v = call(
        "pdf_redact",
        json!({"input": out, "output": gone, "texts": ["\u{5e2}\u{5d5}\u{5dc}\u{5dd}"]}),
    );
    assert_eq!(v["text_matches"], 1, "{v}");
    let text = &texts(&gone)[0];
    assert!(
        text.contains("\u{5e9}\u{5dc}\u{5d5}\u{5dd}") && !text.contains('\u{5e2}'),
        "{text}"
    );

    let made = pdfops::tools::call(
        "pdf_create",
        json!({"markdown": "\u{627}\u{644}\u{633}\u{644}\u{627}\u{645} \u{639}\u{644}\u{64a}\u{643}\u{645}", "output": out}),
    );
    if made.is_err() {
        return;
    }
    // Every letter is there, the lam-alef pairs as ligatures that read as both letters.
    let text = &texts(&out)[0];
    for letter in "\u{627}\u{644}\u{633}\u{645}\u{639}\u{64a}\u{643}".chars() {
        assert!(text.contains(letter), "{letter} missing from {text}");
    }
    assert_eq!(
        text.chars().filter(|c| !c.is_whitespace()).count(),
        11,
        "{text}"
    );
    // The first word of the sentence stands on the right.
    let line = words(&out, 1);
    assert_eq!(line.len(), 2, "{line:?}");
    assert!(
        line.iter().any(|w| w.0.contains('\u{633}')
            && w.1[0] > line.iter().map(|o| o.1[0]).fold(f64::MAX, f64::min)),
        "{line:?}"
    );
}

#[test]
fn replace_makes_room_on_the_line_and_only_there() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    // Every word placed on its own, as many producers write text; two lines.
    let page = "BT /F1 10 Tf \
        1 0 0 1 72 700 Tm (Total) Tj 1 0 0 1 120 700 Tm (2025) Tj 1 0 0 1 160 700 Tm (EUR) Tj \
        1 0 0 1 72 686 Tm (Paid) Tj 1 0 0 1 120 686 Tm (none) Tj 1 0 0 1 160 686 Tm (EUR) Tj ET";
    let pdf = custom(dir.path(), "p.pdf", &[page]);
    let before = words(&pdf, 1);
    let v = call(
        "pdf_replace",
        json!({"input": pdf, "output": out, "find": "2025", "with": "20252025"}),
    );
    assert_eq!(
        (&v["replacements"], &v["pages"][0]["overflow_pt"]),
        (&json!(1), &json!(0.0))
    );
    let after = words(&out, 1);
    let texts: Vec<&str> = after.iter().map(|w| w.0.as_str()).collect();
    assert_eq!(texts, ["Total", "20252025", "EUR", "Paid", "none", "EUR"]);
    // Four more figures of 556 thousandths at 10 points: the word after moves 22.24 points.
    assert!(
        (after[2].1[0] - before[2].1[0] - 22.24).abs() < 0.05,
        "{after:?}"
    );
    // The word before, and the whole of the other line, stay where they were.
    assert_eq!(after[0], before[0]);
    assert_eq!(after[3..], before[3..]);
    // A second edit of the result starts from a sound page.
    call(
        "pdf_replace",
        json!({"input": out, "output": out, "find": "20252025", "with": "2025"}),
    );
    let back = words(&out, 1);
    for (a, b) in back.iter().zip(&before) {
        assert!(a.0 == b.0 && (a.1[0] - b.1[0]).abs() < 0.05, "{back:?}");
    }
}

#[test]
fn replace_draws_a_line_together_to_keep_it_in_its_column() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    // A column of three full lines, the second with a word to replace.
    let column = "BT /F1 12 Tf 72 700 Td (The agreement runs from the first day) Tj \
        0 -15 Td (of May until notice is given by either) Tj \
        0 -15 Td (party, as the schedule below sets out.) Tj ET";
    let pdf = custom(dir.path(), "c.pdf", &[column]);
    let before = words(&pdf, 1);
    let edge = before.iter().map(|w| w.1[2]).fold(0.0, f64::max);
    let line = |w: &[(String, [f64; 4])]| -> Vec<(String, [f64; 4])> {
        w.iter()
            .filter(|w| (w.1[1] - before[7].1[1]).abs() < 1.0)
            .cloned()
            .collect()
    };
    let was = line(&before).last().unwrap().1[2];

    // "May" to "March" lengthens a line that has less room than that to the edge.
    let v = call(
        "pdf_replace",
        json!({"input": pdf, "output": out, "find": "May", "with": "March", "case_sensitive": true}),
    );
    assert_eq!(v["pages"][0]["overflow_pt"], 0.0, "{v}");
    let after = line(&words(&out, 1));
    let now: Vec<&str> = after.iter().map(|w| w.0.as_str()).collect();
    assert_eq!(
        now,
        [
            "of", "March", "until", "notice", "is", "given", "by", "either"
        ]
    );
    // The line ends at the column's edge, not beyond it, and its words stay apart.
    let end = after.last().unwrap().1[2];
    assert!(end <= edge + 0.6 && end > was, "{end} {edge} {was}");
    assert!(after.windows(2).all(|p| p[0].1[2] < p[1].1[0]), "{after:?}");

    // Far more than drawing together can absorb: the rest is reported.
    let v = call(
        "pdf_replace",
        json!({"input": pdf, "output": out, "find": "May", "with": "the month after that", "case_sensitive": true}),
    );
    let over = v["pages"][0]["overflow_pt"].as_f64().unwrap();
    let end = line(&words(&out, 1)).last().unwrap().1[2];
    assert!(
        over > 20.0 && (end - edge - over).abs() < 1.0,
        "{over} {end} {edge}"
    );
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

/// The colour of a page at a point given from its top-left corner, rendered at 72 dpi.
fn colour_at(pdf: &std::path::Path, dir: &std::path::Path, x: usize, y: usize) -> [u8; 3] {
    let v = call(
        "pdf_render",
        json!({"input": pdf, "out_dir": dir, "dpi": 72}),
    );
    let page = image::open(v["files"][0]["file"].as_str().unwrap())
        .unwrap()
        .to_rgb8();
    page.get_pixel(x as u32, y as u32).0
}

/// A 16x16 JPEG in CMYK, written the way Adobe's software does, with every value inverted.
const CMYK_JPEG: &str = "ffd8ffee000e41646f626500640000000000ffdb0043000302020302020303030304030304050805050404050a070706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffc000140800100010044311004d11005911004b1100ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda000e0443004d0059004b00003f00fd53af8f2be3cafd53a28a28a28a28a28a28afffd9";
/// A 16x16 JPEG 2000 image of one colour.
const JPEG_2000: &str = "0000000c6a5020200d0a870a00000014667479706a703220000000006a7032200000002d6a703268000000166968647200000010000000100003070700000000000f636f6c7201000000000010000000ad6a703263ff4fff51002f000000000010000000100000000000000000000000100000001000000000000000000003070101070101070101ff52000c00000001000404040001ff5c00104040484850484850484850484850ff640025000143726561746564206279204f70656e4a5045472076657273696f6e20322e352e34ff90000a0000000000290001ff93cfb408044fc3e704096fcfb40806cf808080808080808080808080ffd9";
/// The same with a fourth channel that makes it opaque everywhere.
const JPEG_2000_ALPHA: &str = "0000000c6a5020200d0a870a00000014667479706a703220000000006a7032200000004f6a703268000000166968647200000010000000100004070700000000000f636f6c720100000000001000000022636465660004000000000001000100000002000200000003000300010000000000b86a703263ff4fff510032000000000010000000100000000000000000000000100000001000000000000000000004070101070101070101070101ff52000c00000001000404040001ff5c00104040484850484850484850484850ff640025000143726561746564206279204f70656e4a5045472076657273696f6e20322e352e34ff90000a0000000000310001ff93cfb408044fc3e704096fcfb40806cfcfb4040080808080808080808080808080808080ffd9";

#[test]
fn redact_blanks_cmyk_jpeg_and_jpeg_2000_images() {
    let dir = tempfile::tempdir().unwrap();
    let packed = |filter: &str, extra: lopdf::Dictionary, hex: &str| {
        let mut dict = dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 16, "Height" => 16,
            "Filter" => filter,
        };
        dict.extend(&extra);
        lopdf::Stream::new(dict, unhex(hex)).with_compression(false)
    };
    let inverted: Vec<lopdf::Object> = [1, 0, 1, 0, 1, 0, 1, 0].map(Into::into).to_vec();
    // Drawn side by side, each 100 points wide, at x = 50, 170 and 290.
    let pdf = with_images(
        dir.path(),
        "packed.pdf",
        vec![
            packed(
                "DCTDecode",
                dictionary! {
                    "ColorSpace" => "DeviceCMYK", "BitsPerComponent" => 8, "Decode" => inverted,
                },
                CMYK_JPEG,
            ),
            // JPEG 2000 may leave the colour model and depth to the data.
            packed("JPXDecode", lopdf::Dictionary::new(), JPEG_2000),
            packed(
                "JPXDecode",
                dictionary! { "SMaskInData" => 1 },
                JPEG_2000_ALPHA,
            ),
        ],
    );
    let out = dir.path().join("out.pdf");
    // The left half of every image.
    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "rects": [
            "1:50,92,100,192", "1:170,92,220,192", "1:290,92,340,192",
        ]}),
    );
    assert_eq!(v["pages"][0]["images_blanked"], 3, "{v}");

    // What is left of each image looks as it did.
    for x in [125, 245, 365] {
        let before = colour_at(&pdf, &dir.path().join("before"), x, 142);
        let after = colour_at(&out, &dir.path().join("after"), x, 142);
        assert!(before != [255, 255, 255], "nothing drawn at {x}");
        assert!(
            before.iter().zip(after).all(|(a, b)| a.abs_diff(b) <= 2),
            "at {x}: {before:?} became {after:?}"
        );
    }
    // The pixels under the areas are gone from the data, not just covered.
    let doc = lopdf::Document::load(&out).unwrap();
    let mut seen = 0;
    for object in doc.objects.values() {
        let Ok(stream) = object.as_stream() else {
            continue;
        };
        if !stream.dict.has(b"Width") {
            continue;
        }
        assert!(
            stream.dict.get(b"Filter").unwrap().as_name().unwrap() == b"FlateDecode"
                && !stream.dict.has(b"SMaskInData"),
            "{:?}",
            stream.dict
        );
        let data = stream.decompressed_content().unwrap();
        let sample = data.len() / (16 * 16);
        for row in data.chunks(16 * sample) {
            assert!(row[..8 * sample].iter().all(|&b| b == 0), "{row:?}");
            assert!(row[8 * sample..].iter().any(|&b| b != 0), "{row:?}");
        }
        seen += 1;
    }
    // Three images and the mask that the transparency of the last one became.
    assert_eq!(seen, 4);
}

#[test]
fn redact_blanks_fax_coded_masks_as_one_bit_data() {
    let dir = tempfile::tempdir().unwrap();
    // Sixteen all-white rows in Group 4 coding: one "same as the row above" code each.
    let mask = lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 16, "Height" => 16,
            "ImageMask" => true, "Filter" => "CCITTFaxDecode",
            "DecodeParms" => dictionary! { "K" => -1, "Columns" => 16, "Rows" => 16 },
        },
        vec![0xFF, 0xFF],
    )
    .with_compression(false);
    let pdf = with_images(dir.path(), "fax.pdf", vec![mask]);
    let out = dir.path().join("out.pdf");
    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "rects": ["1:50,92,100,192"]}),
    );
    assert_eq!(v["pages"][0]["images_blanked"], 1, "{v}");
    let doc = lopdf::Document::load(&out).unwrap();
    let stream = doc
        .objects
        .values()
        .filter_map(|o| o.as_stream().ok())
        .find(|s| s.dict.has(b"ImageMask"))
        .unwrap();
    // Still one bit a pixel, two bytes a row: the left byte cleared, the right one as it was.
    let data = stream.decompressed_content().unwrap();
    assert_eq!(data, [0x00, 0xFF].repeat(16), "{:?}", stream.dict);
}

#[test]
fn redact_and_replace_work_on_pages_with_inline_images() {
    let dir = tempfile::tempdir().unwrap();
    // A 4x4 grey image written into the content itself, hex coded, at (50,600)-(150,700).
    // "EI" also occurs in the text that follows.
    let page = format!(
        "q 100 0 0 100 50 600 cm BI /W 4 /H 4 /CS /RGB /BPC 8 /F /AHx ID {}> EI Q \
         BT /F1 12 Tf 72 500 Td (SEIZE THE BI DAY EI) Tj 0 -20 Td (Account 6110) Tj ET",
        "c8".repeat(4 * 4 * 3)
    );
    let pdf = custom(dir.path(), "inline.pdf", &[&page]);
    let out = dir.path().join("out.pdf");

    // Under an area, the image loses those pixels.
    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "rects": ["1:50,92,100,192"]}),
    );
    assert_eq!(v["pages"][0]["images_blanked"], 1, "{v}");
    let img = call(
        "pdf_images",
        json!({"input": out, "out_dir": dir.path().join("cut")}),
    );
    let rgb = image::open(img["images"][0]["file"].as_str().unwrap())
        .unwrap()
        .to_rgb8();
    assert!((0..4).all(|y| (0..2).all(|x| rgb.get_pixel(x, y).0 == [0, 0, 0])));
    assert!((0..4).all(|y| (2..4).all(|x| rgb.get_pixel(x, y).0 == [200, 200, 200])));
    assert!(texts(&out)[0].contains("SEIZE THE BI DAY EI"));

    // Elsewhere on the page, text goes and the image stays whole.
    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "texts": ["6110"]}),
    );
    assert_eq!(v["pages"][0]["images_blanked"], 0, "{v}");
    let text = &texts(&out)[0];
    assert!(text.contains("Account") && !text.contains("6110"), "{text}");
    let img = call(
        "pdf_images",
        json!({"input": out, "out_dir": dir.path().join("whole")}),
    );
    let rgb = image::open(img["images"][0]["file"].as_str().unwrap())
        .unwrap()
        .to_rgb8();
    assert!(rgb.pixels().all(|p| p.0 == [200, 200, 200]));

    call(
        "pdf_replace",
        json!({"input": pdf, "output": out, "find": "Account", "with": "Number"}),
    );
    assert!(texts(&out)[0].contains("Number 6110"));
    assert_eq!(
        colour_at(&out, &dir.path().join("page"), 100, 142),
        [200, 200, 200]
    );
}

#[test]
fn create_lays_out_lines_that_mix_directions() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    // "shalom olam" inside an English sentence with a comma after it, then alone
    // as a paragraph of its own that ends in a full stop.
    let hebrew = "\u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{5e2}\u{5d5}\u{5dc}\u{5dd}";
    let made = pdfops::tools::call(
        "pdf_create",
        json!({"markdown": format!("He said {hebrew}, then left.\n\n{hebrew}."), "output": out}),
    );
    if let Err(e) = &made {
        assert!(e.to_string().contains("no installed font"), "{e:#}");
        eprintln!("skipped: {e}");
        return;
    }
    let found = words(&out, 1);
    let top = found.iter().map(|w| w.1[1]).fold(f64::MAX, f64::min);
    let (first, second): (Vec<_>, Vec<_>) = found.iter().partition(|w| w.1[1] < top + 5.0);
    let with = |line: &[&(String, [f64; 4])], c: char| {
        line.iter()
            .find(|w| w.0.contains(c))
            .unwrap_or_else(|| panic!("{c} not in {line:?}"))
            .1
    };
    // In the sentence the Hebrew is read from its right end, and the comma follows it
    // where the sentence goes on: after the first Hebrew word's right-hand neighbour.
    let (said, olam, shalom, then) = (
        with(&first, 'd'),
        with(&first, '\u{5e2}'),
        with(&first, '\u{5e9}'),
        with(&first, 'h'),
    );
    assert!(
        said[2] < olam[0] && olam[2] < shalom[0] && shalom[2] < then[0],
        "{first:?}"
    );
    assert_eq!(with(&first, ','), shalom, "{first:?}");
    // The paragraph that runs from the right stands against the right margin, its
    // first word there and its full stop at the far left.
    let (olam, shalom) = (with(&second, '\u{5e2}'), with(&second, '\u{5e9}'));
    assert!(olam[2] < shalom[0], "{second:?}");
    assert!((shalom[2] - (595.28 - 56.0)).abs() < 2.0, "{second:?}");
    assert_eq!(with(&second, '.'), olam, "{second:?}");
}

#[test]
fn create_draws_scripts_that_no_single_font_has() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    // Polish, Chinese and Devanagari in one paragraph.
    let text =
        "Za\u{17c}\u{f3}\u{142}\u{107} \u{4f60}\u{597d}\u{4e16}\u{754c} \u{915}\u{92e}\u{932} end";
    let made = pdfops::tools::call("pdf_create", json!({"markdown": text, "output": out}));
    if let Err(e) = &made {
        assert!(e.to_string().contains("no installed font"), "{e:#}");
        eprintln!("skipped: {e}");
        return;
    }
    // Every word reads back, whichever font drew it.
    assert_eq!(texts(&out)[0], text);
    // Fonts that stand in are separate font objects, each with what it draws.
    let doc = lopdf::Document::load(&out).unwrap();
    let page = doc.get_pages()[&1];
    let fonts = doc.get_page_fonts(page).unwrap();
    let bases: std::collections::HashSet<Vec<u8>> = fonts
        .values()
        .map(|f| f.get(b"BaseFont").unwrap().as_name().unwrap()[7..].to_vec())
        .collect();
    assert_eq!(bases.len(), fonts.len(), "{bases:?}");
    eprintln!("{} fonts drew the paragraph", fonts.len());
}

#[test]
fn redact_takes_the_text_out_of_metadata_and_bookmarks_too() {
    use lopdf::{Object, Stream};
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let mut doc = lopdf::Document::load(&pdf).unwrap();
    // The words on page 1 also stand in the title, in the metadata stream, in a
    // bookmark and in an attached file.
    let info = doc.add_object(dictionary! {
        "Title" => Object::string_literal("Minutes, Keyword alpha-1 and more"),
        "Author" => Object::string_literal("Somebody Else"),
    });
    doc.trailer.set("Info", info);
    let xmp = doc.add_object(Stream::new(
        dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        b"<x:xmpmeta><dc:title>Minutes, Keyword alpha-1 and more</dc:title></x:xmpmeta>".to_vec(),
    ));
    let attached = doc.add_object(Stream::new(
        dictionary! { "Type" => "EmbeddedFile" },
        b"notes: keyword ALPHA-1 was discussed".to_vec(),
    ));
    let file = doc.add_object(dictionary! {
        "Type" => "Filespec", "F" => Object::string_literal("notes.txt"),
        "EF" => dictionary! { "F" => attached },
    });
    let catalog = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
    let outlines = doc
        .get_dictionary(catalog)
        .unwrap()
        .get(b"Outlines")
        .unwrap()
        .as_reference()
        .unwrap();
    let item = doc
        .get_dictionary(outlines)
        .unwrap()
        .get(b"First")
        .unwrap()
        .as_reference()
        .unwrap();
    doc.get_dictionary_mut(item)
        .unwrap()
        .set("Title", Object::string_literal("About alpha-1"));
    let catalog = doc.get_dictionary_mut(catalog).unwrap();
    catalog.set("Metadata", xmp);
    catalog.set(
        "Names",
        dictionary! { "EmbeddedFiles" => dictionary! {
            "Names" => vec![Object::string_literal("notes.txt"), Object::Reference(file)],
        } },
    );
    doc.save(&pdf).unwrap();

    let out = dir.path().join("out.pdf");
    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "texts": ["alpha-1"]}),
    );
    assert_eq!(
        v["beside_pages"],
        json!({
            "metadata_fields_cleaned": ["Title"],
            "xmp_metadata_removed": true,
            "bookmarks_cleaned": 1,
            "attachments_holding_the_text": ["notes.txt"],
        }),
        "{v}"
    );
    let info = call("pdf_info", json!({"input": out}));
    assert_eq!(info["metadata"]["title"], "Minutes, Keyword  and more");
    assert_eq!(info["metadata"]["author"], "Somebody Else");
    let outline = call("pdf_outline", json!({"input": out}));
    assert_eq!(outline["entries"][0]["title"], "About ");
    // The metadata stream is gone with what it repeated. The attached file is named
    // in the result and left for the caller to decide about.
    let bytes = std::fs::read(&out).unwrap();
    assert!(!bytes.windows(9).any(|w| w == b"<dc:title"));
    assert!(!texts(&out)[0].contains("alpha-1"));

    // A text that stands nowhere on the pages is still taken out of the title.
    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "texts": ["Minutes"]}),
    );
    assert_eq!(v["text_matches"], 0, "{v}");
    assert_eq!(
        v["beside_pages"]["metadata_fields_cleaned"],
        json!(["Title"])
    );
    // Areas alone say nothing about texts, so nothing beside the pages is touched.
    let v = call(
        "pdf_redact",
        json!({"input": pdf, "output": out, "rects": ["1:50,50,300,300"]}),
    );
    assert!(v.get("beside_pages").is_none(), "{v}");
}

#[test]
fn signatures_are_checked_against_certificates_the_caller_trusts() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 1);
    let (cert, key, authority) = common::issued_identity(dir.path(), "Ada Signer");
    let (stranger, _) = common::identity(dir.path(), "Somebody Else");
    let signed = dir.path().join("signed.pdf");
    call(
        "pdf_sign",
        json!({"input": pdf, "output": signed, "cert": cert, "key": key}),
    );
    // Nothing is said about trust unless certificates to trust are given.
    let v = call("pdf_signatures", json!({"input": signed}));
    assert!(v["details"][0].get("trusted").is_none(), "{v}");
    assert!(
        v["details"][0]["trust"]
            .as_str()
            .unwrap()
            .starts_with("not checked")
    );

    // The authority that issued the signer's certificate, or that certificate itself.
    for trust in [&authority, &cert] {
        let v = call("pdf_signatures", json!({"input": signed, "trust": trust}));
        let s = &v["details"][0];
        assert_eq!(
            (&s["valid"], &s["trusted"]),
            (&json!(true), &json!(true)),
            "{v}"
        );
    }
    // Somebody else's certificate vouches for nothing; the signature is still intact.
    let v = call(
        "pdf_signatures",
        json!({"input": signed, "trust": stranger}),
    );
    let s = &v["details"][0];
    assert_eq!(
        (&s["valid"], &s["trusted"]),
        (&json!(true), &json!(false)),
        "{v}"
    );
    assert!(
        s["trust"]
            .as_str()
            .unwrap()
            .contains("nothing leads from the certificate of Ada Signer"),
        "{v}"
    );
    let e = call_err("pdf_signatures", json!({"input": signed, "trust": pdf}));
    assert!(e.contains("is not a PEM certificate"), "{e}");
}

#[test]
fn a_signature_can_be_shown_on_the_page() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let (cert, key) = common::identity(dir.path(), "Ada Signer");
    let signed = dir.path().join("signed.pdf");
    // A box in the lower half of page 2, 200 by 60 points.
    call(
        "pdf_sign",
        json!({"input": pdf, "output": signed, "cert": cert, "key": key,
               "reason": "Approved", "visible": "2:300,600,500,660"}),
    );
    let v = call("pdf_signatures", json!({"input": signed}));
    assert_eq!(v["valid"], 1, "{v}");
    // It is drawn there and nowhere else, and says who signed and why.
    let rendered = call(
        "pdf_render",
        json!({"input": signed, "out_dir": dir.path().join("r"), "pages": "2", "dpi": 72}),
    );
    let page = image::open(rendered["files"][0]["file"].as_str().unwrap())
        .unwrap()
        .to_luma8();
    let dark = |x0: u32, y0: u32, x1: u32, y1: u32| {
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| page.get_pixel(x, y).0[0] < 140)
            .count()
    };
    assert!(
        dark(300, 600, 500, 660) > 150,
        "{}",
        dark(300, 600, 500, 660)
    );
    assert_eq!(dark(300, 500, 500, 595), 0);
    let fields = lopdf::Document::load(&signed).unwrap();
    let look = fields
        .objects
        .values()
        .filter_map(|o| o.as_stream().ok())
        .find(|s| s.dict.has(b"BBox"))
        .unwrap();
    let content = String::from_utf8_lossy(&look.content).into_owned();
    assert!(
        content.contains("(Digitally signed by Ada Signer) Tj")
            && content.contains("(Approved) Tj"),
        "{content}"
    );
    let e = call_err(
        "pdf_sign",
        json!({"input": pdf, "output": signed, "cert": cert, "key": key, "visible": "7:0,0,10,10"}),
    );
    assert!(e.contains("out of range"), "{e}");
}

/// A timestamp authority for one request: it states a fixed time about whatever digest
/// it is sent. Returns its address.
fn timestamp_authority(honest: bool, willing: bool) -> String {
    use std::io::{Read, Write};
    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        match content.len() {
            n if n < 128 => out.push(n as u8),
            n if n < 256 => out.extend([0x81, n as u8]),
            n => out.extend([0x82, (n >> 8) as u8, n as u8]),
        }
        out.extend_from_slice(content);
        out
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        // The request ends with the digest, a nonce and a flag: 32 + 10 + 3 bytes after "04 20".
        let digest = loop {
            let n = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..n]);
            let found = request
                .windows(2)
                .rposition(|w| w == [0x04, 0x20])
                .filter(|at| request.len() >= at + 2 + 32 + 13);
            if let Some(at) = found {
                break request[at + 2..at + 34].to_vec();
            }
            assert!(n > 0, "the request ended early");
        };
        let about = if honest { digest } else { vec![7; 32] };
        let algorithm = [
            0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
            0x00,
        ];
        let statement = tlv(
            0x30,
            &[
                vec![0x02, 0x01, 0x01],
                vec![0x06, 0x03, 0x2a, 0x03, 0x04],
                tlv(0x30, &[algorithm.to_vec(), tlv(0x04, &about)].concat()),
                vec![0x02, 0x01, 0x2a],
                tlv(0x18, b"20300102030405Z"),
            ]
            .concat(),
        );
        let signed = tlv(
            0x30,
            &[
                vec![0x02, 0x01, 0x03, 0x31, 0x00],
                tlv(
                    0x30,
                    &[
                        vec![
                            0x06, 0x0b, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x10, 0x01,
                            0x04,
                        ],
                        tlv(0xa0, &tlv(0x04, &statement)),
                    ]
                    .concat(),
                ),
                vec![0x31, 0x00],
            ]
            .concat(),
        );
        let token = tlv(
            0x30,
            &[
                vec![
                    0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02,
                ],
                tlv(0xa0, &signed),
            ]
            .concat(),
        );
        let body = if willing {
            tlv(0x30, &[vec![0x30, 0x03, 0x02, 0x01, 0x00], token].concat())
        } else {
            // Status 2: rejection.
            vec![0x30, 0x05, 0x30, 0x03, 0x02, 0x01, 0x02]
        };
        let mut reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/timestamp-reply\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        reply.extend_from_slice(&body);
        stream.write_all(&reply).unwrap();
    });
    address
}

#[test]
fn a_timestamp_authority_states_when_the_signature_was_made() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 1);
    let (cert, key) = common::identity(dir.path(), "Ada Signer");
    let signed = dir.path().join("signed.pdf");
    let v = call(
        "pdf_sign",
        json!({"input": pdf, "output": signed, "cert": cert, "key": key, "tsa": timestamp_authority(true, true)}),
    );
    assert_eq!(v["timestamped"], true, "{v}");
    let v = call("pdf_signatures", json!({"input": signed}));
    let s = &v["details"][0];
    assert_eq!(s["valid"], true, "{v}");
    assert_eq!(s["timestamp"]["time"], "D:20300102030405Z", "{v}");
    assert_eq!(s["timestamp"]["about_this_signature"], true, "{v}");

    // A token about something else is not embedded, and nothing is written.
    let other = dir.path().join("other.pdf");
    let e = call_err(
        "pdf_sign",
        json!({"input": pdf, "output": other, "cert": cert, "key": key, "tsa": timestamp_authority(false, true)}),
    );
    assert!(e.contains("about something else"), "{e}");
    assert!(!other.exists());
    // Nor is the document signed without the timestamp when the authority declines.
    let e = call_err(
        "pdf_sign",
        json!({"input": pdf, "output": other, "cert": cert, "key": key, "tsa": timestamp_authority(true, false)}),
    );
    assert!(e.contains("the request was refused"), "{e}");
    assert!(!other.exists());
}

#[test]
fn revoked_certificates_are_found_in_their_issuers_lists() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 1);
    let check = |revoked: bool| {
        let dir = tempfile::tempdir().unwrap();
        // One server per case: its address goes into the certificate, its list comes after.
        let (address, give) = common::serve_later();
        let (cert, key, authority, lists) =
            common::issued_identity_with_lists(dir.path(), "Ada Signer", Some(address));
        give(lists[usize::from(revoked)].clone());
        let signed = dir.path().join("signed.pdf");
        call(
            "pdf_sign",
            json!({"input": pdf, "output": signed, "cert": cert, "key": key}),
        );
        call(
            "pdf_signatures",
            json!({"input": signed, "trust": authority, "revocation": true}),
        )
    };
    let v = check(false);
    let s = &v["details"][0];
    assert_eq!(s["trusted"], true, "{v}");
    assert!(
        s["revocation"].as_str().unwrap().starts_with("none of"),
        "{v}"
    );
    let v = check(true);
    let s = &v["details"][0];
    assert_eq!(
        (&s["valid"], &s["trusted"]),
        (&json!(true), &json!(false)),
        "{v}"
    );
    assert_eq!(
        s["revocation"], "the certificate of Ada Signer was revoked",
        "{v}"
    );

    // A certificate that names no list leaves the question open, and says so.
    let (cert, key, authority) = common::issued_identity(dir.path(), "Bob Signer");
    let signed = dir.path().join("signed.pdf");
    call(
        "pdf_sign",
        json!({"input": pdf, "output": signed, "cert": cert, "key": key}),
    );
    let v = call(
        "pdf_signatures",
        json!({"input": signed, "trust": authority, "revocation": true}),
    );
    let s = &v["details"][0];
    assert_eq!(s["trusted"], true, "{v}");
    assert!(
        s["revocation"]
            .as_str()
            .unwrap()
            .contains("names no revocation list"),
        "{v}"
    );
}
