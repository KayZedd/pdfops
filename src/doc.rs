//! Loading, saving and low level object helpers on top of `lopdf`.

use std::collections::{HashMap, HashSet};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use hayro::hayro_syntax::object::{
    Dict, MaybeRef, Object as LazyObject, ObjectIdentifier, Stream as LazyStream,
};
use hayro::hayro_syntax::{Filter, LoadPdfError, Pdf};
use lopdf::{Dictionary, Document, Object, ObjectId};

/// Attributes a page may inherit from its ancestors in the page tree.
pub const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Loads a PDF for rewriting, decrypting it when it is encrypted.
///
/// A file the strict reader cannot take in full is rebuilt from what the
/// repairing reader sees; `repaired_inputs` then names it.
pub fn load(path: &Path, password: Option<&str>) -> Result<Document> {
    Ok(load_noting(path, password)?.0)
}

/// Like `load`, and says whether the document had to be rebuilt.
pub fn load_noting(path: &Path, password: Option<&str>) -> Result<(Document, bool)> {
    let bytes =
        Arc::new(std::fs::read(path).map_err(|e| anyhow!("cannot read {}: {e}", path.display()))?);
    let strict = parse(&bytes, path, password);
    // An encrypted file is rebuilt from decrypted objects, and nothing here could
    // encrypt those again; such a file is taken as it is or not at all.
    let encrypted = trailer_has_encrypt(&bytes);
    let reason = match strict {
        Ok(doc) => match damage(&doc, bytes.clone(), password) {
            None => return Ok((doc, false)),
            Some(reason) => reason,
        },
        Err(e) if encrypted => return Err(e),
        Err(e) => match rebuild(bytes.clone()) {
            Ok(doc) => return Ok((note_repaired(path, doc), true)),
            Err(_) => return Err(e),
        },
    };
    let rebuilt = if encrypted {
        Err(anyhow!("it is encrypted as well"))
    } else {
        rebuild(bytes)
    };
    match rebuilt {
        Ok(doc) => Ok((note_repaired(path, doc), true)),
        Err(why) => bail!(
            "{} is damaged ({reason}) and could not be rebuilt ({why}). It can be read but not rewritten safely; repair it first, e.g. with `qpdf in.pdf repaired.pdf`",
            path.display()
        ),
    }
}

/// Files that had to be rebuilt on loading, in this process.
static REPAIRED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn note_repaired(path: &Path, doc: Document) -> Document {
    let mut repaired = REPAIRED.lock().unwrap_or_else(|e| e.into_inner());
    if !repaired.iter().any(|p| p == path) {
        repaired.push(path.to_path_buf());
    }
    doc
}

