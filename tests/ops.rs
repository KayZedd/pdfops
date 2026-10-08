mod common;

use common::{
    call, call_err, form, image_stream, ink, ocr_lang, page_count, sample, scan, texts, with_images,
};
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
fn text_resumes_inside_a_page_larger_than_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let whole = texts(&pdf);
    // A budget smaller than either page: following the two markers reads all of both.
    let (mut page, mut from) = (json!(1), json!(0));
    let mut read = vec![String::new(); 2];
    let mut calls = 0;
    while !page.is_null() {
        let v = call(
            "pdf_text",
            json!({"input": pdf, "pages": format!("{page}-"), "max_chars": 9, "from_char": from}),
        );
        assert_eq!(
            v["chars"],
            9.min(whole.concat().len() - read.concat().len())
        );
        for p in v["pages"].as_array().unwrap() {
            read[p["page"].as_u64().unwrap() as usize - 1] += p["text"].as_str().unwrap();
        }
        (page, from) = (v["resume_at_page"].clone(), v["resume_at_char"].clone());
        calls += 1;
        assert!(calls < 20, "reading does not advance: {v}");
    }
    assert_eq!(read, whole);
    assert!(calls > 4);

    // The characters skipped are those of the first page read only.
    let v = call(
        "pdf_text",
        json!({"input": pdf, "from_char": 5, "max_chars": 1000}),
    );
    assert_eq!(v["pages"][0]["text"], whole[0][5..]);
    assert_eq!(v["pages"][0]["from_char"], 5);
    assert_eq!(v["pages"][1]["text"], whole[1]);
    assert!(v["resume_at_page"].is_null() && v["resume_at_char"].is_null());
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
fn editing_a_protected_file_keeps_it_protected() {
    let dir = tempfile::tempdir().unwrap();
    let plain = sample(dir.path(), "a.pdf", 3);
    let locked = dir.path().join("locked.pdf");
    call(
        "pdf_encrypt",
        json!({"input": plain, "output": locked, "user_password": "open", "owner_password": "own"}),
    );
    let out = dir.path().join("out.pdf");
    let still_locked = |path: &std::path::Path, expect: &str| {
        assert!(
            call_err("pdf_info", json!({"input": path})).contains("encrypted"),
            "opens without a password"
        );
        let v = call("pdf_text", json!({"input": path, "password": "open"}));
        let text: String = v["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["text"].as_str().unwrap())
            .collect();
        assert!(text.contains(expect), "{text}");
        let raw = std::fs::read(path).unwrap();
        assert!(
            !raw.windows(6).any(|w| w == b"alpha-"),
            "content is readable in the file"
        );
    };

    call(
        "pdf_rotate",
        json!({"input": locked, "output": out, "angle": 90, "password": "open"}),
    );
    still_locked(&out, "alpha-3");
    call(
        "pdf_pages",
        json!({"input": locked, "output": out, "keep": "2", "password": "open"}),
    );
    still_locked(&out, "alpha-2");
    call(
        "pdf_stamp",
        json!({"input": locked, "output": out, "text": "MARK", "position": "footer", "password": "open"}),
    );
    still_locked(&out, "MARK");
    call(
        "pdf_compress",
        json!({"input": locked, "output": out, "password": "open"}),
    );
    still_locked(&out, "alpha-1");
    call(
        "pdf_redact",
        json!({"input": locked, "output": out, "texts": ["alpha-1"], "password": "open"}),
    );
    still_locked(&out, "alpha-2");
    // Mixing in an unprotected file must not strip the protection from the rest.
    call(
        "pdf_merge",
        json!({"inputs": [plain, locked], "output": out, "password": "open"}),
    );
    still_locked(&out, "alpha-3");

    // Removing protection stays an explicit act.
    call(
        "pdf_decrypt",
        json!({"input": locked, "output": out, "password": "open"}),
    );
    assert_eq!(call("pdf_info", json!({"input": out}))["encrypted"], false);
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

/// A one page file whose parts are given as numbered objects, with a correct
/// cross-reference table unless `shift` moves the table's offsets off their objects.
fn handmade(path: &std::path::Path, objects: &[&str], shift: usize) {
    let mut body = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(body.len());
        body.push_str(object);
    }
    let xref = body.len();
    body.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for offset in offsets {
        body.push_str(&format!("{:010} 00000 n \n", offset + shift));
    }
    body.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{}\n%%EOF\n",
        objects.len() + 1,
        xref + shift
    ));
    std::fs::write(path, body).unwrap();
}

