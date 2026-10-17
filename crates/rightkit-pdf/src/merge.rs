//! Page-level assembly: merge several PDFs into one and select/reorder pages.
//!
//! Donor: Legion's `merge_page_pdfs` (lopdf renumber-and-splice recipe). Hardened: attributes a page
//! inherits from its `/Pages` ancestors (`Resources`, `MediaBox`, `CropBox`, `Rotate`) are
//! materialized onto the page before the old page tree is discarded, and unreachable objects
//! (old catalogs and page trees) are pruned.
//!
//! Materializing never copies a value per page. A value an ancestor holds directly (a `/Resources`
//! dictionary, a box array) is turned into **one indirect object** the first time any page below that
//! ancestor needs it, and every such page gets a reference to it, so ten thousand pages under one
//! node with a large `/Resources` cost ten thousand references and one copy, not ten thousand
//! copies. A value that is already a reference, or a number or boolean, is copied as is.

use std::collections::BTreeMap;

use lopdf::{Dictionary, Document, Object, ObjectId};

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("no input documents")]
    Empty,
    #[error("input {index} could not be parsed: {message}")]
    Parse { index: usize, message: String },
    #[error("input {0} is encrypted")]
    Encrypted(usize),
    #[error("page {0} is out of range")]
    PageOutOfRange(u32),
    #[error("serialize failed: {0}")]
    Write(String),
}

const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

pub(crate) fn load(index: usize, bytes: &[u8]) -> Result<Document, MergeError> {
    // The same bounded loader as inspection: no stream (an object stream included) may decode past
    // `MAX_DECOMPRESSED_BYTES`.
    let doc = crate::inspect::load_pdf(bytes).map_err(|e| MergeError::Parse {
        index,
        message: e.to_string(),
    })?;
    if doc.is_encrypted() {
        return Err(MergeError::Encrypted(index));
    }
    Ok(doc)
}

/// The indirect object made for an inherited value, by the ancestor that holds it and which of
/// [`INHERITED`] it is. A page that inherits the same value gets this and nothing else.
pub(crate) type SharedValues = BTreeMap<(ObjectId, usize), Object>;

/// Sets on `page_id` the attributes it inherits. A direct value on an ancestor is cloned **once**
/// into an indirect object kept in `shared` (the clone is of a value the document already holds, so
/// the total is bounded by the document); every page below that ancestor then gets the same
/// reference.
pub(crate) fn materialize_inherited(doc: &mut Document, page_id: ObjectId, shared: &mut SharedValues) {
    // Only asked about, never copied: which attributes the page has itself and where its parent is.
    let (parent, has) = match doc.get_object(page_id).and_then(|o| o.as_dict()) {
        Ok(page) => (
            page.get(b"Parent").ok().and_then(|p| p.as_reference().ok()),
            INHERITED.map(|key| page.has(key)),
        ),
        Err(_) => return,
    };
    let mut found: Vec<(&[u8], Object)> = Vec::new();
    for (index, &key) in INHERITED.iter().enumerate() {
        if has[index] {
            continue;
        }
        let mut holder: Option<ObjectId> = None;
        let mut cursor = parent;
        let mut hops = 0;
        while let Some(id) = cursor {
            hops += 1;
            if hops > 64 {
                break;
            }
            let Ok(node) = doc.get_object(id).and_then(|o| o.as_dict()) else {
                break;
            };
            if node.has(key) {
                holder = Some(id);
                break;
            }
            cursor = node.get(b"Parent").ok().and_then(|p| p.as_reference().ok());
        }
        let Some(holder) = holder else { continue };
        let value = if let Some(value) = shared.get(&(holder, index)).cloned() {
            value
        } else {
            let direct = doc
                .get_object(holder)
                .ok()
                .and_then(|o| o.as_dict().ok())
                .and_then(|node| node.get(key).ok())
                .cloned();
            let value = match direct {
                None => continue,
                // A reference or a number costs the same shared or copied.
                Some(
                    plain @ (Object::Reference(_)
                    | Object::Integer(_)
                    | Object::Real(_)
                    | Object::Boolean(_)
                    | Object::Null),
                ) => plain,
                Some(container) => Object::Reference(doc.add_object(container)),
            };
            shared.insert((holder, index), value.clone());
            value
        };
        found.push((key, value));
    }
    if let Ok(Object::Dictionary(dict)) = doc.get_object_mut(page_id) {
        for (key, value) in found {
            dict.set(key, value);
        }
    }
}

