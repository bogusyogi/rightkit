//! Page organisation beyond merge: delete, reorder, rotate and insert pages, keeping the outline
//! and the page labels (feature `merge`).
//!
//! Every operation takes document bytes and returns new document bytes. Nothing is changed in
//! place, so a caller keeps the input as its undo snapshot (copy on write at the byte level).
//! Each result is reloaded and checked before it is returned ([`OrganizeError::Verification`]):
//! the page count is the expected one, every page is a `/Page` whose `/Parent` is the one flat
//! `/Pages` node, outline destinations that name a page name one of the result's pages, and the
//! page-label tree reads back.
//!
//! Strategy (reference: pdfcraft `crates/organize`, MIT OR Apache-2.0; no code is copied): the
//! attributes a page inherits (`Resources`, `MediaBox`, `CropBox`, `Rotate`) are written onto each
//! page by [`crate::merge`]'s materialisation, then the old page tree is replaced by one flat
//! `/Pages` node. Page objects keep their ids, so outline items, link destinations and named
//! destinations that point at a surviving page stay valid.
//!
//! # What is kept, what is dropped
//!
//! - **Outline.** Kept. An item whose destination was a deleted page loses its `/Dest` (or its
//!   `GoTo` action) and stays as a plain heading with its children. Named destinations are not
//!   rewritten: a name that pointed at a deleted page now resolves to nothing.
//! - **Page labels.** Rebuilt for the new order: a label range is split wherever the new order
//!   breaks the old numbering (`/St` is adjusted so each page keeps its label). Inserted pages
//!   carry the labels of the document they came from, or their number in that document when it
//!   has none. A document without labels gets none.
//! - **Deleted pages** and everything only they reached (content, fonts, images) are pruned.
//!   References to a deleted page elsewhere (structure tree, destinations) become `null` first, so
//!   nothing keeps the page alive.
//! - **Inserted pages** bring their own objects; the source's outline, labels and forms are not
//!   imported.
//!
//! Encrypted inputs are refused. Signatures do not survive: the file is rewritten whole.

use std::collections::HashSet;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::inspect::{deref_obj, load_pdf, resolve_rotation};
use crate::merge::{load, materialize_inherited, MergeError, SharedValues};

#[derive(Debug, thiserror::Error)]
pub enum OrganizeError {
    #[error("document has no pages")]
    Empty,
    #[error("input could not be parsed: {0}")]
    Parse(String),
    #[error("input is encrypted")]
    Encrypted,
    #[error("page {0} is out of range")]
    PageOutOfRange(u32),
    #[error("page order is not a permutation of 1..={0}")]
    NotAPermutation(u32),
    #[error("deleting these pages would leave no page")]
    DeletesEveryPage,
    #[error("rotation {0} is not a multiple of 90 degrees")]
    InvalidRotation(i32),
    #[error("insert position {at} is past the end of a {pages}-page document")]
    InvalidPosition { at: u32, pages: u32 },
    #[error("no pages selected")]
    NoPagesSelected,
    #[error("result failed verification: {0}")]
    Verification(String),
    #[error("serialize failed: {0}")]
    Write(String),
}

impl OrganizeError {
    /// Stable snake_case code.
    pub fn code(&self) -> &'static str {
        match self {
            OrganizeError::Empty => "empty",
            OrganizeError::Parse(_) => "parse",
            OrganizeError::Encrypted => "encrypted",
            OrganizeError::PageOutOfRange(_) => "page_out_of_range",
            OrganizeError::NotAPermutation(_) => "not_a_permutation",
            OrganizeError::DeletesEveryPage => "deletes_every_page",
            OrganizeError::InvalidRotation(_) => "invalid_rotation",
            OrganizeError::InvalidPosition { .. } => "invalid_position",
            OrganizeError::NoPagesSelected => "no_pages_selected",
            OrganizeError::Verification(_) => "verification_failed",
            OrganizeError::Write(_) => "write",
        }
    }
}

fn from_merge(error: MergeError) -> OrganizeError {
    match error {
        MergeError::Encrypted(_) => OrganizeError::Encrypted,
        MergeError::Empty => OrganizeError::Empty,
        other => OrganizeError::Parse(other.to_string()),
    }
}