#[test]
fn damaged_files_that_are_encrypted_are_rebuilt_and_stay_protected() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 2);
    let locked = dir.path().join("locked.pdf");
    call(
        "pdf_encrypt",
        json!({"input": pdf, "output": locked, "user_password": "open", "owner_password": "own"}),
    );
    let sound = std::fs::read(&locked).unwrap();
    // Two kinds of damage: a stream that no longer states its length, which the strict
    // reader opens with that page's content read as empty, and a pointer to the cross-reference table
    // that leads beside it, which it does not open at all.
    let length = regex::bytes::Regex::new(r"/Length \d+").unwrap();
    let unsized_stream = length
        .replace(&sound, |found: &regex::bytes::Captures| {
            vec![b' '; found[0].len()]
        })
        .into_owned();
    let start = regex::bytes::Regex::new(r"startxref\s+(\d+)").unwrap();
    let shifted = start
        .replace(&sound, |found: &regex::bytes::Captures| {
            let offset: usize = std::str::from_utf8(&found[1]).unwrap().parse().unwrap();
            format!("startxref\n{}", offset + 7).into_bytes()
        })
        .into_owned();
    assert!(unsized_stream != sound && shifted != sound);

    for (name, bytes) in [("unsized.pdf", unsized_stream), ("shifted.pdf", shifted)] {
        let damaged = dir.path().join(name);
        let out = dir.path().join(format!("out-{name}"));
        std::fs::write(&damaged, bytes).unwrap();
        let v = call(
            "pdf_rotate",
            json!({"input": damaged, "output": out, "angle": 90, "password": "open"}),
        );
        assert_eq!(v["repaired_inputs"], json!([damaged]), "{name}: {v}");

        // The output is sound and locked as the input was: closed without a password,
        // open with either of the two it had.
        let raw = std::fs::read(&out).unwrap();
        assert!(!raw.windows(7).any(|w| w == b"alpha-1"), "{name}");
        assert!(
            call_err("pdf_info", json!({"input": out})).contains("encrypted"),
            "{name}"
        );
        for password in ["open", "own"] {
            let v = call("pdf_text", json!({"input": out, "password": password}));
            assert!(
                v["pages"][1]["text"].as_str().unwrap().contains("alpha-2"),
                "{name} with {password}: {v}"
            );
        }
        let again = call(
            "pdf_rotate",
            json!({"input": out, "output": out, "angle": 90, "password": "open"}),
        );
        assert!(again.get("repaired_inputs").is_none(), "{name}: {again}");

        // The wrong password opens a damaged file no more than a sound one.
        assert!(
            call_err(
                "pdf_rotate",
                json!({"input": damaged, "output": out, "angle": 90, "password": "nope"})
            )
            .contains("encrypted"),
            "{name}"
        );
    }
}

const CATALOG: &str = "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
const TREE: &str =
    "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>\nendobj\n";
const PAGE: &str = "3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n";
const CONTENT: &str = "4 0 obj\n<< /Length 43 >>\nstream\nBT /F1 18 Tf 72 720 Td (Fragile text) Tj ET\nendstream\nendobj\n";
const FONT: &str = "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n";
const INFO: &str = "6 0 obj\n<< /Title (Kept title) >>\nendobj\n";

