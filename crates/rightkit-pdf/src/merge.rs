//! Page-level assembly: merge several PDFs into one and select/reorder pages.
//!
//! Donor: Legion's `merge_page_pdfs` (lopdf renumber-and-splice recipe). Hardened: attributes a page
//! inherits from its `/Pages` ancestors (`Resources`, `MediaBox`, `CropBox`, `Rotate`) are
//! materialized onto the page before the old page tree is discarded, and unreachable objects
//! (old catalogs and page trees) are pruned.

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

/// Most direct entries (dictionary entries and array items) the inherited attributes copied onto
/// pages may total across one document. A `/Resources` held directly on a `/Pages` node is cloned
/// onto every page below it, so thousands of pages over a large direct dictionary would otherwise
/// multiply it; an indirect one costs a reference per page and is not counted.
const MAX_MATERIALIZED_ENTRIES: usize = 1 << 21;

const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

fn load(index: usize, bytes: &[u8]) -> Result<Document, MergeError> {
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

/// Copies the attributes `page_id` inherits onto the page itself. `budget` is the entries still
/// available to copy for the whole document (see [`MAX_MATERIALIZED_ENTRIES`]); it is charged
/// before each value is cloned.
fn materialize_inherited(
    doc: &mut Document,
    page_id: ObjectId,
    index: usize,
    budget: &mut usize,
) -> Result<(), MergeError> {
    let mut found: Vec<(&[u8], Object)> = Vec::new();
    let own = match doc.get_object(page_id).and_then(|o| o.as_dict()) {
        Ok(d) => d.clone(),
        Err(_) => return Ok(()),
    };
    for key in INHERITED {
        if own.has(key) {
            continue;
        }
        let mut cursor = own.get(b"Parent").ok().and_then(|p| p.as_reference().ok());
        let mut hops = 0;
        while let Some(id) = cursor {
            hops += 1;
            if hops > 64 {
                break;
            }
            let Ok(node) = doc.get_object(id).and_then(|o| o.as_dict()) else {
                break;
            };
            if let Ok(value) = node.get(key) {
                let cost = match value {
                    Object::Dictionary(dict) => dict.len(),
                    Object::Array(items) => items.len(),
                    _ => 1,
                };
                *budget = budget.checked_sub(cost).ok_or_else(|| MergeError::Parse {
                    index,
                    message: "inherited page attributes are too large to copy".to_string(),
                })?;
                found.push((key, value.clone()));
                break;
            }
            cursor = node.get(b"Parent").ok().and_then(|p| p.as_reference().ok());
        }
    }
    if let Ok(Object::Dictionary(dict)) = doc.get_object_mut(page_id) {
        for (key, value) in found {
            dict.set(key, value);
        }
    }
    Ok(())
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
        let mut budget = MAX_MATERIALIZED_ENTRIES;
        for id in &pages {
            materialize_inherited(&mut doc, *id, index, &mut budget)?;
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
    let mut budget = MAX_MATERIALIZED_ENTRIES;
    for id in map.values().copied().collect::<Vec<_>>() {
        materialize_inherited(&mut doc, id, 0, &mut budget)?;
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
