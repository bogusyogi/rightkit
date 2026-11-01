//! The allowlist of endpoint ids shared by the relay admission service and the mailbox.
//!
//! One file, one endpoint id (64 hex) per line, `#` comments and blank lines allowed. It is re-read
//! when its modification time or length changes, so pairing a device is one appended line and no
//! restart. A file with a malformed line is refused whole and the last good list stays; an
//! unreadable file denies everyone. Events are counts only: no endpoint id is ever logged.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use super::server::event;

const MAX_ALLOWED: usize = 4096;

pub fn is_endpoint_id(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parses the allowlist. Any non-comment line that is not 64 hex characters refuses the whole file.
pub fn parse_allowlist(text: &str) -> Result<HashSet<String>, String> {
    let mut ids = HashSet::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if !is_endpoint_id(line) {
            return Err(format!("line {}: not a 64-hex endpoint id", number + 1));
        }
        ids.insert(line.to_ascii_lowercase());
        if ids.len() > MAX_ALLOWED {
            return Err("too many entries".to_string());
        }
    }
    Ok(ids)
}

/// The allowlist, reloaded when the file's stamp (modification time and length) changes.
pub struct Allowlist {
    path: PathBuf,
    state: Mutex<ListState>,
}

struct ListState {
    stamp: Option<(SystemTime, u64)>,
    /// `None` after the file became unreadable: deny everyone until it is back.
    ids: Option<HashSet<String>>,
}

impl Allowlist {
    /// Loads the file once; a missing or malformed file at start is an error (fail closed).
    pub fn open(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let ids = parse_allowlist(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            state: Mutex::new(ListState {
                stamp: stamp_of(path),
                ids: Some(ids),
            }),
        })
    }

    pub fn contains(&self, id: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let now = stamp_of(&self.path);
        if now != state.stamp || state.ids.is_none() {
            match std::fs::read_to_string(&self.path) {
                Ok(text) => match parse_allowlist(&text) {
                    Ok(ids) => {
                        event("admission.reload", ids.len() as u64);
                        state.ids = Some(ids);
                        state.stamp = now;
                    }
                    Err(_) => {
                        // Keep the last good list; remember the stamp so the refusal is logged once.
                        event("admission.reload.refused", 1);
                        state.stamp = now;
                    }
                },
                Err(_) => {
                    if state.ids.take().is_some() {
                        event("admission.list.unreadable", 1);
                    }
                    state.stamp = None;
                }
            }
        }
        state
            .ids
            .as_ref()
            .is_some_and(|ids| ids.contains(&id.to_ascii_lowercase()))
    }
}

fn stamp_of(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Parses a grant file (`--grant-file`, written by the Pulse engine on the computer that decides
/// relay admission, docs/architecture/guest-relay-admission.md): one endpoint id per line, either
/// alone (no expiry) or followed by one space and its expiry in epoch milliseconds. `#` comments
/// and blank lines allowed. Any other line refuses the whole file.
pub fn parse_grants(text: &str) -> Result<std::collections::HashMap<String, Option<u64>>, String> {
    let mut grants = std::collections::HashMap::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(id), until, None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!(
                "line {}: not an endpoint id and expiry",
                number + 1
            ));
        };
        if !is_endpoint_id(id) {
            return Err(format!("line {}: not a 64-hex endpoint id", number + 1));
        }
        let until =
            match until {
                None => None,
                Some(ms) => Some(ms.parse::<u64>().map_err(|_| {
                    format!("line {}: expiry is not epoch milliseconds", number + 1)
                })?),
            };
        grants.insert(id.to_ascii_lowercase(), until);
        if grants.len() > MAX_ALLOWED {
            return Err("too many entries".to_string());
        }
    }
    Ok(grants)
}

/// The grant file, reloaded when its stamp changes. Unlike [`Allowlist`] it may be missing at
/// start (the engine has not written it yet): a missing or unreadable file grants nothing, a
/// malformed one keeps the last good grants, and every entry stops admitting at its own expiry
/// whether or not the file is rewritten.
pub struct GrantList {
    path: PathBuf,
    state: Mutex<GrantState>,
}

struct GrantState {
    stamp: Option<(SystemTime, u64)>,
    loaded: bool,
    grants: std::collections::HashMap<String, Option<u64>>,
}

impl GrantList {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            state: Mutex::new(GrantState {
                stamp: None,
                loaded: false,
                grants: std::collections::HashMap::new(),
            }),
        }
    }

    /// Whether `id` holds a grant that has not expired at `now_ms`.
    pub fn admits(&self, id: &str, now_ms: u64) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let now = stamp_of(&self.path);
        if now != state.stamp || !state.loaded {
            match std::fs::read_to_string(&self.path) {
                Ok(text) => match parse_grants(&text) {
                    Ok(grants) => {
                        event("admission.grants.reload", grants.len() as u64);
                        state.grants = grants;
                    }
                    Err(_) => event("admission.grants.refused", 1),
                },
                Err(_) => {
                    if !state.grants.is_empty() || !state.loaded {
                        event("admission.grants.unreadable", 1);
                    }
                    state.grants.clear();
                }
            }
            state.stamp = now;
            state.loaded = true;
        }
        state
            .grants
            .get(&id.to_ascii_lowercase())
            .is_some_and(|until| until.is_none_or(|ms| now_ms < ms))
    }
}
