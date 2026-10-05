//! Loading, saving and low level object helpers on top of `lopdf`.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use lopdf::{Dictionary, Document, Object, ObjectId};

/// Attributes a page may inherit from its ancestors in the page tree.
pub const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Loads a PDF, decrypting it when it is encrypted.
pub fn load(path: &Path, password: Option<&str>) -> Result<Document> {
    let doc = match password {
        Some(p) => Document::load_with_password(path, p),
        None => Document::load(path),
    }
    .map_err(|e| match e {
        lopdf::Error::IO(io) => anyhow!("cannot read {}: {io}", path.display()),
        other => anyhow!("cannot open {}: {other}", path.display()),
    })?;
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

/// Writes `doc` to `path` through a temp file, so `path` may be the input file.
pub fn save(doc: &mut Document, path: &Path) -> Result<u64> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    // lopdf derives the trailer /Size from `max_id`, which goes stale when objects are removed.
    doc.max_id = doc.objects.keys().map(|id| id.0).max().unwrap_or(0);
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

/// The document information dictionary, if present.
pub fn info_dict(doc: &Document) -> Option<&Dictionary> {
    resolve(doc, doc.trailer.get(b"Info").ok()?).as_dict().ok()
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
