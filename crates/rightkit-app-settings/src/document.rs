//! Frontend-owned settings documents: an opaque JSON object whose schema and migrations live in
//! the app's frontend (for example TypeScript). The crate stores it in the same versioned envelope,
//! with the same atomic write, `.bak`, corruption recovery and cross-process lock as [`crate::Store`],
//! but never interprets the fields and never migrates: the frontend states the version it writes.

use crate::backend::{Edit, FileBackend, SettingsBackend, Stored};
use crate::options::{FileFormat, StoreOptions};
use crate::store::Error;
use serde_json::{Map, Value};
use std::path::Path;

/// One committed frontend-owned document.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentState {
    /// The version the frontend stored it under. `0` for a legacy flat file adopted as an
    /// envelope (a file with no version has not been written by a frontend yet).
    pub version: u32,
    /// The document, always a JSON object.
    pub value: Value,
    /// Committed revision; pass it as `expected_revision` for compare-and-swap writes.
    pub revision: u64,
}

/// A single settings document whose shape the frontend owns.
///
/// Typical frontend flow (through the app's commands): `get`; if `None` or `version` is older than
/// the frontend's current one, migrate in the frontend and `put` with the new version and the
/// `revision` that was read; otherwise use `value`. Later edits use `patch` (top-level merge) or
/// `put`.
pub struct Document {
    backend: Box<dyn SettingsBackend>,
    namespace: String,
}

impl Document {
    /// Open the document at `path` (a versioned envelope). Nothing is created until the first write.
    pub fn open_path(path: impl AsRef<Path>, namespace: impl Into<String>) -> Result<Self, Error> {
        let namespace = namespace.into();
        validate_namespace(&namespace)?;
        let backend = FileBackend::new(path.as_ref().to_path_buf(), FileFormat::Envelope);
        Ok(Self {
            backend: Box::new(backend),
            namespace,
        })
    }

    /// As [`Document::open_path`], configured by the file-level options a [`crate::Store`] takes
    /// (corruption policy, sidecars, replace mode, key order, size cap, ...). The options' schema
    /// supplies the namespace; its version and migrations are not used. Fails unless the options
    /// use [`FileFormat::Envelope`], the only format that records a version.
    pub fn open_path_with(path: impl AsRef<Path>, options: &StoreOptions) -> Result<Self, Error> {
        if options.format != FileFormat::Envelope {
            return Err(Error::Invalid(
                "a frontend-owned document needs FileFormat::Envelope to record its version".into(),
            ));
        }
        let namespace = options.schema.namespace.clone();
        validate_namespace(&namespace)?;
        let backend = FileBackend::with_options(path.as_ref().to_path_buf(), options);
        Ok(Self {
            backend: Box::new(backend),
            namespace,
        })
    }

    /// Open over a custom [`SettingsBackend`].
    pub fn with_backend(
        backend: impl SettingsBackend + 'static,
        namespace: impl Into<String>,
    ) -> Result<Self, Error> {
        let namespace = namespace.into();
        validate_namespace(&namespace)?;
        Ok(Self {
            backend: Box::new(backend),
            namespace,
        })
    }

    /// The committed document, or `None` when nothing has been written. Reads the backup when the
    /// file is missing or damaged, like [`crate::Store`]; nothing is written.
    pub fn get(&self) -> Result<Option<DocumentState>, Error> {
        Ok(self.backend.load()?.map(state_of))
    }

    /// Replace the whole document with `value` (a JSON object), stored under `version`.
    ///
    /// With `expected_revision` the write fails with [`Error::Conflict`] unless the committed
    /// revision still equals it (`0` when nothing is stored). A `version` lower than the stored
    /// one is refused with [`Error::NewerSchema`]: an older frontend never overwrites a newer
    /// document.
    pub fn put(
        &self,
        version: u32,
        value: Value,
        expected_revision: Option<u64>,
    ) -> Result<DocumentState, Error> {
        let values = into_object(value)?;
        self.commit(version, expected_revision, move |_| Ok(Some(values)))
            .map(|s| s.expect("put always commits"))
    }

