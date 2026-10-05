mod common;

use common::{call, call_err, form, page_count, sample, texts};
use serde_json::json;

#[test]
fn info_reports_structure_and_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 5);
    let v = call("pdf_info", json!({"input": pdf}));
    assert_eq!(v["pages"], 5);
    assert_eq!(v["metadata"]["title"], "Sample");
    assert_eq!(v["page_size_pt"], json!({"width": 612.0, "height": 792.0}));
    assert_eq!(v["uniform_page_size"], true);
    assert_eq!(v["encrypted"], false);
    assert_eq!(v["outline_entries"], 1);
    assert_eq!(v["form_fields"], 0);
}

#[test]
fn text_selects_pages_and_respects_budget() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 5);
    let v = call("pdf_text", json!({"input": pdf, "pages": "4,2"}));
    let pages = v["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0]["page"], 4);
    assert!(
        pages[0]["text"]
            .as_str()
            .unwrap()
            .contains("Page 4 of the sample")
    );
    assert!(pages[1]["text"].as_str().unwrap().contains("alpha-2"));
    assert!(v["resume_at_page"].is_null());

    let v = call("pdf_text", json!({"input": pdf, "max_chars": 50}));
    let pages = v["pages"].as_array().unwrap();
    assert_eq!(v["chars"], 50);
    assert_eq!(pages.last().unwrap()["truncated"], true);
    assert_eq!(v["resume_at_page"], pages.last().unwrap()["page"]);
    assert!(pages.len() < 5);
}

#[test]
fn search_finds_literals_and_regexes() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 5);
    let v = call("pdf_search", json!({"input": pdf, "query": "ALPHA-3"}));
    assert_eq!(v["total_matches"], 1);
    assert_eq!(v["matches"][0]["page"], 3);
    assert!(
        v["matches"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("Page 3")
    );

    let v = call(
        "pdf_search",
        json!({"input": pdf, "query": "ALPHA-3", "case_sensitive": true}),
    );
    assert_eq!(v["total_matches"], 0);

    let v = call(
        "pdf_search",
        json!({"input": pdf, "query": r"alpha-\d", "regex": true, "max_results": 2}),
    );
    assert_eq!(v["total_matches"], 5);
    assert_eq!(v["matches"].as_array().unwrap().len(), 2);

    // A literal query must not be interpreted as a pattern.
    let v = call("pdf_search", json!({"input": pdf, "query": "alpha-."}));
    assert_eq!(v["total_matches"], 0);
}

#[test]
fn outline_lists_entries() {
    let dir = tempfile::tempdir().unwrap();
    let v = call(
        "pdf_outline",
        json!({"input": sample(dir.path(), "a.pdf", 3)}),
    );
    assert_eq!(
        v["entries"],
        json!([{"level": 1, "title": "Second chapter", "page": 2}])
    );
    let v = call(
        "pdf_outline",
        json!({"input": sample(dir.path(), "b.pdf", 1)}),
    );
    assert_eq!(v["entries"], json!([]));
}

#[test]
fn render_writes_pngs_at_requested_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 3);
    let v = call(
        "pdf_render",
        json!({"input": pdf, "out_dir": dir.path().join("png"), "pages": "2", "dpi": 72}),
    );
    let file = &v["files"][0];
    assert_eq!(
        (
            file["page"].as_u64(),
            file["width"].as_u64(),
            file["height"].as_u64()
        ),
        (Some(2), Some(612), Some(792))
    );
    let bytes = std::fs::read(file["file"].as_str().unwrap()).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    // The page has dark text on white, so the image cannot be a single flat colour.
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut buf).unwrap();
    assert!(buf.chunks(4).any(|p| p[0] < 128), "expected text pixels");
}

#[test]
fn images_exports_embedded_images() {
    let dir = tempfile::tempdir().unwrap();
    let v = call(
        "pdf_images",
        json!({"input": form(dir.path()), "out_dir": dir.path().join("img")}),
    );
    assert_eq!(v["exported"], 1);
    assert_eq!(
        (
            v["images"][0]["width"].as_u64(),
            v["images"][0]["height"].as_u64()
        ),
        (Some(2), Some(2))
    );
    let bytes = std::fs::read(v["images"][0]["file"].as_str().unwrap()).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut buf).unwrap();
    assert_eq!(&buf[..12], [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);

    let v = call(
        "pdf_images",
        json!({"input": form(dir.path()), "out_dir": dir.path().join("img"), "min_size": 10}),
    );
    assert_eq!(v["exported"], 0);
}

