//! A store in files: one owner-only JSON file per request, and per move out of a state one guard
//! file made with `create_new`, so of two racing moves out of the same state exactly one wins.
//!
//! Layout under the store's root: `<kind>/<destination>.<id>.json` holds the request;
//! `<...>.json.from-pending`, `<...>.json.from-approved` and `<...>.json.from-claimed` are the
//! guards: each is created (`create_new`) by the one move that leaves that state and holds that
//! move as JSON, before anything else is written. Then the descriptive marker follows:
//! `<...>.json.approved` an approval, `<...>.json.claimed` a claim, `<...>.json.used` the use,
//! `<...>.json.closed` a refusal, revocation, failure or expiry, each holding the move as JSON.
//! So an approval and a refusal of the same request, or a claim and a revocation of the same
//! approval, never both succeed: the second finds the guard taken. A claim is then settled by
//! exactly one of `.used` or `.closed` (the `from-claimed` guard). A reader takes the state from
//! the markers and then follows any guard whose marker is not written yet (a move in flight, or
//! one a crash cut short after its guard), so a move is never half-applied. The request file
//! also carries `nonce` (the
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
const CLAIMED: &str = "claimed";
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

/// The guard file suffix for the moves out of `from`; `None` for a final state (nothing leaves it).
fn guard_of(from: State) -> Option<&'static str> {
    match from {
        State::Pending => Some("from-pending"),
        State::Approved => Some("from-approved"),
        State::Claimed => Some("from-claimed"),
        State::Denied | State::Done | State::Failed | State::Expired => None,
    }
}

/// The move a marker or guard file holds, as JSON.
fn change_body(change: &Change) -> Value {
    json!({
        "state": change.to,
        "at_ms": change.at_ms,
        "grant": change.grant,
        "reason": change.reason,
        "result": change.result,
    })
}

/// The descriptive marker a move to `to` writes; `None` for a move to pending (never made).
fn marker_of(to: State) -> Option<&'static str> {
    match to {
        State::Approved => Some(APPROVED),
        State::Claimed => Some(CLAIMED),
        State::Done => Some(USED),
        State::Pending => None,
        State::Denied | State::Failed | State::Expired => Some(CLOSED),
    }
}

/// Creates `path` owner-only with `body`, failing with `Ok(false)` when it exists already.
fn create_exclusive(path: &Path, body: &Value) -> Result<bool> {
    let mut file = match rightkit_fs::create_private_new(path) {
        Ok(file) => file,
        // An existing file is `AlreadyExists` on Unix but an `Io` error carrying that kind on
        // Windows, so test the kind, not the variant.
        Err(e) if e.kind() == Some(std::io::ErrorKind::AlreadyExists) => return Ok(false),
        Err(e) => return Err(io(path, e)),
    };
    use std::io::Write;
    file.write_all(body.to_string().as_bytes())
        .map_err(|e| io(path, e))?;
    Ok(true)
}

/// The move held in a marker or guard file; `None` when the file is absent or not complete JSON
/// yet. `fallback` is the state a marker names when its body carries none.
fn read_change(path: &Path, fallback: Option<State>, grant: Option<Grant>) -> Option<Change> {
    let text = std::fs::read_to_string(path).ok()?;
    let change = serde_json::from_str::<Value>(&text).ok()?;
    let state = serde_json::from_value::<State>(change["state"].clone())
        .ok()
        .or(fallback)?;
    let own = serde_json::from_value::<Grant>(change["grant"].clone()).ok();
    Some(Change {
        to: state,
        at_ms: change["at_ms"].as_u64().unwrap_or(0),
        grant: own.or(grant),
        reason: change["reason"].as_str().map(str::to_string),
        result: change["result"].as_str().map(str::to_string),
    })
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
        for marker in [APPROVED, CLAIMED, CLOSED, USED] {
            let fallback = match marker {
                APPROVED => State::Approved,
                CLAIMED => State::Claimed,
                USED => State::Done,
                _ => State::Denied,
            };
            if let Some(change) =
                read_change(&with_suffix(path, marker), Some(fallback), record.grant)
            {
                change.apply(&mut record);
            }
        }
        // A move whose guard is taken but whose marker is not written yet (in flight, or cut short
        // by a crash) has still happened: follow the guards from the state the markers give. A
        // guard that is not complete JSON yet is a move still being written; it is not applied.
        while let Some(guard) = guard_of(record.state) {
            let Some(change) = read_change(&with_suffix(path, guard), None, record.grant) else {
                break;
            };
            if change.to == record.state || change.to == State::Pending {
                break;
            }
            change.apply(&mut record);
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

    fn claimed(&self) -> Result<Vec<Record>> {
        let mut out: Vec<Record> = self
            .all()
            .into_iter()
            .filter(|r| r.state == State::Claimed)
            .collect();
        out.sort_by_key(|r| r.decided_ms);
        Ok(out)
    }

    fn transition(&self, id: &str, from: State, change: &Change) -> Result<bool> {
        let Some(path) = self.find(id)? else {
            return Ok(false);
        };
        if self.read(&path)?.state != from {
            return Ok(false);
        }
        let (Some(marker), Some(guard)) = (marker_of(change.to), guard_of(from)) else {
            return Ok(false);
        };
        if change.to == from {
            return Ok(false);
        }
        // Records written before the guards: a use and a close exclude each other, and a claim
        // excludes both unless they settle it.
        let rivals: &[&str] = match marker {
            CLAIMED => &[USED, CLOSED],
            USED if from == State::Claimed => &[CLOSED],
            CLOSED if from == State::Claimed => &[USED],
            USED => &[CLOSED, CLAIMED],
            CLOSED => &[USED, CLAIMED],
            _ => &[],
        };
        if rivals.iter().any(|r| with_suffix(&path, r).exists()) {
            return Ok(false);
        }
        let body = change_body(change);
        // The one decision: exactly one move out of `from` creates its guard. Every rival move
        // out of the same state (approve or deny, claim or revoke, use or fail) finds it taken.
        if !create_exclusive(&with_suffix(&path, guard), &body)? {
            return Ok(false);
        }
        // The descriptive marker, for readers that look for `.used` and the like. Only the guard's
        // owner gets here, so the marker is new unless a pre-guard writer made it; the move is
        // already decided and the guard alone keeps it readable either way.
        create_exclusive(&with_suffix(&path, marker), &body)?;
        Ok(true)
    }
}