#[test]
fn damaged_files_are_rebuilt_for_writing() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    // The same document damaged in three ways: a content stream that does not state its
    // length, a cross-reference table pointing beside every object, and a catalog naming a
    // page tree that is not there, so that the page is only found by looking for it and has
    // nothing to inherit its size from.
    let unsized_stream = CONTENT.replace("/Length 43", "");
    let lost_tree = "1 0 obj\n<< /Type /Catalog /Pages 9 0 R >>\nendobj\n";
    let cases: [(&str, Vec<&str>, usize); 3] = [
        (
            "unsized.pdf",
            vec![CATALOG, TREE, PAGE, &unsized_stream, FONT, INFO],
            0,
        ),
        (
            "shifted.pdf",
            vec![CATALOG, TREE, PAGE, CONTENT, FONT, INFO],
            7,
        ),
        (
            "treeless.pdf",
            vec![lost_tree, TREE, PAGE, CONTENT, FONT, INFO],
            0,
        ),
    ];
    for (name, objects, shift) in cases {
        let pdf = dir.path().join(name);
        handmade(&pdf, &objects, shift);
        // Reading goes through the repairing parser and works.
        assert!(texts(&pdf)[0].contains("Fragile text"), "{name}");

        // Writing rebuilds the document from that same view, and says that it did.
        let v = call(
            "pdf_rotate",
            json!({"input": pdf, "output": out, "angle": 90}),
        );
        assert_eq!(v["repaired_inputs"], json!([pdf]), "{name}: {v}");
        // The output is sound: the strict reader takes it, and nothing in it is damaged.
        let written = lopdf::Document::load(&out).unwrap();
        assert_eq!(written.get_pages().len(), 1, "{name}");
        assert!(texts(&out)[0].contains("Fragile text"), "{name}");
        let info = call("pdf_info", json!({"input": out}));
        assert_eq!(
            (
                &info["pages"],
                &info["page_size_pt"]["width"],
                &info["metadata"]["title"]
            ),
            (&json!(1), &json!(792.0), &json!("Kept title")),
            "{name}: {info}"
        );
        let again = call(
            "pdf_rotate",
            json!({"input": out, "output": out, "angle": 90}),
        );
        assert!(again.get("repaired_inputs").is_none(), "{name}: {again}");

        let v = call("pdf_merge", json!({"inputs": [pdf, pdf], "output": out}));
        assert_eq!(v["repaired_inputs"], json!([pdf]), "{name}");
        assert_eq!(texts(&out).len(), 2, "{name}");
        call("pdf_compress", json!({"input": pdf, "output": out}));
        assert!(texts(&out)[0].contains("Fragile text"), "{name}");
        // Text is found and removed in a rebuilt document like in any other.
        call(
            "pdf_redact",
            json!({"input": pdf, "output": out, "texts": ["Fragile"]}),
        );
        assert!(
            !texts(&out)[0].contains("Fragile") && texts(&out)[0].contains("text"),
            "{name}"
        );
    }
    // A sound file is not touched by any of this.
    let sound = dir.path().join("sound.pdf");
    handmade(&sound, &[CATALOG, TREE, PAGE, CONTENT, FONT, INFO], 0);
    let v = call(
        "pdf_rotate",
        json!({"input": sound, "output": out, "angle": 90}),
    );
    assert!(v.get("repaired_inputs").is_none(), "{v}");

    // What cannot be rebuilt is still refused, with the reason.
    let junk = dir.path().join("junk.pdf");
    std::fs::write(&junk, b"%PDF-1.4\nnothing of use\n%%EOF\n").unwrap();
    let e = call_err(
        "pdf_rotate",
        json!({"input": junk, "output": out, "angle": 90}),
    );
    assert!(e.contains("cannot open"), "{e}");
}