#[test]
fn merge_concatenates_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (
        sample(dir.path(), "a.pdf", 5),
        sample(dir.path(), "b.pdf", 3),
    );
    let out = dir.path().join("merged.pdf");
    let v = call("pdf_merge", json!({"inputs": [a, b], "output": out}));
    assert_eq!(v["pages"], 8);
    let t = texts(&out);
    assert_eq!(t.len(), 8);
    assert!(t[4].contains("Page 5") && t[5].contains("Page 1") && t[7].contains("Page 3"));
    // Document info comes from the first input.
    assert_eq!(
        call("pdf_info", json!({"input": out}))["metadata"]["title"],
        "Sample"
    );
}

#[test]
fn merge_keeps_form_fields_of_every_input() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = form(dir.path());
    let filled = dir.path().join("filled.pdf");
    call(
        "pdf_fill",
        json!({"input": pdf, "output": filled, "values": {"name": "Ada"}}),
    );
    let out = dir.path().join("merged.pdf");
    call("pdf_merge", json!({"inputs": [pdf, filled], "output": out}));
    let v = call("pdf_forms", json!({"input": out}));
    let fields: Vec<_> = v["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["name"].as_str().unwrap(), &f["value"], &f["page"]))
        .collect();
    assert_eq!(fields.len(), 8);
    assert_eq!(fields[0], ("name", &json!(null), &json!(1)));
    // Same-named fields from the second input are namespaced, so they keep their own values.
    assert_eq!(fields[4], ("doc2.name", &json!("Ada"), &json!(2)));
}

#[test]
fn pages_keeps_reorders_duplicates_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 5);
    let out = dir.path().join("out.pdf");

    call(
        "pdf_pages",
        json!({"input": pdf, "output": out, "keep": "3,1,3"}),
    );
    let t = texts(&out);
    assert_eq!(t.len(), 3);
    assert!(t[0].contains("Page 3") && t[1].contains("Page 1") && t[2].contains("Page 3"));
    // Dropped pages must not linger in the file, e.g. kept alive by the outline.
    let raw = std::fs::read(&out).unwrap();
    assert!(
        !raw.windows(7).any(|w| w == b"alpha-2"),
        "content of a removed page is still in the file"
    );

    call(
        "pdf_pages",
        json!({"input": pdf, "output": out, "delete": "2-4"}),
    );
    let t = texts(&out);
    assert_eq!(t.len(), 2);
    assert!(t[0].contains("Page 1") && t[1].contains("Page 5"));

    assert!(call_err("pdf_pages", json!({"input": pdf, "output": out})).contains("keep"));
    assert!(
        call_err(
            "pdf_pages",
            json!({"input": pdf, "output": out, "delete": "all"})
        )
        .contains("no pages")
    );
    assert!(
        call_err(
            "pdf_pages",
            json!({"input": pdf, "output": out, "keep": "9"})
        )
        .contains("out of range")
    );
}

#[test]
fn split_by_count_and_by_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "doc.pdf", 5);
    let v = call(
        "pdf_split",
        json!({"input": pdf, "out_dir": dir.path().join("parts"), "every": 2}),
    );
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[2]["source_pages"], json!([5]));
    assert!(files[0]["file"].as_str().unwrap().ends_with("doc-0001.pdf"));
    assert!(texts(std::path::Path::new(files[1]["file"].as_str().unwrap()))[1].contains("Page 4"));

    let v = call(
        "pdf_split",
        json!({"input": pdf, "out_dir": dir.path().join("r"), "ranges": ["1-3", "5"]}),
    );
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(
        page_count(std::path::Path::new(files[0]["file"].as_str().unwrap())),
        3
    );
}

#[test]
fn rotate_adds_to_existing_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let out = dir.path().join("out.pdf");
    call(
        "pdf_rotate",
        json!({"input": pdf, "output": out, "angle": 90, "pages": "1"}),
    );
    let v = call("pdf_info", json!({"input": out}));
    assert_eq!(v["page_size_pt"], json!({"width": 792.0, "height": 612.0}));
    assert_eq!(v["uniform_page_size"], false);

    // In place, and back to upright.
    call(
        "pdf_rotate",
        json!({"input": out, "output": out, "angle": -90, "pages": "1"}),
    );
    assert_eq!(
        call("pdf_info", json!({"input": out}))["uniform_page_size"],
        true
    );
    assert!(
        call_err(
            "pdf_rotate",
            json!({"input": pdf, "output": out, "angle": 45})
        )
        .contains("multiple of 90")
    );
}