/// Merges `inputs` in order into one document (one page tree, pages in input order).
pub fn merge_pdfs(inputs: &[&[u8]]) -> Result<Vec<u8>, MergeError> {
    if inputs.is_empty() {
        return Err(MergeError::Empty);
    }
    let mut max_id: u32 = 1;
    let mut page_ids: Vec<ObjectId> = Vec::new();
    let mut objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
    for (index, bytes) in inputs.iter().enumerate() {
        let mut doc = load(index, bytes)?;
        let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
        let mut shared = SharedValues::new();
        for id in &pages {
            materialize_inherited(&mut doc, *id, &mut shared);
        }
        // Renumber moves page ids too; recompute them from the renumbered document.
        doc.renumber_objects_with(max_id);
        max_id = doc.max_id + 1;
        page_ids.extend(doc.get_pages().into_values());
        objects.extend(doc.objects);
    }
    assemble(objects, page_ids)
}

/// Builds a new document containing `pages` (1-based, any order, repeats allowed) of `source`.
pub fn select_pages(source: &[u8], pages: &[u32]) -> Result<Vec<u8>, MergeError> {
    if pages.is_empty() {
        return Err(MergeError::Empty);
    }
    let mut doc = load(0, source)?;
    let map = doc.get_pages();
    let mut shared = SharedValues::new();
    for id in map.values().copied().collect::<Vec<_>>() {
        materialize_inherited(&mut doc, id, &mut shared);
    }
    let mut picked = Vec::with_capacity(pages.len());
    let mut objects = doc.objects.clone();
    let mut next = doc.max_id;
    for &p in pages {
        let id = *map.get(&p).ok_or(MergeError::PageOutOfRange(p))?;
        if picked.contains(&id) {
            // Repeated page: clone the page dictionary under a fresh id (shares content streams).
            next += 1;
            let new_id = (next, 0);
            let copy = objects
                .get(&id)
                .cloned()
                .ok_or(MergeError::PageOutOfRange(p))?;
            objects.insert(new_id, copy);
            picked.push(new_id);
        } else {
            picked.push(id);
        }
    }
    assemble(objects.into_iter().collect(), picked)
}

fn assemble(
    objects: BTreeMap<ObjectId, Object>,
    page_ids: Vec<ObjectId>,
) -> Result<Vec<u8>, MergeError> {
    let mut merged = Document::with_version("1.7");
    merged.objects = objects;
    merged.max_id = merged.objects.keys().map(|id| id.0).max().unwrap_or(1);
    let pages_id = merged.new_object_id();
    let kids: Vec<Object> = page_ids.iter().map(|id| Object::Reference(*id)).collect();
    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set("Count", Object::Integer(kids.len() as i64));
    pages_dict.set("Kids", Object::Array(kids));
    merged
        .objects
        .insert(pages_id, Object::Dictionary(pages_dict));
    for id in &page_ids {
        if let Ok(Object::Dictionary(dict)) = merged.get_object_mut(*id) {
            dict.set("Parent", Object::Reference(pages_id));
        }
    }
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    let catalog_id = merged.add_object(Object::Dictionary(catalog));
    merged.trailer.set("Root", catalog_id);
    merged.prune_objects();
    merged.renumber_objects();
    merged.compress();
    let mut out = Vec::new();
    merged
        .save_to(&mut out)
        .map_err(|e| MergeError::Write(e.to_string()))?;
    Ok(out)
}
