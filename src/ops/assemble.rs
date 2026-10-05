//! Commands that build new documents out of pages: merge, pages, split.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args;
use lopdf::{Dictionary, Document, Object, ObjectId};
use rayon::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{doc, pagespec};

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct MergeArgs {
    /// PDF files to concatenate, in order
    #[arg(required = true, num_args = 1..)]
    pub inputs: Vec<PathBuf>,
    /// Where to write the merged PDF
    #[arg(short, long)]
    pub output: PathBuf,
    /// Password used for any encrypted input
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct PagesArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Where to write the result
    #[arg(short, long)]
    pub output: PathBuf,
    /// Pages to keep, in output order, e.g. "3,1,5-" (reorders and may repeat pages)
    #[arg(short, long, conflicts_with = "delete")]
    pub keep: Option<String>,
    /// Pages to remove, e.g. "2,7-9"; all other pages are kept in order
    #[arg(short, long)]
    pub delete: Option<String>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct SplitArgs {
    /// Source PDF file
    pub input: PathBuf,
    /// Directory for the parts, created if missing
    #[arg(short, long)]
    pub out_dir: PathBuf,
    /// Pages per part (default: 1, unless ranges are given)
    #[arg(long, conflicts_with = "ranges")]
    pub every: Option<u32>,
    /// One page spec per part, e.g. "1-3" then "4-"
    #[arg(long = "range")]
    #[serde(default)]
    pub ranges: Vec<String>,
    /// Password, if the file is encrypted
    #[arg(long)]
    pub password: Option<String>,
}

/// Pages taken from one source document, in output order.
pub struct Source<'a> {
    pub doc: &'a Document,
    pub pages: Vec<u32>,
    /// The source's bookmarks, read once by the caller: a split builds many
    /// documents from the same source.
    pub outline: &'a [doc::OutlineItem],
}

/// Copies the object graph reachable from selected pages into another document.
struct Copier<'a> {
    src: &'a Document,
    /// Every page of the source, so links to unselected pages can be cut.
    all_pages: HashSet<ObjectId>,
    map: HashMap<ObjectId, ObjectId>,
    queue: Vec<ObjectId>,
}

impl Copier<'_> {
    /// Rewrites references inside `obj` to ids in `out`, queueing unseen targets.
    fn rewrite(&mut self, obj: &mut Object, out: &mut Document) {
        match obj {
            Object::Reference(id) => {
                if let Some(new) = self.map.get(id) {
                    *id = *new;
                } else if self.is_page_tree(*id) || !self.src.has_object(*id) {
                    // Following these would drag unselected pages into the output.
                    *obj = Object::Null;
                } else {
                    let new = out.new_object_id();
                    self.map.insert(*id, new);
                    self.queue.push(*id);
                    *id = new;
                }
            }
            Object::Array(items) => items.iter_mut().for_each(|o| self.rewrite(o, out)),
            Object::Dictionary(dict) => dict.iter_mut().for_each(|(_, o)| self.rewrite(o, out)),
            Object::Stream(stream) => stream
                .dict
                .iter_mut()
                .for_each(|(_, o)| self.rewrite(o, out)),
            _ => {}
        }
    }

    fn is_page_tree(&self, id: ObjectId) -> bool {
        self.all_pages.contains(&id)
            || self.src.get_dictionary(id).is_ok_and(|d| {
                d.get(b"Type")
                    .is_ok_and(|t| t.as_name().ok() == Some(b"Pages"))
            })
    }

    /// Copies queued objects until the graph is closed. Iterative, so long chains cannot overflow the stack.
    fn drain(&mut self, out: &mut Document) {
        while let Some(old) = self.queue.pop() {
            let Ok(mut obj) = self.src.get_object(old).cloned() else {
                continue;
            };
            self.rewrite(&mut obj, out);
            out.objects.insert(self.map[&old], obj);
        }
    }

    /// Copies one page under `parent`, with inherited attributes made explicit.
    fn copy_page(
        &mut self,
        page: ObjectId,
        new: ObjectId,
        parent: ObjectId,
        out: &mut Document,
    ) -> Result<()> {
        let mut dict = self.src.get_dictionary(page)?.clone();
        dict.remove(b"Parent");
        for key in doc::INHERITABLE {
            if !dict.has(key)
                && let Some(v) = doc::inherited(self.src, page, key)
            {
                dict.set(key, v.clone());
            }
        }
        let mut obj = Object::Dictionary(dict);
        self.rewrite(&mut obj, out);
        if let Object::Dictionary(d) = &mut obj {
            d.set("Parent", parent);
        }
        out.objects.insert(new, obj);
        self.drain(out);
        Ok(())
    }
}