#[test]
fn stamp_draws_page_numbers_and_watermarks() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 3);
    let out = dir.path().join("out.pdf");
    call(
        "pdf_stamp",
        json!({"input": pdf, "output": out, "text": "{page}/{pages}", "position": "footer"}),
    );
    let t = texts(&out);
    assert!(t[1].contains("2/3") && t[1].contains("Page 2"), "{t:?}");

    let v = call(
        "pdf_stamp",
        json!({"input": pdf, "output": out, "text": "DRAFT (v2)", "pages": "1"}),
    );
    assert_eq!(v["stamped_pages"], json!([1]));
    let t = texts(&out);
    assert!(t[0].contains("DRAFT (v2)") && !t[1].contains("DRAFT"));

    assert!(
        call_err(
            "pdf_stamp",
            json!({"input": pdf, "output": out, "text": "\u{4f60}"})
        )
        .contains("Latin-1")
    );
    assert!(
        call_err(
            "pdf_stamp",
            json!({"input": pdf, "output": out, "text": "x", "opacity": 2})
        )
        .contains("opacity")
    );
}

#[test]
fn stamping_twice_keeps_the_first_stamps_resources() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    call(
        "pdf_stamp",
        json!({"input": sample(dir.path(), "a.pdf", 1), "output": out, "text": "DRAFT", "opacity": 0.25}),
    );
    call(
        "pdf_stamp",
        json!({"input": out, "output": out, "text": "1", "position": "footer"}),
    );
    // The footer must not replace the graphics state the watermark's content refers to.
    let doc = lopdf::Document::load(&out).unwrap();
    let page = doc.get_pages()[&1];
    let res = doc
        .get_dictionary(page)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .as_dict()
        .unwrap();
    let states = res.get(b"ExtGState").unwrap().as_dict().unwrap();
    let mut alphas: Vec<f32> = states
        .iter()
        .map(|(_, v)| {
            doc.get_dictionary(v.as_reference().unwrap())
                .unwrap()
                .get(b"ca")
                .unwrap()
                .as_float()
                .unwrap()
        })
        .collect();
    alphas.sort_by(f32::total_cmp);
    assert_eq!(alphas, [0.25, 1.0]);
    assert_eq!(res.get(b"Font").unwrap().as_dict().unwrap().len(), 3);
}

/// Dark pixel counts inside page rectangles given in PDF points, at 72 dpi.
fn ink<const N: usize>(
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

#[test]
fn filled_text_is_visible_when_rendered() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = form(dir.path());
    let (name, notes_top, notes_rest) = (
        [72, 600, 300, 620],
        [72, 474, 300, 500],
        [72, 400, 300, 474],
    );
    assert_eq!(
        ink(&pdf, dir.path(), [name, notes_top, notes_rest]),
        [0, 0, 0]
    );

    let out = dir.path().join("filled.pdf");
    let long = "first line\nsecond paragraph that is long enough to need wrapping inside the field";
    call(
        "pdf_fill",
        json!({"input": pdf, "output": out, "values": {"name": "Ada", "notes": long}}),
    );
    // Multiline fields wrap: the newline and the long paragraph give at least three lines.
    let seen = ink(&out, dir.path(), [name, notes_top, notes_rest]);
    assert!(seen.iter().all(|&n| n > 20), "{seen:?}");
    let raw = std::fs::read(&out).unwrap();
    assert!(raw.windows(3).filter(|w| w == b"T*\n").count() >= 2);
}

#[test]
fn set_meta_updates_and_removes_fields() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 1);
    let out = dir.path().join("out.pdf");
    call(
        "pdf_set_meta",
        json!({"input": pdf, "output": out, "title": "Zażółć gęślą", "author": ""}),
    );
    let meta = &call("pdf_info", json!({"input": out}))["metadata"];
    assert_eq!(meta["title"], "Zażółć gęślą");
    assert!(meta.get("author").is_none());
    assert!(
        call_err("pdf_set_meta", json!({"input": pdf, "output": out})).contains("nothing to set")
    );
}

#[test]
fn compress_never_grows_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 40);
    let out = dir.path().join("out.pdf");
    let v = call("pdf_compress", json!({"input": pdf, "output": out}));
    assert!(v["size_after"].as_u64() < v["size_before"].as_u64(), "{v}");
    assert_eq!(
        v["size_after"].as_u64().unwrap(),
        std::fs::metadata(&out).unwrap().len()
    );
    assert!(texts(&out)[39].contains("Page 40"));

    // Already compact: the output must be no larger than the input.
    let again = dir.path().join("again.pdf");
    let v = call("pdf_compress", json!({"input": out, "output": again}));
    assert!(v["size_after"].as_u64() <= v["size_before"].as_u64());
    assert_eq!(page_count(&again), 40);
}