/// The files loaded for rewriting that were damaged and had to be rebuilt.
pub fn repaired_inputs() -> Vec<PathBuf> {
    REPAIRED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Loads a PDF for looking at, without insisting that it could be written back.
///
/// Returns the file's bytes as well, for callers that go on to check it.
pub fn read(path: &Path, password: Option<&str>) -> Result<(Document, Arc<Vec<u8>>)> {
    let bytes =
        Arc::new(std::fs::read(path).map_err(|e| anyhow!("cannot read {}: {e}", path.display()))?);
    Ok((parse(&bytes, path, password)?, bytes))
}

/// Parses a file's bytes with the strict reader.
fn parse(bytes: &[u8], path: &Path, password: Option<&str>) -> Result<Document> {
    let doc = match password {
        Some(p) => Document::load_mem_with_options(bytes, lopdf::LoadOptions::with_password(p)),
        None => Document::load_mem(bytes),
    }
    .map_err(|e| anyhow!("cannot open {}: {e}", path.display()))?;
    if doc.is_encrypted() {
        bail!(
            "{} is encrypted: {}",
            path.display(),
            if password.is_some() {
                "wrong password"
            } else {
                "pass a password"
            }
        );
    }
    Ok(doc)
}

/// One object as the repairing reader has it, in the writing model's terms.
///
/// References met on the way are added to `open`, to be fetched in turn.
fn convert(object: &LazyObject<'_>, open: &mut Vec<ObjectId>, depth: u32) -> Result<Object> {
    if depth > 200 {
        bail!("objects are nested too deeply");
    }
    let item = |item: MaybeRef<LazyObject<'_>>, open: &mut Vec<ObjectId>| match item {
        MaybeRef::Ref(r) => {
            // Numbers out of range cannot name an object; such a reference reads as null.
            match (u32::try_from(r.obj_number), u16::try_from(r.gen_number)) {
                (Ok(number), Ok(generation)) if number > 0 => {
                    open.push((number, generation));
                    Ok(Object::Reference((number, generation)))
                }
                _ => Ok(Object::Null),
            }
        }
        MaybeRef::NotRef(inner) => convert(&inner, open, depth + 1),
    };
    let dictionary = |dict: &Dict<'_>, open: &mut Vec<ObjectId>| -> Result<Dictionary> {
        let mut out = Dictionary::new();
        for (key, value) in dict.entries() {
            out.set(key.to_vec(), item(value, open)?);
        }
        Ok(out)
    };
    Ok(match object {
        LazyObject::Null(_) => Object::Null,
        LazyObject::Boolean(b) => Object::Boolean(*b),
        LazyObject::Number(n) => {
            let value = n.as_f64();
            if value.fract() == 0.0 && value.abs() < 9.0e15 {
                Object::Integer(value as i64)
            } else {
                Object::Real(value as f32)
            }
        }
        LazyObject::String(s) => Object::String(s.to_vec(), lopdf::StringFormat::Literal),
        LazyObject::Name(n) => Object::Name(n.to_vec()),
        LazyObject::Dict(dict) => Object::Dictionary(dictionary(dict, open)?),
        LazyObject::Array(array) => Object::Array(
            array
                .raw_iter()
                .map(|i| item(i, open))
                .collect::<Result<_>>()?,
        ),
        LazyObject::Stream(stream) => {
            let dict = dictionary(stream.dict(), open)?;
            // The data stays as stored; its length is stated anew from what was found.
            Object::Stream(lopdf::Stream::new(dict, stream.raw_data().into_owned()))
        }
    })
}