/// Builds a document from the selected pages of each source.
///
/// Document info comes from the first source; form fields and bookmarks from all of them.
pub fn assemble(sources: &[Source]) -> Result<Document> {
    let mut out = Document::with_version("1.7");
    let pages_id = out.new_object_id();
    let mut kids = Vec::new();
    let mut form: Option<Dictionary> = None;
    let mut outline: Vec<(usize, String, ObjectId)> = Vec::new();
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", pages_id);

    for (i, src) in sources.iter().enumerate() {
        let ids = doc::page_ids(src.doc);
        let mut copier = Copier {
            src: src.doc,
            all_pages: ids.iter().copied().collect(),
            map: HashMap::new(),
            queue: Vec::new(),
        };
        // Selected pages get their ids first, so links between them survive the copy.
        let mut targets = Vec::with_capacity(src.pages.len());
        for &n in &src.pages {
            let old = *ids
                .get(n as usize - 1)
                .with_context(|| format!("page {n} out of range"))?;
            let new = out.new_object_id();
            copier.map.entry(old).or_insert(new);
            targets.push((old, new));
        }
        for (old, new) in targets {
            copier.copy_page(old, new, pages_id, &mut out)?;
            kids.push(Object::Reference(new));
        }
        if i == 0
            && let Ok(info) = src.doc.trailer.get(b"Info")
        {
            let mut info = info.clone();
            copier.rewrite(&mut info, &mut out);
            out.trailer.set("Info", info);
            copier.drain(&mut out);
        }
        // Bookmarks follow their pages: entries whose target was not selected are dropped.
        for entry in src.outline {
            if let Some(&page) = entry.page.and_then(|old| copier.map.get(&old)) {
                outline.push((entry.level, entry.title.clone(), page));
            }
        }
        let source_form = src.doc.catalog().ok().and_then(|c| {
            doc::resolve(src.doc, c.get(b"AcroForm").ok()?)
                .as_dict()
                .ok()
        });
        if let Some(source_form) = source_form {
            let mut copied = Object::Dictionary(source_form.clone());
            copier.rewrite(&mut copied, &mut out);
            copier.drain(&mut out);
            let Object::Dictionary(mut copied) = copied else {
                unreachable!("rewrite keeps the object kind")
            };
            let fields = copied
                .get(b"Fields")
                .ok()
                .and_then(|f| doc::resolve(&out, f).as_array().ok())
                .cloned()
                .unwrap_or_default();
            match &mut form {
                None => {
                    copied.set("Fields", fields);
                    form = Some(copied);
                }
                Some(form) if !fields.is_empty() => {
                    // Field names are global, so later documents get their own namespace
                    // instead of silently sharing values with same-named fields.
                    let group = out.new_object_id();
                    for kid in fields.iter().filter_map(|f| f.as_reference().ok()) {
                        if let Ok(kid) = out.get_dictionary_mut(kid) {
                            kid.set("Parent", group);
                        }
                    }
                    let mut dict = Dictionary::new();
                    dict.set("T", Object::string_literal(format!("doc{}", i + 1)));
                    dict.set("Kids", fields);
                    out.objects.insert(group, Object::Dictionary(dict));
                    if let Ok(Object::Array(all)) = form.get_mut(b"Fields") {
                        all.push(Object::Reference(group));
                    }
                }
                Some(_) => {}
            }
        }
    }
    if let Some(form) = form {
        catalog.set("AcroForm", form);
    }
    if let Some(root) = build_outline(&mut out, &outline) {
        catalog.set("Outlines", root);
    }

    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Count", kids.len() as i64);
    pages.set("Kids", kids);
    out.objects.insert(pages_id, Object::Dictionary(pages));
    let catalog_id = out.add_object(catalog);
    out.trailer.set("Root", catalog_id);
    Ok(out)
}

/// Writes an outline tree for `entries` (level, title, target page) and returns its root.
///
/// An entry whose parent was dropped moves up to the nearest surviving ancestor.
fn build_outline(out: &mut Document, entries: &[(usize, String, ObjectId)]) -> Option<ObjectId> {
    if entries.is_empty() {
        return None;
    }
    let root = out.new_object_id();
    let ids: Vec<ObjectId> = entries.iter().map(|_| out.new_object_id()).collect();
    // children[0] belongs to the root, children[i + 1] to entry i.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); entries.len() + 1];
    let mut parents = Vec::with_capacity(entries.len());
    let mut open: Vec<(usize, usize)> = Vec::new();
    for (i, (level, ..)) in entries.iter().enumerate() {
        while open.last().is_some_and(|&(l, _)| l >= *level) {
            open.pop();
        }
        let parent = open.last().map(|&(_, p)| p);
        children[parent.map_or(0, |p| p + 1)].push(i);
        parents.push(parent);
        open.push((*level, i));
    }
    for (i, (_, title, page)) in entries.iter().enumerate() {
        let siblings = &children[parents[i].map_or(0, |p| p + 1)];
        let at = siblings
            .iter()
            .position(|&s| s == i)
            .expect("every entry is listed under its parent");
        let mut item = Dictionary::new();
        item.set("Title", lopdf::text_string(title));
        item.set("Parent", parents[i].map_or(root, |p| ids[p]));
        // XYZ with nulls keeps the reader's zoom and lands at the top of the page.
        item.set(
            "Dest",
            vec![
                Object::Reference(*page),
                "XYZ".into(),
                Object::Null,
                Object::Null,
                Object::Null,
            ],
        );
        if at > 0 {
            item.set("Prev", ids[siblings[at - 1]]);
        }
        if let Some(&next) = siblings.get(at + 1) {
            item.set("Next", ids[next]);
        }
        if let (Some(&first), Some(&last)) = (children[i + 1].first(), children[i + 1].last()) {
            item.set("First", ids[first]);
            item.set("Last", ids[last]);
            // Negative: the entry starts collapsed.
            item.set("Count", -(children[i + 1].len() as i64));
        }
        out.objects.insert(ids[i], Object::Dictionary(item));
    }
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"Outlines".to_vec()));
    dict.set("First", ids[*children[0].first()?]);
    dict.set("Last", ids[*children[0].last()?]);
    dict.set("Count", children[0].len() as i64);
    out.objects.insert(root, Object::Dictionary(dict));
    Some(root)
}