#[test]
fn sparse_object_numbers_are_compacted_on_save() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 1);
    let mut doc = lopdf::Document::load(&pdf).unwrap();
    // Move the page far away in the numbering, as some producers and fuzzers do.
    let page = doc.get_pages()[&1];
    let far = (90_000, 0);
    let dict = doc.objects.remove(&page).unwrap();
    doc.objects.insert(far, dict);
    for object in doc.objects.values_mut() {
        if let Ok(d) = object.as_dict_mut()
            && let Ok(kids) = d.get_mut(b"Kids").and_then(|k| k.as_array_mut())
        {
            kids.iter_mut()
                .for_each(|k| *k = lopdf::Object::Reference(far));
        }
    }
    doc.max_id = 90_000;
    doc.save(&pdf).unwrap();

    let out = dir.path().join("out.pdf");
    call(
        "pdf_stamp",
        json!({"input": pdf, "output": out, "text": "X", "position": "footer"}),
    );
    let written = lopdf::Document::load(&out).unwrap();
    assert!(written.objects.keys().map(|id| id.0).max().unwrap() < 100);
    assert!(texts(&out)[0].contains("alpha-1"));
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

#[test]
fn info_counts_form_fields_like_forms_does() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = form(dir.path());
    let listed = call("pdf_forms", json!({"input": pdf}))["fields"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(
        call("pdf_info", json!({"input": pdf}))["form_fields"],
        listed
    );
}

#[test]
fn stamp_and_fill_draw_text_outside_latin1() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    let text = "Zażółć gęślą {page}";
    let stamped = pdfops::tools::call(
        "pdf_stamp",
        json!({"input": sample(dir.path(), "a.pdf", 2), "output": out, "text": text, "position": "footer"}),
    );
    if let Err(e) = &stamped {
        // Needs any installed font with Polish letters; a bare container may have none.
        assert!(e.to_string().contains("no installed font"), "{e:#}");
        eprintln!("skipped: {e}");
        return;
    }
    // The embedded font carries a ToUnicode map, so the stamp is extractable and searchable.
    assert!(
        texts(&out)[1].contains("Zażółć gęślą 2"),
        "{:?}",
        texts(&out)
    );
    assert_eq!(
        call("pdf_search", json!({"input": out, "query": "gęślą"}))["total_matches"],
        2
    );
    // Only the glyphs in use are embedded, not the whole font.
    assert!(std::fs::metadata(&out).unwrap().len() < 40_000);

    let filled = dir.path().join("filled.pdf");
    call(
        "pdf_fill",
        json!({"input": form(dir.path()), "output": filled, "values": {"name": "Zażółć"}}),
    );
    assert_eq!(
        call("pdf_forms", json!({"input": filled}))["fields"][0]["value"],
        "Zażółć"
    );
    assert!(ink(&filled, dir.path(), [[72, 600, 300, 620]])[0] > 20);
}

#[test]
fn outline_follows_pages_through_pages_split_and_merge() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 4);
    let out = dir.path().join("out.pdf");
    let entries =
        |path: &std::path::Path| call("pdf_outline", json!({"input": path}))["entries"].clone();

    // The bookmark targets source page 2, which becomes page 1 here.
    call(
        "pdf_pages",
        json!({"input": pdf, "output": out, "keep": "2-3"}),
    );
    assert_eq!(
        entries(&out),
        json!([{"level": 1, "title": "Second chapter", "page": 1}])
    );

    // Its target is gone, so the bookmark goes too.
    call(
        "pdf_pages",
        json!({"input": pdf, "output": out, "delete": "2"}),
    );
    assert_eq!(entries(&out), json!([]));

    call("pdf_merge", json!({"inputs": [pdf, pdf], "output": out}));
    let merged = entries(&out);
    assert_eq!(
        merged
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["page"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [2, 6]
    );

    let v = call(
        "pdf_split",
        json!({"input": pdf, "out_dir": dir.path().join("parts"), "every": 2}),
    );
    let first = std::path::Path::new(v["files"][0]["file"].as_str().unwrap()).to_path_buf();
    assert_eq!(entries(&first)[0]["page"], 2);
}