/// A loaded document whose pages carry their inherited attributes, in page order.
struct Open {
    doc: Document,
    pages: Vec<ObjectId>,
}

fn open(index: usize, bytes: &[u8]) -> Result<Open, OrganizeError> {
    let mut doc = load(index, bytes).map_err(from_merge)?;
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    if pages.is_empty() {
        return Err(OrganizeError::Empty);
    }
    let mut shared = SharedValues::new();
    for id in &pages {
        materialize_inherited(&mut doc, *id, &mut shared);
    }
    Ok(Open { doc, pages })
}

// ---------------------------------------------------------------------------------------------
// Page labels (ISO 32000-1 12.4.2)
// ---------------------------------------------------------------------------------------------

/// One page-label range: first page index (0-based) and its label dictionary (`/S`, `/P`, `/St`).
#[derive(Clone, Debug)]
struct LabelRange {
    start: usize,
    dict: Dictionary,
}

const MAX_TREE_DEPTH: usize = 32;

fn collect_nums(
    doc: &Document,
    node: &Dictionary,
    out: &mut Vec<LabelRange>,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth > MAX_TREE_DEPTH {
        return;
    }
    if let Some(Object::Array(nums)) = node.get(b"Nums").ok().and_then(|o| deref_obj(doc, o)) {
        for pair in nums.chunks(2) {
            let [key, value] = pair else { continue };
            let start = deref_obj(doc, key).and_then(|o| o.as_i64().ok());
            let dict = deref_obj(doc, value).and_then(|o| o.as_dict().ok());
            if let (Some(start), Some(dict)) = (start, dict) {
                if start >= 0 {
                    out.push(LabelRange {
                        start: start as usize,
                        dict: dict.clone(),
                    });
                }
            }
        }
    }
    if let Some(Object::Array(kids)) = node.get(b"Kids").ok().and_then(|o| deref_obj(doc, o)) {
        for kid in kids {
            let Object::Reference(id) = kid else { continue };
            if !visited.insert(*id) {
                continue;
            }
            if let Ok(Object::Dictionary(child)) = doc.get_object(*id) {
                collect_nums(doc, child, out, depth + 1, visited);
            }
        }
    }
}

fn read_labels(doc: &Document) -> Vec<LabelRange> {
    let mut out = Vec::new();
    let tree = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"PageLabels").ok())
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok());
    if let Some(tree) = tree {
        collect_nums(doc, tree, &mut out, 0, &mut HashSet::new());
    }
    out.sort_by_key(|range| range.start);
    out.dedup_by_key(|range| range.start);
    out
}

/// The range that covers page `index`: its start and dictionary. A page before the first range (or
/// in a document with none) is labelled by a default decimal range starting at page 0.
fn range_for(ranges: &[LabelRange], index: usize) -> (usize, Dictionary) {
    match ranges.iter().rev().find(|range| range.start <= index) {
        Some(range) => (range.start, range.dict.clone()),
        None => {
            let mut dict = Dictionary::new();
            dict.set("S", Object::Name(b"D".to_vec()));
            (0, dict)
        }
    }
}

/// Where a page of the result came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    /// 0-based page of the document being changed.
    Dest(usize),
    /// 0-based page of a document pages were inserted from.
    Source(usize),
}

/// The label ranges for `plan` (the result's pages in order), or `None` when neither document has
/// labels and the catalog must be left alone.
fn build_labels(
    plan: &[Origin],
    dest: &[LabelRange],
    source: &[LabelRange],
) -> Option<Vec<LabelRange>> {
    if dest.is_empty() && source.is_empty() {
        return None;
    }
    let mut out: Vec<LabelRange> = Vec::new();
    let mut previous: Option<(Origin, usize)> = None;
    for (new_index, origin) in plan.iter().enumerate() {
        let (ranges, old_index) = match origin {
            Origin::Dest(i) => (dest, *i),
            Origin::Source(i) => (source, *i),
        };
        let (range_start, dict) = range_for(ranges, old_index);
        let continues = match (previous, origin) {
            (Some((Origin::Dest(p), start)), Origin::Dest(i)) => p + 1 == *i && start == range_start,
            (Some((Origin::Source(p), start)), Origin::Source(i)) => {
                p + 1 == *i && start == range_start
            }
            _ => false,
        };
        if !continues {
            // The page keeps the label it had: its number within its old range moves into `/St`.
            let mut dict = dict;
            let base = dict
                .get(b"St")
                .ok()
                .and_then(|o| o.as_i64().ok())
                .unwrap_or(1);
            dict.set(
                "St",
                Object::Integer(base + old_index.saturating_sub(range_start) as i64),
            );
            out.push(LabelRange {
                start: new_index,
                dict,
            });
        }
        previous = Some((*origin, range_start));
    }
    Some(out)
}