/// Builds a document from what the repairing reader sees of a damaged file.
///
/// Everything the catalog and the pages reach is carried over as it is read; the
/// page tree is laid out afresh from the pages in the order that reader gives
/// them. What it cannot read is absent, exactly as it is for `text` and `render`.
fn rebuild(bytes: Arc<Vec<u8>>) -> Result<Document> {
    let pdf = Pdf::new(bytes.clone()).map_err(|_| anyhow!("it has no readable structure"))?;
    let xref = pdf.xref();
    let key = |id: ObjectIdentifier| -> Option<ObjectId> {
        let number = u32::try_from(id.obj_number).ok().filter(|n| *n > 0)?;
        Some((number, u16::try_from(id.gen_number).ok()?))
    };
    let pages: Vec<ObjectId> = pdf
        .pages()
        .iter()
        .map(|page| page.raw().obj_id().and_then(key))
        .collect::<Option<_>>()
        .ok_or_else(|| anyhow!("a page is not an object of its own"))?;
    if pages.is_empty() {
        bail!("no page could be read");
    }
    let root = key(xref.root_id()).ok_or_else(|| anyhow!("it has no catalog"))?;
    let info = info_reference(&bytes);

    let mut doc = Document::with_version("1.7");
    let mut open: Vec<ObjectId> = pages.clone();
    open.push(root);
    open.extend(info);
    while let Some(id) = open.pop() {
        if doc.objects.contains_key(&id) {
            continue;
        }
        if doc.objects.len() > 5_000_000 {
            bail!("it has too many objects");
        }
        // A reference to nothing reads as null, which is what leaving it out gives.
        let lazy = xref.get::<LazyObject<'_>>(ObjectIdentifier::new(id.0 as i32, id.1 as i32));
        if let Some(lazy) = lazy {
            let mut object = convert(&lazy, &mut open, 0)?;
            // Packed data that does not unpack is of no use to any reader, and a strict
            // one rejects the whole file over it. The stream stays, empty.
            if let (LazyObject::Stream(source), Object::Stream(stream)) = (&lazy, &mut object) {
                let packed = source.filters().iter().all(|f| {
                    matches!(
                        f,
                        Filter::FlateDecode
                            | Filter::LzwDecode
                            | Filter::AsciiHexDecode
                            | Filter::Ascii85Decode
                            | Filter::RunLengthDecode
                    )
                });
                // The reader here forgives a deflate stream its header and checksum;
                // others do not, so such data is unpacked and packed again.
                let strict = || {
                    use std::io::Read;
                    let mut sink = Vec::new();
                    flate2::read::ZlibDecoder::new(&stream.content[..])
                        .read_to_end(&mut sink)
                        .is_ok()
                };
                let unsound = match source.filters().as_slice() {
                    [] => false,
                    [Filter::FlateDecode] => !strict(),
                    _ => packed && source.decoded().is_err(),
                };
                if unsound {
                    let data = source.decoded().map(|d| d.into_owned()).unwrap_or_default();
                    stream.dict.remove(b"Filter");
                    stream.dict.remove(b"DecodeParms");
                    stream.set_content(data);
                    let _ = stream.compress();
                }
            }
            doc.objects.insert(id, object);
        }
    }
    if doc.get_dictionary(root).is_err() {
        bail!("its catalog cannot be read");
    }
    doc.max_id = doc.objects.keys().map(|id| id.0).max().unwrap_or(0);

    // What a page inherits is written onto the page before its ancestors are let go.
    let mut own: Vec<Vec<(&[u8], Object)>> = Vec::with_capacity(pages.len());
    for &page in &pages {
        if doc.get_dictionary(page).is_err() {
            bail!("a page cannot be read");
        }
        own.push(
            INHERITABLE
                .iter()
                .filter_map(|key| Some((*key, inherited(&doc, page, key)?.clone())))
                .collect(),
        );
    }
    // Content that is not a stream, or whose data does not decode, cannot be drawn: a
    // page found by searching may name anything. The reader this mirrors shows nothing
    // for it, and a strict one would reject the file over it.
    let drawable = |object: &Object| {
        object.as_reference().is_ok_and(|id| {
            xref.get::<LazyStream<'_>>(ObjectIdentifier::new(id.0 as i32, id.1 as i32))
                .is_some_and(|stream| stream.decoded().is_ok())
        })
    };
    // The parts may also be listed in an object of their own.
    let lists: HashMap<ObjectId, Vec<Object>> = doc
        .objects
        .iter()
        .filter_map(|(id, object)| Some((*id, object.as_array().ok()?.clone())))
        .collect();
    let tree = doc.new_object_id();
    for (&page, attributes) in pages.iter().zip(own) {
        let dict = doc.get_dictionary_mut(page)?;
        for (key, value) in attributes {
            dict.set(key, value);
        }
        // A page must say how large it is; the format's own default is US Letter.
        if !dict.has(b"MediaBox") {
            dict.set(
                "MediaBox",
                vec![0.into(), 0.into(), 612.into(), 792.into()] as Vec<Object>,
            );
        }
        let listed = |object: &Object| lists.get(&object.as_reference().ok()?);
        let contents = match dict.get(b"Contents").ok() {
            Some(one) if listed(one).is_some() => Some(Object::Array(
                listed(one)
                    .into_iter()
                    .flatten()
                    .filter(|p| drawable(p))
                    .cloned()
                    .collect(),
            )),
            Some(Object::Array(parts)) => Some(Object::Array(
                parts.iter().filter(|p| drawable(p)).cloned().collect(),
            )),
            Some(one) if drawable(one) => Some(one.clone()),
            _ => None,
        };
        match contents {
            Some(contents) => dict.set("Contents", contents),
            None => {
                dict.remove(b"Contents");
            }
        }
        dict.set("Type", Object::Name(b"Page".to_vec()));
        dict.set("Parent", tree);
    }
    let mut node = Dictionary::new();
    node.set("Type", Object::Name(b"Pages".to_vec()));
    node.set("Count", pages.len() as i64);
    node.set(
        "Kids",
        pages
            .iter()
            .map(|&id| Object::Reference(id))
            .collect::<Vec<_>>(),
    );
    doc.objects.insert(tree, Object::Dictionary(node));
    let catalog = doc.get_dictionary_mut(root)?;
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", tree);
    doc.trailer.set("Root", root);
    if let Some(info) = info.filter(|id| doc.get_dictionary(*id).is_ok()) {
        doc.trailer.set("Info", info);
    }
    // The old tree's nodes, and whatever only they held on to.
    crate::ops::edit::prune(&mut doc);
    if doc.get_pages().len() != pages.len() {
        bail!("its pages cannot be laid out again");
    }
    Ok(doc)
}