pub fn merge(a: MergeArgs) -> Result<Value> {
    let docs: Vec<Document> = a
        .inputs
        .par_iter()
        .map(|p| doc::load(p, a.password.as_deref()))
        .collect::<Result<_>>()?;
    let outlines: Vec<Vec<doc::OutlineItem>> = docs.iter().map(doc::outline).collect();
    let sources: Vec<Source> = docs
        .iter()
        .zip(&outlines)
        .map(|(d, outline)| Source {
            doc: d,
            pages: (1..=doc::page_ids(d).len() as u32).collect(),
            outline,
        })
        .collect();
    let mut out = assemble(&sources)?;
    let size = doc::save(&mut out, &a.output)?;
    Ok(json!({
        "output": a.output,
        "pages": sources.iter().map(|s| s.pages.len()).sum::<usize>(),
        "inputs": a.inputs.iter().zip(&sources).map(|(p, s)| json!({"file": p, "pages": s.pages.len()})).collect::<Vec<_>>(),
        "size_bytes": size,
    }))
}

pub fn pages(a: PagesArgs) -> Result<Value> {
    let d = doc::load(&a.input, a.password.as_deref())?;
    let total = doc::page_ids(&d).len() as u32;
    let selected = match (&a.keep, &a.delete) {
        (Some(_), Some(_)) => bail!("use either keep or delete, not both"),
        (Some(keep), None) => pagespec::parse(keep, total)?,
        (None, Some(delete)) => {
            let gone: HashSet<u32> = pagespec::parse(delete, total)?.into_iter().collect();
            let rest: Vec<u32> = (1..=total).filter(|n| !gone.contains(n)).collect();
            if rest.is_empty() {
                bail!("deleting '{delete}' would leave no pages");
            }
            rest
        }
        (None, None) => bail!("give pages to keep or pages to delete"),
    };
    let mut out = assemble(&[Source {
        doc: &d,
        pages: selected.clone(),
        outline: &doc::outline(&d),
    }])?;
    let size = doc::save(&mut out, &a.output)?;
    Ok(
        json!({"output": a.output, "pages": selected.len(), "source_pages": selected, "size_bytes": size}),
    )
}

pub fn split(a: SplitArgs) -> Result<Value> {
    let d = doc::load(&a.input, a.password.as_deref())?;
    let total = doc::page_ids(&d).len() as u32;
    let parts: Vec<Vec<u32>> = if a.ranges.is_empty() {
        let every = a.every.unwrap_or(1);
        if every == 0 {
            bail!("every must be at least 1");
        }
        (1..=total)
            .collect::<Vec<_>>()
            .chunks(every as usize)
            .map(<[u32]>::to_vec)
            .collect()
    } else {
        if a.every.is_some() {
            bail!("use either every or ranges, not both");
        }
        a.ranges
            .iter()
            .map(|r| pagespec::parse(r, total))
            .collect::<Result<_>>()?
    };
    std::fs::create_dir_all(&a.out_dir)?;
    let stem = a
        .input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "part".into());

    let outline = doc::outline(&d);
    let files: Vec<Value> = parts
        .par_iter()
        .enumerate()
        .map(|(i, pages)| {
            let path = a.out_dir.join(format!("{stem}-{:04}.pdf", i + 1));
            let mut out = assemble(&[Source {
                doc: &d,
                pages: pages.clone(),
                outline: &outline,
            }])?;
            let size = doc::save(&mut out, &path)?;
            Ok(json!({"file": path, "source_pages": pages, "size_bytes": size}))
        })
        .collect::<Result<_>>()?;
    Ok(json!({"parts": files.len(), "files": files}))
}