#[test]
fn outline_keeps_nesting_and_promotes_orphans() {
    use lopdf::{Document, Object, dictionary};
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 3);
    // Replace the fixture's outline with: A (page 1) > B (page 2) > C (page 3).
    let mut doc = Document::load(&pdf).unwrap();
    let pages: Vec<_> = doc.get_pages().into_values().collect();
    let (root, a, b, c) = (
        doc.new_object_id(),
        doc.new_object_id(),
        doc.new_object_id(),
        doc.new_object_id(),
    );
    let item = |title: &str, parent, page, child: Option<lopdf::ObjectId>| {
        let mut d = dictionary! {
            "Title" => Object::string_literal(title), "Parent" => parent,
            "Dest" => vec![Object::Reference(page), "Fit".into()],
        };
        if let Some(child) = child {
            d.set("First", child);
            d.set("Last", child);
            d.set("Count", 1);
        }
        Object::Dictionary(d)
    };
    doc.objects.insert(a, item("A", root, pages[0], Some(b)));
    doc.objects.insert(b, item("B", a, pages[1], Some(c)));
    doc.objects.insert(c, item("C", b, pages[2], None));
    doc.objects.insert(
        root,
        Object::Dictionary(
            dictionary! { "Type" => "Outlines", "First" => a, "Last" => a, "Count" => 1 },
        ),
    );
    doc.catalog_mut().unwrap().set("Outlines", root);
    doc.save(&pdf).unwrap();

    let out = dir.path().join("out.pdf");
    let levels = |keep: &str| {
        call(
            "pdf_pages",
            json!({"input": pdf, "output": out, "keep": keep}),
        );
        let v = call("pdf_outline", json!({"input": out}));
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["title"].as_str().unwrap().to_string(),
                    e["level"].as_u64().unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        levels("1-3"),
        [
            ("A".to_string(), 1),
            ("B".to_string(), 2),
            ("C".to_string(), 3)
        ]
    );
    // B's page is dropped, so C moves up under A.
    assert_eq!(levels("1,3"), [("A".to_string(), 1), ("C".to_string(), 2)]);
}

#[test]
fn images_decodes_indexed_and_fax_encoded_images() {
    let dir = tempfile::tempdir().unwrap();
    // A 2x2 palette image: red, blue / blue, red.
    let palette = lopdf::Object::Array(vec![
        "Indexed".into(),
        "DeviceRGB".into(),
        1.into(),
        lopdf::Object::String(vec![255, 0, 0, 0, 0, 255], lopdf::StringFormat::Hexadecimal),
    ]);
    let indexed = image_stream(2, 2, palette, 8, vec![0, 1, 1, 0]);
    // 64x32 CCITT Group 4: white with a black rectangle from (8,8) to (40,20) inclusive.
    let mut fax = image_stream(
        64,
        32,
        "DeviceGray".into(),
        1,
        vec![
            0xff, 0x33, 0x06, 0xbf, 0xff, 0xff, 0xff, 0xff, 0x8f, 0xff, 0x00, 0x10, 0x01,
        ],
    );
    fax.dict.set("Filter", "CCITTFaxDecode");
    fax.dict.set(
        "DecodeParms",
        lopdf::dictionary! { "K" => -1, "Columns" => 64, "Rows" => 32 },
    );
    let pdf = with_images(dir.path(), "img.pdf", vec![indexed, fax]);

    let v = call(
        "pdf_images",
        json!({"input": pdf, "out_dir": dir.path().join("img")}),
    );
    assert_eq!(v["exported"], 2, "{v}");
    let decode = |i: usize| {
        let bytes = std::fs::read(v["images"][i]["file"].as_str().unwrap()).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        (info.width, info.height, buf)
    };
    let mut seen: Vec<_> = (0..2).map(decode).collect();
    seen.sort_by_key(|s| s.0);
    assert_eq!(
        seen[0],
        (2, 2, vec![255, 0, 0, 0, 0, 255, 0, 0, 255, 255, 0, 0])
    );
    let (w, h, fax) = &seen[1];
    assert_eq!((*w, *h), (64, 32));
    let channels = fax.len() / (64 * 32);
    let dark = |x: usize, y: usize| fax[(y * 64 + x) * channels] < 128;
    assert!(dark(8, 8) && dark(40, 20) && dark(24, 14));
    assert!(!dark(7, 8) && !dark(41, 20) && !dark(24, 21) && !dark(0, 0));
}

