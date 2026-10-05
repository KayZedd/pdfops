//! Loading, saving and low level object helpers on top of `lopdf`.

use std::collections::{HashMap, HashSet};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use hayro::hayro_syntax::{LoadPdfError, Pdf};
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
    protect_again(doc)?;
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

/// Encrypts the document again with the passwords and permissions it was opened with.
///
/// Loading decrypts in memory; without this, editing a protected file would
/// silently hand back an unprotected one. Returns whether it is encrypted now.
pub fn protect_again(doc: &mut Document) -> Result<bool> {
    if let Some(state) = doc.encryption_state.clone().filter(|_| !doc.is_encrypted()) {
        doc.encrypt(&state)
            .map_err(|e| anyhow!("cannot keep the document's encryption: {e}"))?;
    }
    Ok(doc.is_encrypted())
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
