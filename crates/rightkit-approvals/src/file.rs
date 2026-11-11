//! A store in files: one owner-only JSON file per request, and one marker file per move made
//! with `create_new`, so of two racing moves exactly one wins.
//!
//! Layout under the store's root: `<kind>/<destination>.<id>.json` holds the request;
//! `<...>.json.approved` an approval, `<...>.json.used` the use, `<...>.json.closed` a refusal,
//! failure or expiry, each holding the move as JSON. The request file also carries `nonce` (the
//! id) and `input_hash` (the binding's arguments) at its top level, so a reader that only checks
//! "this request was issued for these arguments and used" (`<file>` plus `<file>.used`) needs
//! nothing else.
//!
//! LIMIT: files prove nothing against a process that can write the root. A file store binds
//! approvals made through the app's own calls; it is not a boundary against local code running as
//! the same user.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Refusal, Result};
use crate::model::{Change, Grant, Record, State};
use crate::store::Store;

/// Records in files under one root folder.
#[derive(Clone, Debug)]
pub struct FileStore {
    root: PathBuf,
}

const APPROVED: &str = "approved";
const USED: &str = "used";
const CLOSED: &str = "closed";

fn io(path: &Path, e: impl std::fmt::Display) -> Refusal {
    Refusal::new("io", format!("{}: {e}", path.display()))
}

fn safe(label: &str, value: &str, allow_dots: bool) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 160
        && !value.contains("..")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || (allow_dots && c == '.'));
    if ok {
        Ok(())
    } else {
        Err(Refusal::new(
            "invalid_input",
            format!("invalid {label} \"{value}\""),
        ))
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

impl FileStore {
    /// A store under `root` (created owner-only on the first insert).
    pub fn new(root: impl Into<PathBuf>) -> FileStore {
        FileStore { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the request file of `kind`, `destination` and `id` lives. Every part must be
    /// letters, digits, `-` or `_` (the destination may also hold single dots).
    pub fn path_of(&self, kind: &str, destination: &str, id: &str) -> Result<PathBuf> {
        safe("approval kind", kind, false)?;
        safe("destination", destination, true)?;
        safe("approval id", id, false)?;
        Ok(self
            .root
            .join(kind)
            .join(format!("{destination}.{id}.json")))
    }

    /// The request file with this id, searched under every kind.
    fn find(&self, id: &str) -> Result<Option<PathBuf>> {
        safe("approval id", id, false)?;
        let tail = format!(".{id}.json");
        let Ok(kinds) = std::fs::read_dir(&self.root) else {
            return Ok(None);
        };
        for kind in kinds.flatten() {
            let Ok(files) = std::fs::read_dir(kind.path()) else {
                continue;
            };
            for file in files.flatten() {
                if file.file_name().to_string_lossy().ends_with(&tail) {
                    return Ok(Some(file.path()));
                }
            }
        }
        Ok(None)
    }

    fn read(&self, path: &Path) -> Result<Record> {
        let text = std::fs::read_to_string(path).map_err(|e| io(path, e))?;
        let value: Value = serde_json::from_str(&text).map_err(|e| io(path, e))?;
        let mut record: Record =
            serde_json::from_value(value["record"].clone()).map_err(|e| io(path, e))?;
        for marker in [APPROVED, CLOSED, USED] {
            let Ok(text) = std::fs::read_to_string(with_suffix(path, marker)) else {
                continue;
            };
            let Ok(change) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let state =
                serde_json::from_value::<State>(change["state"].clone()).unwrap_or(match marker {
                    APPROVED => State::Approved,
                    USED => State::Done,
                    _ => State::Denied,
                });
            let grant = serde_json::from_value::<Grant>(change["grant"].clone()).ok();
            Change {
                to: state,
                at_ms: change["at_ms"].as_u64().unwrap_or(0),
                grant: grant.or(record.grant),
                reason: change["reason"].as_str().map(str::to_string),
                result: change["result"].as_str().map(str::to_string),
            }
            .apply(&mut record);
        }
        Ok(record)
    }

    fn all(&self) -> Vec<Record> {
        let mut out = Vec::new();
        let Ok(kinds) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for kind in kinds.flatten() {
            let Ok(files) = std::fs::read_dir(kind.path()) else {
                continue;
            };
            for file in files.flatten() {
                if file.file_name().to_string_lossy().ends_with(".json") {
                    if let Ok(record) = self.read(&file.path()) {
                        out.push(record);
                    }
                }
            }
        }
        out.sort_by_key(|r| r.request.created_ms);
        out
    }
}

impl Store for FileStore {
    fn insert(&self, record: &Record) -> Result<()> {
        let request = &record.request;
        let path = self.path_of(&request.kind, &request.binding.destination, &request.id)?;
        if let Some(parent) = path.parent() {
            rightkit_fs::ensure_private_dir(parent).map_err(|e| io(parent, e))?;
        }
        let body = json!({
            "nonce": request.id,
            "input_hash": request.binding.args,
            "record": record,
        });
        if path.exists() {
            return Err(Refusal::new(
                "approval_invalid",
                "that approval id is taken",
            ));
        }
        rightkit_fs::atomic_write(
            &path,
            serde_json::to_string_pretty(&body)
                .unwrap_or_default()
                .as_bytes(),
        )
        .map_err(|e| io(&path, e))
    }

    fn get(&self, id: &str) -> Result<Option<Record>> {
        match self.find(id)? {
            Some(path) => self.read(&path).map(Some),
            None => Ok(None),
        }
    }

    fn pending(&self, now_ms: u64) -> Result<Vec<Record>> {
        Ok(self
            .all()
            .into_iter()
            .filter(|r| r.state == State::Pending && r.live(now_ms))
            .collect())
    }

    fn approved(&self, now_ms: u64) -> Result<Vec<Record>> {
        Ok(self
            .all()
            .into_iter()
            .filter(|r| r.state == State::Approved && r.live(now_ms))
            .collect())
    }

    fn transition(&self, id: &str, from: State, change: &Change) -> Result<bool> {
        let Some(path) = self.find(id)? else {
            return Ok(false);
        };
        if self.read(&path)?.state != from {
            return Ok(false);
        }
        let marker = match change.to {
            State::Approved => APPROVED,
            State::Done => USED,
            State::Pending => return Ok(false),
            _ => CLOSED,
        };
        // A use and a close exclude each other: whichever marker exists first wins.
        let rival = match marker {
            USED => Some(CLOSED),
            CLOSED => Some(USED),
            _ => None,
        };
        if rival.is_some_and(|r| with_suffix(&path, r).exists()) {
            return Ok(false);
        }
        let marker_path = with_suffix(&path, marker);
        let mut file = match rightkit_fs::create_private_new(&marker_path) {
            Ok(file) => file,
            // An existing marker is `AlreadyExists` on Unix but an `Io` error carrying that kind
            // on Windows, so test the kind, not the variant.
            Err(e) if e.kind() == Some(std::io::ErrorKind::AlreadyExists) => return Ok(false),
            Err(e) => return Err(io(&marker_path, e)),
        };
        let body = json!({
            "state": change.to,
            "at_ms": change.at_ms,
            "grant": change.grant,
            "reason": change.reason,
            "result": change.result,
        });
        use std::io::Write;
        file.write_all(body.to_string().as_bytes())
            .map_err(|e| io(&marker_path, e))?;
        Ok(true)
    }
}