#[test]
fn compress_can_downscale_and_reencode_images() {
    let dir = tempfile::tempdir().unwrap();
    // A smooth 256x256 colour gradient, stored raw like a scanner's lossless output.
    let pixels: Vec<u8> = (0..256u32)
        .flat_map(|y| (0..256u32).flat_map(move |x| [x as u8, y as u8, ((x + y) / 2) as u8]))
        .collect();
    let pdf = with_images(
        dir.path(),
        "photo.pdf",
        vec![image_stream(256, 256, "DeviceRGB".into(), 8, pixels)],
    );
    let out = dir.path().join("out.pdf");

    let v = call(
        "pdf_compress",
        json!({"input": pdf, "output": out, "max_image_edge": 64, "image_quality": 70}),
    );
    assert_eq!(v["images_recompressed"], 1, "{v}");
    assert!(
        v["size_after"].as_u64().unwrap() * 10 < v["size_before"].as_u64().unwrap(),
        "{v}"
    );
    let img = call(
        "pdf_images",
        json!({"input": out, "out_dir": dir.path().join("img")}),
    );
    assert_eq!(
        (
            img["images"][0]["width"].as_u64(),
            img["images"][0]["height"].as_u64()
        ),
        (Some(64), Some(64))
    );
    assert!(img["images"][0]["file"].as_str().unwrap().ends_with(".jpg"));
    // The picture still shows: the page is not blank where it is drawn.
    assert!(ink(&out, dir.path(), [[50, 600, 150, 700]])[0] > 500);

    // Without the lossy options images are left alone.
    let v = call("pdf_compress", json!({"input": pdf, "output": out}));
    assert_eq!(v["images_recompressed"], 0);
    let img = call(
        "pdf_images",
        json!({"input": out, "out_dir": dir.path().join("img2")}),
    );
    assert_eq!(img["images"][0]["width"], 256);
    assert!(
        call_err(
            "pdf_compress",
            json!({"input": pdf, "output": out, "image_quality": 0})
        )
        .contains("between 1 and 100")
    );
}