fn labels_object(ranges: &[LabelRange]) -> Object {
    let mut nums = Vec::with_capacity(ranges.len() * 2);
    for range in ranges {
        nums.push(Object::Integer(range.start as i64));
        nums.push(Object::Dictionary(range.dict.clone()));
    }
    let mut tree = Dictionary::new();
    tree.set("Nums", Object::Array(nums));
    Object::Dictionary(tree)
}

// ---------------------------------------------------------------------------------------------
// Outline
// ---------------------------------------------------------------------------------------------

/// Every outline item id, bounded and cycle-safe.
fn outline_items(doc: &Document) -> Vec<ObjectId> {
    let root = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Outlines").ok())
        .and_then(|o| o.as_reference().ok());
    let mut items = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<ObjectId> = Vec::new();
    if let Some(root) = root {
        if let Ok(dict) = doc.get_dictionary(root) {
            if let Ok(Object::Reference(first)) = dict.get(b"First") {
                stack.push(*first);
            }
        }
    }
    while let Some(id) = stack.pop() {
        if !seen.insert(id) || items.len() >= 1_000_000 {
            continue;
        }
        let Ok(dict) = doc.get_dictionary(id) else {
            continue;
        };
        items.push(id);
        for key in [&b"First"[..], b"Next"] {
            if let Ok(Object::Reference(next)) = dict.get(key) {
                stack.push(*next);
            }
        }
    }
    items
}

/// True for a destination array whose page was nulled.
fn destination_is_dead(destination: Option<&Object>) -> bool {
    matches!(destination, Some(Object::Array(items)) if matches!(items.first(), Some(Object::Null)))
}

