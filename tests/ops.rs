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

    let e = call_err("pdf_ocr", json!({"input": pdf, "lang": "zzz"}));
    assert!(
        e.contains("ocr-install --lang zzz") && e.contains(&lang),
        "{e}"
    );
}