    /// Merge `patch` (a JSON object) into the document at the top level: each key replaces the
    /// stored one, and `null` removes it. Nested objects are replaced whole, not merged. The write
    /// is stored under `version` (same downgrade refusal and `expected_revision` as
    /// [`Document::put`]); a document that does not exist yet starts empty. A patch that changes
    /// nothing under an unchanged version commits nothing.
    pub fn patch(
        &self,
        version: u32,
        patch: &Value,
        expected_revision: Option<u64>,
    ) -> Result<DocumentState, Error> {
        let Value::Object(patch) = patch else {
            return Err(Error::Invalid("a document patch must be a JSON object".into()));
        };
        let patch = patch.clone();
        let mut out = self.commit(version, expected_revision, move |cur| {
            let mut next = cur.cloned().unwrap_or_default();
            for (k, v) in patch {
                if v.is_null() {
                    next.remove(&k);
                } else {
                    next.insert(k, v);
                }
            }
            Ok(Some(next))
        })?;
        Ok(out.take().expect("patch returns the committed state"))
    }

    /// Store `build()` as the document, only when nothing is stored yet, atomically with the
    /// check. This is the one-time import of a legacy store (for example an older SQLite
    /// settings table): `build` runs inside the write lock and only for a missing document, so
    /// two processes cannot both import. `build` returning `Ok(None)` stores nothing. Returns
    /// whether a document was written. `version` is what the imported value is stored under.
    pub fn import_if_missing(
        &self,
        version: u32,
        build: impl FnOnce() -> Result<Option<Value>, Error>,
    ) -> Result<bool, Error> {
        let mut build = Some(build);
        let ns = self.namespace.clone();
        let mut wrote = false;
        self.backend.patch(&mut |cur| {
            if cur.is_some() {
                return Ok(Edit::Keep);
            }
            let Some(build) = build.take() else {
                return Err(Error::Invalid("document import ran twice".into()));
            };
            match build()? {
                None => Ok(Edit::Keep),
                Some(v) => {
                    wrote = true;
                    Ok(Edit::Commit(Stored {
                        version: Some(version),
                        namespace: Some(ns.clone()),
                        values: into_object(v)?,
                        revision: 0,
                        needs_rewrite: false,
                    }))
                }
            }
        })?;
        Ok(wrote)
    }

    /// Shared transaction. `edit` receives the stored object (if any) and returns the object to
    /// commit; `None` keeps storage. Returns the committed state (or the unchanged one when kept).
    fn commit(
        &self,
        version: u32,
        expected: Option<u64>,
        edit: impl FnOnce(Option<&Map<String, Value>>) -> Result<Option<Map<String, Value>>, Error>,
    ) -> Result<Option<DocumentState>, Error> {
        let mut edit = Some(edit);
        let ns = self.namespace.clone();
        let mut result: Option<DocumentState> = None;
        let committed = self.backend.patch(&mut |cur| {
            let revision = cur.map_or(0, |s| s.revision);
            if let Some(e) = expected {
                if e != revision {
                    return Err(Error::Conflict {
                        expected: e,
                        actual: revision,
                    });
                }
            }
            if let Some(s) = cur {
                if let Some(other) = s.namespace.as_deref().filter(|n| *n != ns) {
                    return Err(Error::Invalid(format!(
                        "document belongs to namespace {other}, not {ns}"
                    )));
                }
                if let Some(found) = s.version.filter(|v| *v > version) {
                    return Err(Error::NewerSchema {
                        found,
                        supported: version,
                    });
                }
            }
            let edit = edit
                .take()
                .ok_or(Error::Invalid("document transaction ran twice".into()))?;
            let Some(values) = edit(cur.map(|s| &s.values))? else {
                return Ok(Edit::Keep);
            };
            let unchanged = cur.is_some_and(|s| {
                s.values == values && s.version == Some(version) && s.namespace.is_some()
            });
            if unchanged {
                result = cur.map(|s| state_of(s.clone()));
                return Ok(Edit::Keep);
            }
            result = Some(DocumentState {
                version,
                value: Value::Object(values.clone()),
                revision,
            });
            Ok(Edit::Commit(Stored {
                version: Some(version),
                namespace: Some(ns.clone()),
                values,
                revision,
                needs_rewrite: false,
            }))
        })?;
        Ok(result.map(|mut s| {
            if let Some(r) = committed {
                s.revision = r;
            }
            s
        }))
    }
}

fn state_of(s: Stored) -> DocumentState {
    DocumentState {
        version: s.version.unwrap_or(0),
        value: Value::Object(s.values),
        revision: s.revision,
    }
}

fn into_object(v: Value) -> Result<Map<String, Value>, Error> {
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(Error::Invalid(
            "a settings document must be a JSON object".into(),
        )),
    }
}

fn validate_namespace(ns: &str) -> Result<(), Error> {
    let ok = !ns.is_empty()
        && ns
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(Error::BadSchema("namespace must be [A-Za-z0-9._-]+"))
    }
}