#[test]
fn bookmarks_and_named_links_land_where_they_did() {
    use lopdf::{Object, dictionary};
    let dir = tempfile::tempdir().unwrap();
    let pdf = sample(dir.path(), "a.pdf", 3);
    let mut doc = lopdf::Document::load(&pdf).unwrap();
    let pages: Vec<_> = doc.get_pages().into_values().collect();
    // The bookmark goes to a height on page 2, not just to the page.
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
    doc.get_dictionary_mut(item).unwrap().set(
        "Dest",
        vec![Object::Reference(pages[1]), "FitH".into(), 500.into()],
    );
    // Two links on page 1 go to places that have names: one named in the old way, in
    // the catalog's own dictionary, the other in the name tree and through an action.
    let tree = doc.add_object(dictionary! {
        "Names" => vec![
            Object::string_literal("figure"),
            vec![Object::Reference(pages[1]), "XYZ".into(), 30.into(), 400.into(), Object::Null].into(),
        ],
    });
    let catalog = doc.get_dictionary_mut(catalog).unwrap();
    catalog.set(
        "Dests",
        dictionary! {
            "chapter" => dictionary! {
                "D" => vec![Object::Reference(pages[2]), "XYZ".into(), 10.into(), 700.into(), Object::Null],
            },
        },
    );
    catalog.set("Names", dictionary! { "Dests" => tree });
    let by_name = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Link",
        "Rect" => vec![72.into(), 700.into(), 172.into(), 720.into()],
        "Dest" => "chapter",
    });
    let by_action = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Link",
        "Rect" => vec![72.into(), 600.into(), 172.into(), 620.into()],
        "A" => dictionary! { "S" => "GoTo", "D" => Object::string_literal("figure") },
    });
    doc.get_dictionary_mut(pages[0]).unwrap().set(
        "Annots",
        vec![Object::Reference(by_name), Object::Reference(by_action)],
    );
    doc.save(&pdf).unwrap();

    // Pages reordered, in a merge with another file that uses the same name.
    let reordered = dir.path().join("reordered.pdf");
    call(
        "pdf_pages",
        json!({"input": pdf, "keep": "3,1,2", "output": reordered}),
    );
    let merged = dir.path().join("merged.pdf");
    call(
        "pdf_merge",
        json!({"inputs": [sample(dir.path(), "b.pdf", 2), reordered], "output": merged}),
    );
    let v = call("pdf_outline", json!({"input": merged}));
    let titles: Vec<(&str, u64)> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["title"].as_str().unwrap(), o["page"].as_u64().unwrap()))
        .collect();
    assert_eq!(titles, [("Second chapter", 2), ("Second chapter", 5)]);
    // The links still lead to their pages, now given outright.
    let v = call("pdf_annotations", json!({"input": merged}));
    let targets: Vec<u64> = v["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["target_page"].as_u64().unwrap())
        .collect();
    assert_eq!(targets, [3, 5], "{v}");
    let out = lopdf::Document::load(&merged).unwrap();
    let views: Vec<Vec<Object>> = out
        .objects
        .values()
        .filter_map(|o| o.as_dict().ok())
        .filter_map(|d| {
            let to = d
                .get(b"Dest")
                .ok()
                .or_else(|| d.get(b"A").ok()?.as_dict().ok()?.get(b"D").ok())?;
            Some(to.as_array().ok()?[1..].to_vec())
        })
        .collect();
    for view in [
        vec!["FitH".into(), 500.into()],
        vec!["XYZ".into(), 10.into(), 700.into(), Object::Null],
        vec!["XYZ".into(), 30.into(), 400.into(), Object::Null],
    ] {
        assert!(views.contains(&view), "{view:?} not in {views:?}");
    }
}

#[test]
fn recognised_words_become_an_invisible_text_layer() {
    use pdfops::ops::ocr::{add_text_layer, parse_tsv};
    let dir = tempfile::tempdir().unwrap();
    let pdf = common::custom(dir.path(), "blank.pdf", &["0.9 g 0 0 612 792 re f"]);
    // What tesseract reports for a letter page at 144 dpi, two pixels to the point.
    let rows = [
        "level page_num block_num par_num line_num word_num left top width height conf text",
        "1 1 0 0 0 0 0 0 1224 1584 -1 ",
        "2 1 1 0 0 0 200 300 480 140 -1 ",
        "3 1 1 1 0 0 200 300 480 40 -1 ",
        "4 1 1 1 1 0 200 300 480 40 -1 ",
        "5 1 1 1 1 1 200 300 220 40 96.1 Invoice",
        "5 1 1 1 1 2 440 300 240 40 95.3 4471",
        "3 1 1 2 0 0 200 400 160 40 -1 ",
        "4 1 1 2 1 0 200 400 160 40 -1 ",
        "5 1 1 2 1 1 200 400 160 40 91.0 Total",
        "5 1 1 2 1 2 380 400 10 40 20.0  ",
    ];
    let lines = parse_tsv(&rows.map(|row| row.replace(' ', "	")).join(
        "
",
    ));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].words.len(), 2);
    assert_eq!(
        lines[1].words,
        [("Total".to_string(), [200.0, 400.0, 360.0, 440.0])]
    );
    assert_ne!(lines[0].paragraph, lines[1].paragraph);

    let mut doc = lopdf::Document::load(&pdf).unwrap();
    let page = doc.get_pages()[&1];
    let font = pdfops::font::TextFont::new(&mut doc, "Invoice 4471 Total", None).unwrap();
    let open = doc.add_object(lopdf::Stream::new(
        lopdf::Dictionary::new(),
        b"q
"
        .to_vec(),
    ));
    assert_eq!(
        add_text_layer(&mut doc, page, &lines, 144.0, &font, open).unwrap(),
        3
    );
    let out = dir.path().join("layered.pdf");
    doc.save(&out).unwrap();

    // The words read back from where they were seen, each as wide as its picture.
    assert_eq!(texts(&out)[0], "Invoice 4471\n\nTotal");
    let found = common::words(&out, 1);
    let place = |word: &str| found.iter().find(|w| w.0 == word).unwrap().1;
    for (word, left, right, top, bottom) in [
        ("Invoice", 100.0, 210.0, 150.0, 170.0),
        ("4471", 220.0, 340.0, 150.0, 170.0),
        ("Total", 100.0, 180.0, 200.0, 220.0),
    ] {
        let b = place(word);
        assert!(
            (b[0] - left).abs() < 1.5 && (b[2] - right).abs() < 1.5,
            "{word}: {b:?}"
        );
        assert!(b[1] > top - 6.0 && b[3] < bottom + 6.0, "{word}: {b:?}");
    }
    // Nothing of it is drawn.
    assert_eq!(ink(&out, dir.path(), [[0, 0, 612, 792]]), [0]);
}