#[test]
fn encrypt_and_decrypt_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let locked = dir.path().join("locked.pdf");
    call(
        "pdf_encrypt",
        json!({"input": pdf, "output": locked, "user_password": "open", "owner_password": "own"}),
    );
    let raw = std::fs::read(&locked).unwrap();
    assert!(
        !raw.windows(7).any(|w| w == b"alpha-1"),
        "content is readable without the password"
    );

    assert!(call_err("pdf_info", json!({"input": locked})).contains("encrypted"));
    assert!(
        call_err("pdf_info", json!({"input": locked, "password": "nope"})).contains("encrypted")
    );
    assert_eq!(
        call("pdf_info", json!({"input": locked, "password": "open"}))["encrypted"],
        true
    );
    let v = call(
        "pdf_text",
        json!({"input": locked, "password": "open", "pages": "2"}),
    );
    assert!(v["pages"][0]["text"].as_str().unwrap().contains("alpha-2"));

    let open = dir.path().join("open.pdf");
    call(
        "pdf_decrypt",
        json!({"input": locked, "output": open, "password": "own"}),
    );
    assert_eq!(call("pdf_info", json!({"input": open}))["encrypted"], false);
    assert!(texts(&open)[0].contains("alpha-1"));
    assert!(
        call_err("pdf_decrypt", json!({"input": pdf, "output": open})).contains("not encrypted")
    );

    // Removing the encryption dictionary must not leave a stale object count behind.
    let doc = lopdf::Document::load(&open).unwrap();
    let highest = doc.objects.keys().map(|id| id.0).max().unwrap();
    assert_eq!(
        doc.trailer.get(b"Size").unwrap().as_i64().unwrap(),
        highest as i64 + 1
    );
}

#[test]
fn owner_only_encryption_opens_without_a_password() {
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked.pdf");
    call(
        "pdf_encrypt",
        json!({"input": sample(dir.path(), "a.pdf", 1), "output": locked, "owner_password": "own", "deny_copy": true}),
    );
    assert_eq!(
        call("pdf_info", json!({"input": locked}))["encrypted"],
        true
    );
    assert!(texts(&locked)[0].contains("alpha-1"));
}

#[test]
fn forms_lists_and_fills_fields() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = form(dir.path());
    let v = call("pdf_forms", json!({"input": pdf}));
    let fields = v["fields"].as_array().unwrap();
    assert_eq!(fields.len(), 4);
    assert_eq!(
        fields[0],
        json!({"name": "name", "type": "text", "value": null, "options": [], "read_only": false, "page": 1})
    );
    assert_eq!(fields[1]["type"], "checkbox");
    assert_eq!(fields[1]["options"], json!(["Yes"]));
    assert_eq!(fields[2]["options"], json!(["red", "green"]));

    let out = dir.path().join("filled.pdf");
    call(
        "pdf_fill",
        json!({"input": pdf, "output": out, "values": {"name": "Ada Lovelace", "agree": "true", "color": "green"}}),
    );
    let v = call("pdf_forms", json!({"input": out}));
    let values: Vec<_> = v["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["value"].clone())
        .collect();
    assert_eq!(
        values,
        [
            json!("Ada Lovelace"),
            json!("Yes"),
            json!("green"),
            json!(null)
        ]
    );
    // The generated appearance makes the value visible to renderers and text extraction alike.
    let raw = std::fs::read(&out).unwrap();
    assert!(raw.windows(14).any(|w| w == b"(Ada Lovelace)"));

    call(
        "pdf_fill",
        json!({"input": out, "output": out, "values": {"agree": "false"}}),
    );
    assert_eq!(
        call("pdf_forms", json!({"input": out}))["fields"][1]["value"],
        "Off"
    );

    let e = call_err(
        "pdf_fill",
        json!({"input": pdf, "output": out, "values": {"nmae": "x"}}),
    );
    assert!(e.contains("available: name, agree, color, notes"), "{e}");
    let e = call_err(
        "pdf_fill",
        json!({"input": pdf, "output": out, "values": {"color": "blue"}}),
    );
    assert!(e.contains("options: red, green"), "{e}");
    let e = call_err(
        "pdf_fill",
        json!({"input": sample(dir.path(), "a.pdf", 1), "output": out, "values": {"a": "b"}}),
    );
    assert!(e.contains("no form fields"), "{e}");
}

#[test]
fn bad_inputs_produce_clear_errors() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.pdf");
    assert!(call_err("pdf_info", json!({"input": missing})).contains("cannot read"));
    let junk = dir.path().join("junk.pdf");
    std::fs::write(&junk, b"not a pdf").unwrap();
    assert!(call_err("pdf_info", json!({"input": junk})).contains("cannot open"));
    // Unknown arguments are rejected rather than silently ignored.
    assert!(call_err("pdf_info", json!({"input": junk, "pagez": "1"})).contains("unknown field"));
    assert!(call_err("pdf_nope", json!({})).contains("unknown tool"));
}
