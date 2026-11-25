//! Readers for a state document a UI process publishes (`notch-state.json`-style: a flat JSON
//! object only that process writes) and for the frontend-owned envelope documents beside it
//! (`hub-<name>.json`). The app names the file and the document namespace; nothing here knows a
//! product.
//!
//! Published state is read as a flat file with no `.bak`, a byte order mark tolerated and at most
//! [`PUBLISHED_MAX_BYTES`] bytes. A missing, damaged, oversized or non-object file reads as `None`
//! (callers use their default); the strict readers tell a missing file from a damaged one and never
//! replace or move a damaged file.

use crate::{
    read_with_options, Document, DocumentState, FileFormat, OnCorrupt, ReadOptions, Schema, Store,
    StoreOptions,
};
use serde_json::{json, Value};
use std::path::Path;

/// The largest published state file read: 16 MiB.
pub const PUBLISHED_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// The whole object in the published state file at `path`, or `None` when the file is missing,
/// damaged, oversized or not an object.
pub fn read_published_state(path: &Path) -> Option<Value> {
    let options = ReadOptions::new(FileFormat::Flat)
        .sidecars(false)
        .max_bytes(PUBLISHED_MAX_BYTES)
        .utf8_bom(true);
    read_with_options::<Value>(path, &json!({}), options)
        .ok()
        .filter(Value::is_object)
}

/// [`read_published_state`] when the object's `product` field equals `product`.
pub fn read_published_state_of(path: &Path, product: &str) -> Option<Value> {
    read_published_state(path).filter(|state| state["product"] == product)
}

/// The published state file for readers that must tell a missing file from a damaged one:
/// `Ok(None)` when it does not exist, `Err` when it exists but is damaged, oversized or
/// unreadable, otherwise the whole object. Opened read-only: it creates and writes nothing.
pub fn read_published_state_strict(path: &Path, namespace: &str) -> Result<Option<Value>, String> {
    Ok(open_published_state(path, namespace)?.map(|store| store.get()))
}

/// The read-only view [`read_published_state_strict`] reads through, for a caller that looks at the
/// file more than once: `get()` re-reads only when the file's stamp changed. `Ok(None)` when the
/// file does not exist. `namespace` is the store's schema namespace (version 1).
pub fn open_published_state(path: &Path, namespace: &str) -> Result<Option<Store<Value>>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let options = StoreOptions::new(Schema::new(namespace, 1))
        .format(FileFormat::Flat)
        .defaults_provider(|| json!({}))
        .on_corrupt(OnCorrupt::Refuse)
        .sidecars(false)
        .max_bytes(PUBLISHED_MAX_BYTES)
        .utf8_bom(true);
    Store::<Value>::open_read_only(path, options)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// A frontend-owned envelope document at `path` under `namespace`.
pub fn open_frontend_document(path: &Path, namespace: &str) -> Result<Document, String> {
    Document::open_path(path, namespace).map_err(|error| error.to_string())
}

/// A document's state as `{"values": <object>, "revision": <n>}`; a document not yet written is
/// `{"values": {}, "revision": 0}`.
pub fn document_snapshot(state: Option<DocumentState>) -> Value {
    match state {
        Some(state) => json!({"values": state.value, "revision": state.revision}),
        None => json!({"values": {}, "revision": 0}),
    }
}

/// The document at `path` as `{values, revision}`.
pub fn load_frontend_document(path: &Path, namespace: &str) -> Result<Value, String> {
    let document = open_frontend_document(path, namespace)?;
    let state = document.get().map_err(|error| error.to_string())?;
    Ok(document_snapshot(state))
}

/// Merge `patch` (top-level keys; `null` removes one) into the document at `path`, stored under
/// version 1, and return the result as `{values, revision}`.
pub fn patch_frontend_document(
    path: &Path,
    namespace: &str,
    patch: &Value,
) -> Result<Value, String> {
    let document = open_frontend_document(path, namespace)?;
    document
        .patch(1, patch, None)
        .map(|state| document_snapshot(Some(state)))
        .map_err(|error| error.to_string())
}