// Drives the external tesseract program, which takes seconds: run with `cargo test -- --ignored`.
#[test]
#[ignore = "needs tesseract; slow"]
fn ocr_reads_pages_without_a_text_layer() {
    let Some(lang) = ocr_lang() else {
        eprintln!("skipped: tesseract with language data is not installed");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let pdf = scan(dir.path(), &sample(dir.path(), "a.pdf", 1));
    let plain = call("pdf_text", json!({"input": pdf}));
    assert_eq!(plain["pages"][0]["text"], "");

    let v = call("pdf_ocr", json!({"input": pdf, "lang": lang}));
    let text = v["pages"][0]["text"].as_str().unwrap().to_lowercase();
    assert!(
        text.contains("sample") && text.contains("keyword"),
        "{text}"
    );

    let v = call(
        "pdf_text",
        json!({"input": pdf, "ocr": true, "ocr_lang": lang}),
    );
    assert_eq!(v["pages"][0]["ocr"], true);
    assert!(
        v["pages"][0]["text"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("sample")
    );

    // With an output, the text is written into a copy, where it is found without recognition.
    let out = dir.path().join("searchable.pdf");
    let v = call(
        "pdf_ocr",
        json!({"input": pdf, "lang": lang, "output": out}),
    );
    assert_eq!(v["pages"][0]["text_layer"], "added", "{v}");
    assert!(v["pages"][0]["words"].as_u64().unwrap() >= 5, "{v}");
    let text = texts(&out)[0].to_lowercase();
    assert!(
        text.contains("sample") && text.contains("keyword"),
        "{text}"
    );
    let found = call("pdf_search", json!({"input": out, "query": "sample"}));
    assert_eq!(found["total_matches"], 1, "{found}");
    // A page that has text is left as it is.
    let again = dir.path().join("again.pdf");
    let v = call(
        "pdf_ocr",
        json!({"input": out, "lang": lang, "output": again}),
    );
    assert!(
        v["pages"][0]["text_layer"]
            .as_str()
            .unwrap()
            .starts_with("kept"),
        "{v}"
    );
    assert_eq!(texts(&again)[0].to_lowercase().matches("sample").count(), 1);

    let e = call_err("pdf_ocr", json!({"input": pdf, "lang": "zzz"}));
    assert!(
        e.contains("ocr-install --lang zzz") && e.contains(&lang),
        "{e}"
    );
}