/// The information dictionary the newest trailer names, read from the bytes.
fn info_reference(bytes: &[u8]) -> Option<ObjectId> {
    let pattern = regex::bytes::Regex::new(r"(?-u)/Info\s+(\d{1,10})\s+(\d{1,5})\s+R")
        .expect("the pattern is valid");
    let found = pattern.captures_iter(bytes).last()?;
    let number = |i: usize| std::str::from_utf8(&found[i]).ok()?.parse::<u32>().ok();
    Some((
        number(1).filter(|n| *n > 0)?,
        u16::try_from(number(2)?).ok()?,
    ))
}

/// Why this document must not be written back, if it was not read in full.
///
/// The object model used for writing follows the file's cross-reference table to
/// the letter, while the reader used for everything else repairs broken files.
/// Where the two disagree, saving would silently drop what the first one missed.
fn damage(
    doc: &Document,
    bytes: std::sync::Arc<Vec<u8>>,
    password: Option<&str>,
) -> Option<String> {
    let pages = doc.get_pages();
    let seen = Pdf::new_with_password(bytes, password.unwrap_or(""))
        .map(|pdf| pdf.pages().len())
        .ok();
    if pages.is_empty() {
        return Some("its page tree could not be read".to_string());
    }
    if seen.is_some_and(|n| n != pages.len()) {
        return Some(format!(
            "{} of its {} pages could be read",
            pages.len(),
            seen.unwrap_or(0)
        ));
    }
    for (number, id) in pages {
        let Ok(page) = doc.get_dictionary(id) else {
            return Some(format!("page {number} is missing"));
        };
        // A page whose content or resources cannot be found would come out blank.
        for key in [&b"Contents"[..], b"Resources"] {
            let target = page.get(key).ok();
            let missing = |o: &Object| o.as_reference().is_ok_and(|r| !doc.has_object(r));
            let lost = target.is_some_and(|o| match o {
                Object::Array(items) => items.iter().any(missing),
                other => missing(other),
            });
            if lost {
                return Some(format!("objects of page {number} are missing"));
            }
        }
    }
    // Without a length the extent of a stream is a guess, and its data is not read.
    let unsized_stream = doc
        .objects
        .values()
        .any(|o| o.as_stream().is_ok_and(|s| !s.dict.has(b"Length")));
    unsized_stream.then(|| "a stream has no length".to_string())
}

/// Writes `doc` to `path` through a temp file, so `path` may be the input file.
pub fn save(doc: &mut Document, path: &Path) -> Result<u64> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    seal(doc)?;
    let written = (|| -> Result<()> {
        let mut w = BufWriter::new(std::fs::File::create(&tmp)?);
        doc.save_to(&mut w)?;
        w.flush()?;
        Ok(())
    })();
    if let Err(e) = written.and_then(|_| Ok(std::fs::rename(&tmp, path)?)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.context(format!("cannot write {}", path.display())));
    }
    Ok(std::fs::metadata(path)?.len())
}

/// Like `save`, but a dry run only measures: the document is serialised and nothing is written.
pub fn save_unless(dry_run: bool, doc: &mut Document, path: &Path) -> Result<u64> {
    if !dry_run {
        return save(doc, path);
    }
    seal(doc)?;
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)?;
    Ok(bytes.len() as u64)
}

/// Makes a document ready to be serialised. Returns whether it is encrypted.
///
/// Every path that writes a document goes through here, so that numbering,
/// the object count and protection are settled in one place.
pub fn seal(doc: &mut Document) -> Result<bool> {
    let highest = |doc: &Document| doc.objects.keys().map(|id| id.0).max().unwrap_or(0);
    // Object numbers far beyond the number of objects are legal, but readers that
    // guard against corrupt files reject them. Numbering goes first: older ciphers
    // mix the object number into each object's key.
    if highest(doc) as usize > doc.objects.len() * 2 + 100 {
        doc.renumber_objects();
    }
    let encrypted = protect_again(doc)?;
    // lopdf derives the trailer /Size from `max_id`, which goes stale when objects are removed.
    doc.max_id = highest(doc);
    Ok(encrypted)
}