fn clean_dead_destinations(doc: &mut Document) {
    for id in outline_items(doc) {
        let Ok(Object::Dictionary(item)) = doc.get_object_mut(id) else {
            continue;
        };
        if destination_is_dead(item.get(b"Dest").ok()) {
            item.remove(b"Dest");
        }
        let dead_action = match item.get(b"A") {
            Ok(Object::Dictionary(action)) => {
                action
                    .get(b"S")
                    .ok()
                    .and_then(|s| s.as_name().ok())
                    .is_some_and(|name| name == &b"GoTo"[..])
                    && destination_is_dead(action.get(b"D").ok())
            }
            _ => false,
        };
        if dead_action {
            item.remove(b"A");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Assembly and verification
// ---------------------------------------------------------------------------------------------

fn root_pages_id(doc: &Document) -> Option<ObjectId> {
    doc.catalog()
        .ok()
        .and_then(|c| c.get(b"Pages").ok())
        .and_then(|o| o.as_reference().ok())
}

/// Replaces the page tree by one flat node over `new_pages`, applies `labels`, drops what only
/// `removed` pages reached, and serialises.
fn finish(
    mut doc: Document,
    new_pages: &[ObjectId],
    removed: &HashSet<ObjectId>,
    labels: Option<Vec<LabelRange>>,
) -> Result<Vec<u8>, OrganizeError> {
    let root_id = doc
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|o| o.as_reference().ok())
        .ok_or_else(|| OrganizeError::Parse("no catalog".to_string()))?;
    let pages_id = match root_pages_id(&doc) {
        Some(id) => id,
        None => {
            let id = doc.new_object_id();
            if let Ok(Object::Dictionary(catalog)) = doc.get_object_mut(root_id) {
                catalog.set("Pages", Object::Reference(id));
            }
            id
        }
    };
    let kids: Vec<Object> = new_pages.iter().map(|id| Object::Reference(*id)).collect();
    let mut node = Dictionary::new();
    node.set("Type", Object::Name(b"Pages".to_vec()));
    node.set("Count", Object::Integer(kids.len() as i64));
    node.set("Kids", Object::Array(kids));
    doc.objects.insert(pages_id, Object::Dictionary(node));
    for id in new_pages {
        if let Ok(Object::Dictionary(page)) = doc.get_object_mut(*id) {
            page.set("Parent", Object::Reference(pages_id));
        }
    }
    if let Some(ranges) = labels {
        if let Ok(Object::Dictionary(catalog)) = doc.get_object_mut(root_id) {
            if ranges.is_empty() {
                catalog.remove(b"PageLabels");
            } else {
                catalog.set("PageLabels", labels_object(&ranges));
            }
        }
    }
    if !removed.is_empty() {
        let _ = doc.traverse_objects(|object| {
            if let Object::Reference(id) = object {
                if removed.contains(&*id) {
                    *object = Object::Null;
                }
            }
        });
        clean_dead_destinations(&mut doc);
    }
    doc.prune_objects();
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .map_err(|e| OrganizeError::Write(e.to_string()))?;
    verify_structure(&bytes, new_pages.len())?;
    Ok(bytes)
}

/// The structural check every result passes (see the module documentation). Public so a host can
/// run it on a result it is about to stage.
pub fn verify_structure(bytes: &[u8], expected_pages: usize) -> Result<(), OrganizeError> {
    let fail = |message: String| OrganizeError::Verification(message);
    let doc = load_pdf(bytes).map_err(|e| fail(format!("reload failed: {e}")))?;
    let pages = doc.get_pages();
    if pages.len() != expected_pages {
        return Err(fail(format!(
            "expected {expected_pages} pages, found {}",
            pages.len()
        )));
    }
    let root = root_pages_id(&doc).ok_or_else(|| fail("catalog has no /Pages".to_string()))?;
    let mut seen: HashSet<ObjectId> = HashSet::new();
    for (number, id) in &pages {
        if !seen.insert(*id) {
            return Err(fail(format!("page {number} repeats a page object")));
        }
        let dict = doc
            .get_dictionary(*id)
            .map_err(|_| fail(format!("page {number} is not a dictionary")))?;
        if !dict.has_type(b"Page") {
            return Err(fail(format!("page {number} is not a /Page")));
        }
        let parent = dict.get(b"Parent").ok().and_then(|p| p.as_reference().ok());
        if parent != Some(root) {
            return Err(fail(format!("page {number} is not under the root /Pages")));
        }
    }
    let count = doc
        .get_dictionary(root)
        .ok()
        .and_then(|d| d.get(b"Count").ok())
        .and_then(|c| c.as_i64().ok());
    if count != Some(expected_pages as i64) {
        return Err(fail("root /Pages /Count is wrong".to_string()));
    }
    for id in outline_items(&doc) {
        let Ok(item) = doc.get_dictionary(id) else {
            continue;
        };
        let mut destinations: Vec<&Object> = Vec::new();
        if let Ok(destination) = item.get(b"Dest") {
            destinations.push(destination);
        }
        if let Ok(Object::Dictionary(action)) = item.get(b"A") {
            if let Ok(destination) = action.get(b"D") {
                destinations.push(destination);
            }
        }
        for destination in destinations {
            if let Object::Array(array) = destination {
                if let Some(Object::Reference(page)) = array.first() {
                    if !seen.contains(page) {
                        return Err(fail("an outline item points at a missing page".to_string()));
                    }
                }
            }
        }
    }
    let labels = read_labels(&doc);
    if labels.iter().any(|range| range.start >= expected_pages) {
        return Err(fail("a page-label range starts past the last page".to_string()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------------------------

/// Deletes `pages` (1-based, repeats ignored). Deleting every page is refused.
pub fn delete_pages(source: &[u8], pages: &[u32]) -> Result<Vec<u8>, OrganizeError> {
    let Open { doc, pages: all } = open(0, source)?;
    let count = all.len();
    let mut drop: HashSet<usize> = HashSet::new();
    for &page in pages {
        if page == 0 || page as usize > count {
            return Err(OrganizeError::PageOutOfRange(page));
        }
        drop.insert(page as usize - 1);
    }
    if drop.is_empty() {
        return Err(OrganizeError::NoPagesSelected);
    }
    if drop.len() == count {
        return Err(OrganizeError::DeletesEveryPage);
    }
    let dest_labels = read_labels(&doc);
    let mut kept = Vec::new();
    let mut plan = Vec::new();
    let mut removed = HashSet::new();
    for (index, id) in all.iter().enumerate() {
        if drop.contains(&index) {
            removed.insert(*id);
        } else {
            kept.push(*id);
            plan.push(Origin::Dest(index));
        }
    }
    let labels = build_labels(&plan, &dest_labels, &[]);
    finish(doc, &kept, &removed, labels)
}

/// Puts the pages in `order`: a permutation of `1..=page_count` where entry `i` is the old number
/// of the page that becomes page `i + 1`.
pub fn reorder_pages(source: &[u8], order: &[u32]) -> Result<Vec<u8>, OrganizeError> {
    let Open { doc, pages: all } = open(0, source)?;
    let count = all.len();
    let mut seen = vec![false; count];
    if order.len() != count {
        return Err(OrganizeError::NotAPermutation(count as u32));
    }
    for &page in order {
        let index = (page as usize)
            .checked_sub(1)
            .filter(|index| *index < count)
            .ok_or(OrganizeError::NotAPermutation(count as u32))?;
        if std::mem::replace(&mut seen[index], true) {
            return Err(OrganizeError::NotAPermutation(count as u32));
        }
    }
    let dest_labels = read_labels(&doc);
    let plan: Vec<Origin> = order
        .iter()
        .map(|page| Origin::Dest(*page as usize - 1))
        .collect();
    let new_pages: Vec<ObjectId> = order.iter().map(|page| all[*page as usize - 1]).collect();
    let labels = build_labels(&plan, &dest_labels, &[]);
    finish(doc, &new_pages, &HashSet::new(), labels)
}

/// Turns `pages` (1-based) clockwise by `degrees`, a multiple of 90 (negative turns the other
/// way), added to each page's effective `/Rotate`.
pub fn rotate_pages(source: &[u8], pages: &[u32], degrees: i32) -> Result<Vec<u8>, OrganizeError> {
    if degrees % 90 != 0 {
        return Err(OrganizeError::InvalidRotation(degrees));
    }
    let Open { mut doc, pages: all } = open(0, source)?;
    if pages.is_empty() {
        return Err(OrganizeError::NoPagesSelected);
    }
    let mut targets: Vec<usize> = Vec::new();
    for &page in pages {
        if page == 0 || page as usize > all.len() {
            return Err(OrganizeError::PageOutOfRange(page));
        }
        if !targets.contains(&(page as usize - 1)) {
            targets.push(page as usize - 1);
        }
    }
    for index in targets {
        let id = all[index];
        let current = doc
            .get_object(id)
            .ok()
            .and_then(|o| o.as_dict().ok())
            .map(|page| resolve_rotation(&doc, page))
            .unwrap_or(0);
        let next = (current + degrees).rem_euclid(360);
        if let Ok(Object::Dictionary(page)) = doc.get_object_mut(id) {
            page.set("Rotate", Object::Integer(i64::from(next)));
        }
    }
    finish(doc, &all, &HashSet::new(), None)
}

/// Inserts pages of `source` (all of them, or the 1-based `source_pages` in that order, repeats
/// allowed) into `dest` so that `at` pages of `dest` come before them (`0` inserts at the start,
/// the page count appends).
pub fn insert_pages(
    dest: &[u8],
    at: u32,
    source: &[u8],
    source_pages: Option<&[u32]>,
) -> Result<Vec<u8>, OrganizeError> {
    let Open {
        mut doc,
        pages: dest_pages,
    } = open(0, dest)?;
    if at as usize > dest_pages.len() {
        return Err(OrganizeError::InvalidPosition {
            at,
            pages: dest_pages.len() as u32,
        });
    }
    let Open {
        doc: mut from,
        pages: source_all,
    } = open(1, source)?;
    let selection: Vec<usize> = match source_pages {
        None => (0..source_all.len()).collect(),
        Some([]) => return Err(OrganizeError::NoPagesSelected),
        Some(list) => {
            let mut picked = Vec::with_capacity(list.len());
            for &page in list {
                if page == 0 || page as usize > source_all.len() {
                    return Err(OrganizeError::PageOutOfRange(page));
                }
                picked.push(page as usize - 1);
            }
            picked
        }
    };
    let dest_labels = read_labels(&doc);
    let source_labels = read_labels(&from);
    from.renumber_objects_with(doc.max_id + 1);
    let source_ids: Vec<ObjectId> = from.get_pages().into_values().collect();
    doc.max_id = doc.max_id.max(from.max_id);
    doc.objects.extend(from.objects);

    let mut inserted: Vec<ObjectId> = Vec::with_capacity(selection.len());
    let mut used: HashSet<ObjectId> = HashSet::new();
    for index in &selection {
        let id = *source_ids
            .get(*index)
            .ok_or(OrganizeError::PageOutOfRange(*index as u32 + 1))?;
        if used.insert(id) {
            inserted.push(id);
        } else {
            // A repeated page: a copy of the page dictionary under a new id (content is shared).
            let copy = doc
                .get_object(id)
                .cloned()
                .map_err(|_| OrganizeError::PageOutOfRange(*index as u32 + 1))?;
            let new_id = doc.new_object_id();
            doc.objects.insert(new_id, copy);
            inserted.push(new_id);
        }
    }

    let at = at as usize;
    let mut new_pages: Vec<ObjectId> = Vec::new();
    let mut plan: Vec<Origin> = Vec::new();
    for (index, id) in dest_pages.iter().enumerate().take(at) {
        new_pages.push(*id);
        plan.push(Origin::Dest(index));
    }
    for (id, index) in inserted.iter().zip(&selection) {
        new_pages.push(*id);
        plan.push(Origin::Source(*index));
    }
    for (index, id) in dest_pages.iter().enumerate().skip(at) {
        new_pages.push(*id);
        plan.push(Origin::Dest(index));
    }
    let labels = build_labels(&plan, &dest_labels, &source_labels);
    finish(doc, &new_pages, &HashSet::new(), labels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect::page_graph_from_bytes;
    use lopdf::{dictionary, Stream, StringFormat};

    /// `n` pages, page `k` showing "P{k}"; optional label ranges (roman for the first two pages,
    /// decimal after) and one outline item per page.
    fn numbered(n: usize, labels: bool, outline: bool) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let mut page_ids = Vec::new();
        for k in 1..=n {
            let content = doc.add_object(Stream::new(
                Dictionary::new(),
                format!("BT /F1 12 Tf 72 700 Td (P{k}) Tj ET").into_bytes(),
            ));
            page_ids.push(doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => pages_id, "Contents" => content,
                "Resources" => resources,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792),
                ]),
            }));
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(page_ids.iter().map(|id| Object::Reference(*id)).collect()),
                "Count" => n as i64,
            }),
        );
        let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => pages_id };
        if labels {
            catalog.set(
                "PageLabels",
                dictionary! { "Nums" => Object::Array(vec![
                    Object::Integer(0), Object::Dictionary(dictionary! { "S" => "r" }),
                    Object::Integer(2), Object::Dictionary(dictionary! { "S" => "D" }),
                ]) },
            );
        }
        if outline {
            let outlines = doc.new_object_id();
            let items: Vec<ObjectId> = (0..n).map(|_| doc.new_object_id()).collect();
            for (k, id) in items.iter().enumerate() {
                let mut item = dictionary! {
                    "Title" => Object::String(format!("Item {}", k + 1).into_bytes(), StringFormat::Literal),
                    "Parent" => outlines,
                    "Dest" => Object::Array(vec![
                        Object::Reference(page_ids[k]), Object::Name(b"Fit".to_vec()),
                    ]),
                };
                if k + 1 < n {
                    item.set("Next", Object::Reference(items[k + 1]));
                }
                if k > 0 {
                    item.set("Prev", Object::Reference(items[k - 1]));
                }
                doc.objects.insert(*id, Object::Dictionary(item));
            }
            doc.objects.insert(
                outlines,
                Object::Dictionary(dictionary! {
                    "Type" => "Outlines", "First" => items[0], "Last" => items[n - 1],
                    "Count" => n as i64,
                }),
            );
            catalog.set("Outlines", Object::Reference(outlines));
        }
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("fixture saves");
        out
    }

    fn texts(bytes: &[u8]) -> Vec<String> {
        let count = load_pdf(bytes).unwrap().get_pages().len();
        (1..=count as u32)
            .map(|page| {
                page_graph_from_bytes(bytes, page)
                    .unwrap()
                    .islands
                    .iter()
                    .map(|i| i.text.clone())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    #[test]
    fn delete_keeps_the_others_and_drops_the_dead_outline_destination() {
        let out = delete_pages(&numbered(4, false, true), &[2]).unwrap();
        assert_eq!(texts(&out), ["P1", "P3", "P4"]);
        let doc = load_pdf(&out).unwrap();
        let items = outline_items(&doc);
        assert_eq!(items.len(), 4, "the heading stays");
        let with_dest = items
            .iter()
            .filter(|id| doc.get_dictionary(**id).unwrap().has(b"Dest"))
            .count();
        assert_eq!(with_dest, 3);
    }

    #[test]
    fn deleted_page_content_does_not_survive() {
        let out = delete_pages(&numbered(3, false, true), &[2]).unwrap();
        let doc = load_pdf(&out).unwrap();
        let has_p2 = doc.objects.values().any(|o| {
            o.as_stream()
                .ok()
                .and_then(|s| s.decompressed_content().ok())
                .is_some_and(|c| c.windows(4).any(|w| w == b"(P2)"))
        });
        assert!(!has_p2);
    }

    #[test]
    fn reorder_rebuilds_the_label_ranges() {
        let out = reorder_pages(&numbered(4, true, false), &[3, 4, 1, 2]).unwrap();
        assert_eq!(texts(&out), ["P3", "P4", "P1", "P2"]);
        let labels = read_labels(&load_pdf(&out).unwrap());
        assert_eq!(labels.iter().map(|r| r.start).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(
            labels[0].dict.get(b"S").unwrap().as_name().unwrap(),
            b"D"
        );
        assert_eq!(
            labels[1].dict.get(b"S").unwrap().as_name().unwrap(),
            b"r"
        );
    }

    #[test]
    fn delete_shifts_st_so_pages_keep_their_labels() {
        // Pages 3 and 4 are decimal 1 and 2; deleting page 3 leaves page 4 as decimal 2.
        let out = delete_pages(&numbered(4, true, false), &[3]).unwrap();
        let labels = read_labels(&load_pdf(&out).unwrap());
        let last = labels.last().unwrap();
        assert_eq!(last.start, 2);
        assert_eq!(last.dict.get(b"St").unwrap().as_i64().unwrap(), 2);
    }

    #[test]
    fn rotate_adds_to_the_effective_rotation() {
        let source = numbered(2, false, false);
        let once = rotate_pages(&source, &[1], 90).unwrap();
        let twice = rotate_pages(&once, &[1], 270).unwrap();
        let rotation = |bytes: &[u8], page: u32| {
            let doc = load_pdf(bytes).unwrap();
            let id = doc.get_pages()[&page];
            resolve_rotation(&doc, doc.get_dictionary(id).unwrap())
        };
        assert_eq!(rotation(&once, 1), 90);
        assert_eq!(rotation(&once, 2), 0);
        assert_eq!(rotation(&twice, 1), 0);
    }

    #[test]
    fn insert_places_pages_and_keeps_both_documents_text() {
        let dest = numbered(2, false, true);
        let source = numbered(3, false, false);
        let out = insert_pages(&dest, 1, &source, Some(&[3, 3])).unwrap();
        assert_eq!(texts(&out), ["P1", "P3", "P3", "P2"]);
        let appended = insert_pages(&dest, 2, &source, None).unwrap();
        assert_eq!(texts(&appended), ["P1", "P2", "P1", "P2", "P3"]);
    }

    #[test]
    fn bad_requests_are_typed() {
        let source = numbered(3, false, false);
        assert!(matches!(
            reorder_pages(&source, &[1, 1, 2]),
            Err(OrganizeError::NotAPermutation(3))
        ));
        assert!(matches!(
            delete_pages(&source, &[1, 2, 3]),
            Err(OrganizeError::DeletesEveryPage)
        ));
        assert!(matches!(
            delete_pages(&source, &[4]),
            Err(OrganizeError::PageOutOfRange(4))
        ));
        assert!(matches!(
            rotate_pages(&source, &[1], 45),
            Err(OrganizeError::InvalidRotation(45))
        ));
        assert!(matches!(
            insert_pages(&source, 9, &source, None),
            Err(OrganizeError::InvalidPosition { at: 9, pages: 3 })
        ));
    }
}
