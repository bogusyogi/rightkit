//! Protocol fake of `rightkit-cloudkit-helper`: speaks `rightkit-sync-helper/1`
//! over stdin/stdout with the same semantics the CloudKit helper has (change
//! feed with paging, expiry, zone loss, per-record fetch failures, non-atomic
//! saves, account scope), backed by a JSON zone file so several helper
//! processes can play several devices on one iCloud zone.
//!
//! Faults are injected by writing `<zone-file>.faults.json` (see `Faults`);
//! one-shot faults are consumed when they fire. Test fixture only.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use rightkit_sync::helper::wire::{
    b64_decode, b64_encode, err_line, ok_line, Request, WireError, WireRecord,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Zone {
    seq: u64,
    epoch: u64,
    exists: bool,
    records: BTreeMap<String, (u64, WireRecord)>,
    deletions: Vec<(u64, String)>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Faults {
    /// Account identity reported as `scope` (default "fake-account-A").
    account: Option<String>,
    /// `status` reports unavailable with this detail.
    unavailable: Option<String>,
    /// Every zone call fails with a network retry-after 15 error.
    offline: bool,
    /// Max records per fetch page (0 = unlimited).
    page_size: usize,
    expire_token_once: bool,
    zone_missing_once: bool,
    /// Accept this many saves in total, then fail the rest of the call.
    fail_after_saves: Option<usize>,
    /// Record names reported as transient per-record fetch failures once.
    failed_fetch_once: Vec<String>,
    failed_fetch_retry_after: Option<f64>,
    /// Record names reported as permanently undecodable on every fetch.
    undecodable: Vec<String>,
    /// Every zone call fails as throttled with this retry-after.
    throttle_secs: Option<f64>,
    /// Sleep this long (ms) before answering the next request (of command
    /// `hang_cmd` when set), once.
    hang_once_ms: Option<u64>,
    hang_cmd: Option<String>,
    /// Exit without answering the next request, once.
    crash_once: bool,
    subscription_error: Option<String>,
    /// Another device's write that lands just before the next `save_records`
    /// is evaluated (the window between the engine's fetch and its save).
    inject_before_save: Option<WireRecord>,
    conflicts: u64,
    /// Counters written back for the test to read.
    fetch_calls: u64,
    save_calls: u64,
    saved_records: u64,
}

struct Files {
    /// Zone files are per account: `<base>.<account>.json`, so an account
    /// switch sees a different (initially empty) zone, like iCloud.
    base: PathBuf,
    faults: PathBuf,
}

impl Files {
    fn zone(&self, account: &str) -> PathBuf {
        PathBuf::from(format!("{}.{account}.json", self.base.display()))
    }
}

fn load<T: Default + for<'de> Deserialize<'de>>(p: &Path) -> T {
    std::fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn store<T: Serialize>(p: &Path, v: &T) {
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(v).expect("serialize")).expect("write state");
    std::fs::rename(&tmp, p).expect("rename state");
}

fn put(zone: &mut Zone, mut r: WireRecord) {
    zone.seq += 1;
    r.modified_ms = Some(zone.seq as i64);
    r.change_tag = Some(zone.seq.to_string());
    zone.records.insert(r.name.clone(), (zone.seq, r));
}

fn handle(files: &Files, req: &Request) -> Result<Map<String, Value>, WireError> {
    let mut faults: Faults = load(&files.faults);
    let account = faults.account.clone().unwrap_or_else(|| "fake-account-A".into());
    let zone_path = files.zone(&account);
    let mut zone: Zone = load(&zone_path);
    let params = Value::Object(req.params.clone());
    let scope = faults.account.clone().unwrap_or_else(|| "fake-account-A".into());
    let mut body = Map::new();
    body.insert("scope".into(), scope.clone().into());

    if req.cmd == "status" {
        match &faults.unavailable {
            Some(detail) => {
                body.insert("status".into(), "unavailable".into());
                body.insert("detail".into(), detail.clone().into());
            }
            None => {
                body.insert("status".into(), "ready".into());
            }
        }
        return Ok(body);
    }
    if req.cmd == "shutdown" {
        return Ok(body);
    }

    // Account isolation: a request bound to another account is refused
    // before anything is read or written.
    match params.get("expected_scope").and_then(Value::as_str) {
        None => return Err(WireError::new("transient", "missing expected scope", None)),
        Some(s) if s != scope => {
            return Err(WireError::new(
                "scope_changed",
                "iCloud account changed during the sync request",
                None,
            ))
        }
        _ => {}
    }
    if faults.offline {
        return Err(WireError::new("network", "no network", Some(15.0)));
    }
    if let Some(secs) = faults.throttle_secs {
        return Err(WireError::new("busy", "request rate limited", Some(secs)));
    }

    let result = match req.cmd.as_str() {
        "prepare" => {
            zone.exists = true;
            if let Some(e) = &faults.subscription_error {
                body.insert("subscription_error".into(), e.clone().into());
            }
            Ok(body)
        }
        "fetch_changes" => {
            faults.fetch_calls += 1;
            if faults.zone_missing_once {
                faults.zone_missing_once = false;
                zone = Zone::default();
                store(&zone_path, &zone);
                store(&files.faults, &faults);
                return Err(WireError::new("zone_missing", "zone not found", None));
            }
            let since = params.get("since").and_then(Value::as_str);
            let mut after = 0u64;
            if let Some(s) = since {
                if faults.expire_token_once {
                    faults.expire_token_once = false;
                    store(&files.faults, &faults);
                    return Err(WireError::new("token_expired", "change token expired", None));
                }
                let bytes = b64_decode(s).unwrap_or_default();
                match <[u8; 8]>::try_from(bytes.as_slice()) {
                    Ok(b) => after = u64::from_le_bytes(b),
                    Err(_) => return Err(WireError::new("token_expired", "bad token", None)),
                }
            }
            if !zone.exists {
                return Err(WireError::new("zone_missing", "zone not found", None));
            }
            let mut changed: Vec<(u64, WireRecord)> = zone
                .records
                .values()
                .filter(|(s, _)| *s > after)
                .cloned()
                .collect();
            changed.sort_by_key(|(s, _)| *s);
            let limit = if faults.page_size == 0 { usize::MAX } else { faults.page_size };
            let more = changed.len() > limit;
            changed.truncate(limit);
            let cursor = if more {
                changed.last().map(|(s, _)| *s).unwrap_or(after)
            } else {
                zone.seq
            };
            let deleted: Vec<String> = zone
                .deletions
                .iter()
                .filter(|(s, _)| *s > after && *s <= cursor)
                .map(|(_, n)| n.clone())
                .collect();
            let mut records = Vec::new();
            let mut failed = Vec::new();
            let mut undecodable = Vec::new();
            for (_, r) in changed {
                if let Some(i) = faults.failed_fetch_once.iter().position(|n| *n == r.name) {
                    faults.failed_fetch_once.remove(i);
                    failed.push(r.name);
                } else if faults.undecodable.contains(&r.name) {
                    undecodable.push(r.name);
                } else {
                    records.push(r);
                }
            }
            body.insert("records".into(), serde_json::to_value(records).unwrap());
            body.insert("deleted".into(), json!(deleted));
            body.insert("token".into(), b64_encode(&cursor.to_le_bytes()).into());
            body.insert("more_coming".into(), more.into());
            body.insert("failed_records".into(), json!(failed));
            if !failed.is_empty() {
                if let Some(s) = faults.failed_fetch_retry_after {
                    body.insert("failed_retry_after_secs".into(), s.into());
                }
            }
            body.insert("undecodable_records".into(), json!(undecodable));
            Ok(body)
        }
        "fetch_record" => {
            let name = params.get("record_name").and_then(Value::as_str).unwrap_or("");
            let rec = zone.records.get(name).map(|(_, r)| r.clone());
            body.insert("record".into(), serde_json::to_value(rec).unwrap());
            Ok(body)
        }
        "save_records" => {
            faults.save_calls += 1;
            if !zone.exists {
                return Err(WireError::new("zone_missing", "zone not found", None));
            }
            let records: Vec<WireRecord> = serde_json::from_value(
                params.get("records").cloned().unwrap_or(Value::Null),
            )
            .map_err(|e| WireError::new("transient", e.to_string(), None))?;
            if let Some(r) = faults.inject_before_save.take() {
                put(&mut zone, r);
            }
            let conditional = params.get("policy").and_then(Value::as_str) == Some("if_server_unchanged");
            let mut outcomes = Vec::new();
            let mut budget = faults.fail_after_saves;
            for r in records {
                let name = r.name.clone();
                if conditional {
                    let server = zone.records.get(&name).map(|(_, s)| s.clone());
                    let stale = match &server {
                        Some(s) => s.change_tag != r.change_tag,
                        None => r.change_tag.is_some(),
                    };
                    if stale {
                        faults.conflicts += 1;
                        let (kind, rec) = match server {
                            Some(s) => ("conflict", Some(s)),
                            None => ("transient", None),
                        };
                        outcomes.push(json!({
                            "name": name, "ok": false,
                            "error": WireError::new(kind, "server record changed", None),
                            "server_record": rec,
                        }));
                        continue;
                    }
                }
                if budget == Some(0) {
                    outcomes.push(json!({
                        "name": name, "ok": false,
                        "error": WireError::new("transient", "injected save failure", None)
                    }));
                    continue;
                }
                if let Some(b) = budget.as_mut() {
                    *b -= 1;
                }
                put(&mut zone, r);
                faults.saved_records += 1;
                outcomes.push(json!({ "name": name, "ok": true }));
            }
            faults.fail_after_saves = budget;
            body.insert("outcomes".into(), json!(outcomes));
            Ok(body)
        }
        other => Err(WireError::new("transient", format!("unknown command {other}"), None)),
    };
    store(&zone_path, &zone);
    store(&files.faults, &faults);
    result
}

fn main() {
    let mut zone_file: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--zone-file" {
            zone_file = args.next().map(PathBuf::from);
        }
    }
    let Some(zone_path) = zone_file else {
        eprintln!("usage: rightkit-sync-fake-helper --zone-file PATH");
        std::process::exit(2);
    };
    let files = Files {
        faults: PathBuf::from(format!("{}.faults.json", zone_path.display())),
        base: zone_path,
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(out, "{}", err_line(0, &WireError::new("transient", e.to_string(), None)));
                let _ = out.flush();
                continue;
            }
        };
        let mut faults: Faults = load(&files.faults);
        if faults.crash_once {
            faults.crash_once = false;
            store(&files.faults, &faults);
            eprintln!("fake helper: injected crash");
            std::process::exit(1);
        }
        let hang_matches = faults.hang_cmd.as_deref().is_none_or(|c| c == req.cmd);
        if hang_matches {
            if let Some(ms) = faults.hang_once_ms.take() {
                faults.hang_cmd = None;
                store(&files.faults, &faults);
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
        }
        let reply = match handle(&files, &req) {
            Ok(body) => ok_line(req.id, body),
            Err(e) => err_line(req.id, &e),
        };
        if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
            break;
        }
        if req.cmd == "shutdown" {
            break;
        }
    }
}