/// Gives the trailer a file identifier if it lacks a usable one.
pub fn ensure_id(doc: &mut Document) -> Result<()> {
    let usable = doc
        .trailer
        .get(b"ID")
        .ok()
        .and_then(|id| id.as_array().ok())
        .is_some_and(|parts| {
            parts.len() == 2
                && parts
                    .iter()
                    .all(|p| p.as_str().is_ok_and(|s| !s.is_empty()))
        });
    if !usable {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| anyhow!("no system randomness: {e}"))?;
        let part = Object::String(bytes.to_vec(), lopdf::StringFormat::Hexadecimal);
        doc.trailer.set("ID", vec![part.clone(), part]);
    }
    Ok(())
}

/// Encrypts the document again with the passwords and permissions it was opened with.
///
/// Loading decrypts in memory; without this, editing a protected file would
/// silently hand back an unprotected one. Returns whether it is encrypted now.
fn protect_again(doc: &mut Document) -> Result<bool> {
    let Some(state) = doc.encryption_state.clone().filter(|_| !doc.is_encrypted()) else {
        return Ok(doc.is_encrypted());
    };
    doc.encrypt(&state)
        .map_err(|e| anyhow!("cannot keep the document's encryption: {e}"))?;
    // Some files name crypt filters they never define. Re-encrypting those would write
    // a dictionary no reader can use, around content that is not encrypted at all.
    let dict = doc
        .get_encrypted()
        .map_err(|e| anyhow!("cannot keep the document's encryption: {e}"))?;
    let version = dict
        .get(b"V")
        .ok()
        .and_then(|v| v.as_i64().ok())
        .unwrap_or(0);
    let defined = |key: &[u8]| {
        let name = dict
            .get(key)
            .ok()
            .and_then(|n| n.as_name().ok())
            .unwrap_or(b"Identity");
        name == b"Identity"
            || dict
                .get(b"CF")
                .ok()
                .and_then(|cf| cf.as_dict().ok())
                .is_some_and(|cf| cf.has(name))
    };
    if version >= 4 && !(defined(b"StmF") && defined(b"StrF")) {
        bail!(
            "this document's encryption cannot be reproduced; remove it first with the decrypt command"
        );
    }
    Ok(true)
}

/// Writes finished bytes to `path` through a temp file, so `path` may be the input file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            anyhow!("cannot write {}: {e}", path.display())
        })
}

/// Page number to object id, 1-based and in document order.
pub fn page_ids(doc: &Document) -> Vec<ObjectId> {
    doc.get_pages().into_values().collect()
}

/// Follows a reference, if `obj` is one.
pub fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    doc.dereference(obj).map(|(_, o)| o).unwrap_or(obj)
}

/// Looks `key` up on the page, then on its ancestors.
pub fn inherited<'a>(doc: &'a Document, page: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut dict = doc.get_dictionary(page).ok()?;
    // Bounded so that a cyclic /Parent chain cannot loop forever.
    for _ in 0..64 {
        if let Ok(v) = dict.get(key) {
            return Some(resolve(doc, v));
        }
        dict = doc
            .get_dictionary(dict.get(b"Parent").ok()?.as_reference().ok()?)
            .ok()?;
    }
    None
}

pub fn number(doc: &Document, obj: &Object) -> Option<f64> {
    match resolve(doc, obj) {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(*r as f64),
        _ => None,
    }
}

/// `[x0, y0, x1, y1]` of the visible page area, in points.
pub fn page_box(doc: &Document, page: ObjectId) -> [f64; 4] {
    let read = |key: &[u8]| -> Option<[f64; 4]> {
        let arr = inherited(doc, page, key)?.as_array().ok()?;
        let v: Vec<f64> = arr.iter().filter_map(|o| number(doc, o)).collect();
        <[f64; 4]>::try_from(v).ok()
    };
    // US Letter is the PDF default when no box is present.
    read(b"CropBox")
        .or_else(|| read(b"MediaBox"))
        .unwrap_or([0.0, 0.0, 612.0, 792.0])
}

pub fn rotation(doc: &Document, page: ObjectId) -> i64 {
    inherited(doc, page, b"Rotate")
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0)
        .rem_euclid(360)
}

pub fn text(obj: &Object) -> Option<String> {
    match obj {
        Object::String(..) => lopdf::decode_text_string(obj).ok(),
        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
        _ => None,
    }
}

/// Id of an indirect dictionary stored under `key`, creating or relocating it as needed.
pub fn ensure_indirect_dict(
    doc: &mut Document,
    owner: Option<ObjectId>,
    key: &[u8],
) -> Result<ObjectId> {
    let current = match owner {
        Some(id) => doc.get_dictionary(id)?.get(key).ok().cloned(),
        None => doc.trailer.get(key).ok().cloned(),
    };
    let id = match current {
        Some(Object::Reference(id)) if doc.get_dictionary(id).is_ok() => return Ok(id),
        Some(Object::Dictionary(d)) => doc.add_object(d),
        _ => doc.add_object(Dictionary::new()),
    };
    match owner {
        Some(o) => doc.get_dictionary_mut(o)?.set(key, id),
        None => doc.trailer.set(key, id),
    }
    Ok(id)
}

pub fn catalog_id(doc: &Document) -> Result<ObjectId> {
    Ok(doc.trailer.get(b"Root")?.as_reference()?)
}

/// One bookmark of the document outline.
pub struct OutlineItem {
    /// Nesting depth, 1 for top-level entries.
    pub level: usize,
    pub title: String,
    /// The page the bookmark jumps to, if it has a resolvable target.
    pub page: Option<ObjectId>,
}

/// The page a destination (an array, or a dictionary wrapping one) points at.
fn destination_page(doc: &Document, dest: &Object) -> Option<ObjectId> {
    match resolve(doc, dest) {
        Object::Array(a) => a.first()?.as_reference().ok(),
        Object::Dictionary(d) => resolve(doc, d.get(b"D").ok()?)
            .as_array()
            .ok()?
            .first()?
            .as_reference()
            .ok(),
        _ => None,
    }
}

/// Named destinations from the catalog's /Dests dictionary and /Names tree.
fn named_destinations(doc: &Document) -> HashMap<Vec<u8>, ObjectId> {
    let mut map = HashMap::new();
    let Ok(catalog) = doc.catalog() else {
        return map;
    };
    let dict = |owner: &Dictionary, key: &[u8]| {
        owner
            .get(key)
            .ok()
            .and_then(|o| resolve(doc, o).as_dict().ok())
            .cloned()
    };
    if let Some(dests) = dict(catalog, b"Dests") {
        for (name, dest) in dests.iter() {
            map.extend(destination_page(doc, dest).map(|page| (name.clone(), page)));
        }
    }
    let mut open: Vec<Dictionary> = dict(catalog, b"Names")
        .and_then(|n| dict(&n, b"Dests"))
        .into_iter()
        .collect();
    // Bounded so that a cyclic tree cannot loop forever.
    for _ in 0..100_000 {
        let Some(node) = open.pop() else { break };
        let array = |key: &[u8]| {
            node.get(key)
                .ok()
                .and_then(|o| resolve(doc, o).as_array().ok())
        };
        for pair in array(b"Names")
            .map(|n| n.as_chunks::<2>().0.iter())
            .into_iter()
            .flatten()
        {
            if let (Ok(name), Some(page)) = (
                resolve(doc, &pair[0]).as_str(),
                destination_page(doc, &pair[1]),
            ) {
                map.insert(name.to_vec(), page);
            }
        }
        for kid in array(b"Kids").into_iter().flatten() {
            open.extend(resolve(doc, kid).as_dict().ok().cloned());
        }
    }
    map
}

/// The document outline in reading order.
///
/// Read here rather than with lopdf's `get_toc`, which keys entries by title
/// and so loses every bookmark whose title repeats.
pub fn outline(doc: &Document) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    let first =
        |dict: &Dictionary, key: &[u8]| dict.get(key).ok().and_then(|o| o.as_reference().ok());
    let root = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Outlines").ok())
        .and_then(|o| resolve(doc, o).as_dict().ok());
    let mut open: Vec<(ObjectId, usize)> = root
        .and_then(|r| first(r, b"First"))
        .map(|id| (id, 1))
        .into_iter()
        .collect();
    let mut seen = HashSet::new();
    let mut named: Option<HashMap<Vec<u8>, ObjectId>> = None;
    while let Some((id, level)) = open.pop() {
        // `seen` guards against cyclic First/Next links.
        let Ok(item) = doc.get_dictionary(id) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        // The sibling goes on the stack first, so the children are visited before it.
        open.extend(first(item, b"Next").map(|next| (next, level)));
        open.extend(first(item, b"First").map(|child| (child, level + 1)));

        let target = item.get(b"Dest").ok().or_else(|| {
            let action = resolve(doc, item.get(b"A").ok()?).as_dict().ok()?;
            (action.get(b"S").ok()?.as_name().ok()? == b"GoTo").then(|| action.get(b"D").ok())?
        });
        let page = target.and_then(|t| match resolve(doc, t) {
            Object::Name(name) | Object::String(name, _) => named
                .get_or_insert_with(|| named_destinations(doc))
                .get(name)
                .copied(),
            other => destination_page(doc, other),
        });
        let title = item
            .get(b"Title")
            .ok()
            .and_then(|t| text(resolve(doc, t)))
            .unwrap_or_default();
        items.push(OutlineItem { level, title, page });
    }
    items
}

/// Opens a PDF without parsing its objects up front; they are read on demand.
///
/// This is what read-only commands on large files want: opening costs a couple
/// of milliseconds regardless of document size.
pub fn open_lazy(path: &Path, password: Option<&str>) -> Result<(Pdf, bool)> {
    let bytes = std::fs::read(path).map_err(|e| anyhow!("cannot read {}: {e}", path.display()))?;
    let encrypted = trailer_has_encrypt(&bytes);
    let pdf = Pdf::new_with_password(bytes, password.unwrap_or("")).map_err(|e| match e {
        LoadPdfError::Decryption(_) => anyhow!(
            "{} is encrypted: {}",
            path.display(),
            if password.is_some() {
                "wrong password"
            } else {
                "pass a password"
            }
        ),
        LoadPdfError::Invalid => anyhow!("cannot open {}: not a valid PDF", path.display()),
    })?;
    Ok((pdf, encrypted))
}

/// Whether the newest trailer names an encryption dictionary.
///
/// The lazy parser decrypts transparently and does not say whether it had to,
/// so this reads the trailer the file's `startxref` points at.
pub fn trailer_has_encrypt(bytes: &[u8]) -> bool {
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }
    fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).rposition(|w| w == needle)
    }
    let tail = &bytes[bytes.len().saturating_sub(2048)..];
    let Some(at) = rfind(tail, b"startxref") else {
        return false;
    };
    let digits: String = tail[at + 9..]
        .iter()
        .skip_while(|b| b.is_ascii_whitespace())
        .take_while(|b| b.is_ascii_digit())
        .map(|&b| b as char)
        .collect();
    let Some(section) = digits
        .parse::<usize>()
        .ok()
        .and_then(|offset| bytes.get(offset..))
    else {
        return false;
    };
    // A classic table is followed by "trailer << ... >>"; a cross-reference stream
    // keeps the same entries in its own dictionary, which ends where its data starts.
    let trailer = if section.starts_with(b"xref") {
        let start = find(section, b"trailer").unwrap_or(section.len());
        let rest = &section[start..];
        &rest[..find(rest, b"startxref").unwrap_or(rest.len())]
    } else {
        &section[..find(section, b"stream").unwrap_or(section.len().min(4096))]
    };
    find(trailer, b"/Encrypt").is_some()
}

#[cfg(test)]
mod tests {
    use super::trailer_has_encrypt;

    #[test]
    fn detects_encrypt_in_classic_trailers_and_xref_streams() {
        let classic = |trailer: &str| {
            format!(
                "%PDF-1.4\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< {trailer} >>\nstartxref\n9\n%%EOF"
            )
        };
        assert!(trailer_has_encrypt(
            classic("/Root 1 0 R /Encrypt 5 0 R").as_bytes()
        ));
        assert!(!trailer_has_encrypt(classic("/Root 1 0 R").as_bytes()));

        let stream = |dict: &str| {
            format!(
                "%PDF-1.5\n7 0 obj\n<< /Type /XRef {dict} >>\nstream\n/Encrypt\nendstream\nendobj\nstartxref\n9\n%%EOF"
            )
        };
        assert!(trailer_has_encrypt(stream("/Encrypt 5 0 R").as_bytes()));
        // The word inside the stream data is not a dictionary entry.
        assert!(!trailer_has_encrypt(stream("/Size 8").as_bytes()));

        assert!(!trailer_has_encrypt(b"not a pdf"));
        assert!(!trailer_has_encrypt(b"startxref\n99999\n%%EOF"));
    }
}
