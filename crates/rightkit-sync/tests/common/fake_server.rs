//! In-process fake of the `rightkit-sync-http/1` server (the shapes of
//! `packages/sync-worker`), on rightkit-http's loopback mock server. Multi-tenant by bearer
//! token, with per-zone sequence numbers, conditional saves, a compaction floor and one-shot
//! injected failures.
#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use rightkit_http::testing::{MockResponse, MockServer, RecordedRequest};
use serde_json::{json, Value};

#[derive(Clone)]
struct Rec {
    name: String,
    record_type: String,
    seq: u64,
    version: u64,
    updated_at: i64,
    deleted_at: Option<i64>,
    enc: Option<String>,
}

impl Rec {
    fn wire(&self) -> Value {
        json!({
            "name": self.name,
            "type": self.record_type,
            "seq": self.seq,
            "change_tag": self.version.to_string(),
            "updated_at": self.updated_at,
            "deleted_at": self.deleted_at,
            "enc": self.enc,
        })
    }
}

#[derive(Default)]
struct Zone {
    seq: u64,
    floor: u64,
    records: BTreeMap<String, Rec>,
}

/// A scripted failure: status, headers, JSON body.
type FailNext = (u16, Vec<(String, String)>, Value);

#[derive(Default)]
pub struct FakeState {
    /// bearer token -> account id
    tokens: BTreeMap<String, String>,
    zones: BTreeMap<(String, String), Zone>,
    fail_next: VecDeque<FailNext>,
}

pub struct FakeServer {
    pub server: MockServer,
    pub state: Arc<Mutex<FakeState>>,
}

fn err(status: u16, code: &str, message: &str) -> MockResponse {
    MockResponse::json(
        status,
        &json!({ "error": { "code": code, "message": message } }),
    )
}

impl FakeServer {
    /// `tokens`: `(bearer token, account id)` pairs the server accepts.
    pub fn start(tokens: &[(&str, &str)]) -> Self {
        let state = Arc::new(Mutex::new(FakeState::default()));
        for (t, a) in tokens {
            state
                .lock()
                .unwrap()
                .tokens
                .insert((*t).to_string(), (*a).to_string());
        }
        let st = state.clone();
        let server = MockServer::start(move |req, _| handle(&st, req));
        Self { server, state }
    }

    pub fn base_url(&self) -> String {
        self.server.base_url()
    }

    /// The next request (any route except health) answers with this error instead.
    pub fn fail_next(&self, status: u16, headers: &[(&str, &str)], code: &str) {
        self.state.lock().unwrap().fail_next.push_back((
            status,
            headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            json!({ "error": { "code": code, "message": "injected" } }),
        ));
    }

    pub fn revoke(&self, token: &str) {
        self.state.lock().unwrap().tokens.remove(token);
    }

    /// Wire JSON of one stored record.
    pub fn record(&self, account: &str, zone: &str, name: &str) -> Option<Value> {
        let st = self.state.lock().unwrap();
        st.zones
            .get(&(account.to_string(), zone.to_string()))?
            .records
            .get(name)
            .map(Rec::wire)
    }

    /// Every stored record of a zone as wire JSON.
    pub fn records(&self, account: &str, zone: &str) -> Vec<Value> {
        let st = self.state.lock().unwrap();
        st.zones
            .get(&(account.to_string(), zone.to_string()))
            .map(|z| z.records.values().map(Rec::wire).collect())
            .unwrap_or_default()
    }

    /// Server-side compaction of a tombstone as the spec requires: the sealed `enc` stays
    /// (only an out-of-line body would be dropped, and this fake keeps `enc` inline), and the
    /// floor rises to its seq.
    pub fn compact_tombstone(&self, account: &str, zone: &str, name: &str) {
        let mut st = self.state.lock().unwrap();
        let z = st
            .zones
            .get_mut(&(account.to_string(), zone.to_string()))
            .expect("zone");
        let r = z.records.get_mut(name).expect("record");
        assert!(r.deleted_at.is_some(), "only tombstones are compacted");
        z.floor = z.floor.max(r.seq);
    }

    /// What a non-conforming or hostile server could serve: a tombstone with its sealed `enc`
    /// stripped. Clients must quarantine it.
    pub fn strip_tombstone_enc(&self, account: &str, zone: &str, name: &str) {
        let mut st = self.state.lock().unwrap();
        let z = st
            .zones
            .get_mut(&(account.to_string(), zone.to_string()))
            .expect("zone");
        let r = z.records.get_mut(name).expect("record");
        assert!(r.deleted_at.is_some(), "only tombstones are stripped");
        r.enc = None;
        z.floor = z.floor.max(r.seq);
    }

    /// Delete the zone (the next call answers 404 zone_missing until prepare).
    pub fn drop_zone(&self, account: &str, zone: &str) {
        self.state
            .lock()
            .unwrap()
            .zones
            .remove(&(account.to_string(), zone.to_string()));
    }
}

fn handle(state: &Mutex<FakeState>, req: &RecordedRequest) -> MockResponse {
    let path = req.path().to_string();
    if req.method == "GET" && path == "/v1/sync/health" {
        return MockResponse::json(
            200,
            &json!({ "ok": true, "protocol": "rightkit-sync-http/1" }),
        );
    }
    let mut st = state.lock().unwrap();
    if let Some((status, headers, body)) = st.fail_next.pop_front() {
        let mut r = MockResponse::json(status, &body);
        for (k, v) in headers {
            r = r.header(&k, &v);
        }
        return r;
    }
    let account = match req
        .header("authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .and_then(|t| st.tokens.get(t))
    {
        Some(a) => a.clone(),
        None => return err(401, "invalid_token", "bad token"),
    };
    let decoded: Vec<String> = path
        .trim_start_matches('/')
        .split('/')
        .map(rightkit_http::percent_decode)
        .collect();
    let segs: Vec<&str> = decoded.iter().map(String::as_str).collect();
    match (req.method.as_str(), segs.as_slice()) {
        ("DELETE", ["v1", "sync", "account"]) => {
            if req.header("x-rightkit-confirm") != Some("erase-account") {
                return err(400, "confirm_required", "missing confirmation");
            }
            let before = st.zones.len();
            st.zones.retain(|(a, _), _| *a != account);
            let erased = before - st.zones.len();
            MockResponse::json(200, &json!({ "erased": { "zones": erased } }))
        }
        ("POST", ["v1", "sync", "zones", zone]) => {
            let key = (account, (*zone).to_string());
            let created = !st.zones.contains_key(&key);
            st.zones.entry(key).or_default();
            MockResponse::json(200, &json!({ "zone": zone, "created": created }))
        }
        ("GET", ["v1", "sync", "zones", zone, "changes"]) => {
            let Some(z) = st.zones.get(&(account, (*zone).to_string())) else {
                return err(404, "zone_missing", "no such zone");
            };
            let q = req.query_pairs();
            let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            let since = match get("since").filter(|s| !s.is_empty()) {
                None => 0,
                Some(s) => match s.parse::<u64>() {
                    Ok(n) => n,
                    Err(_) => return err(410, "token_expired", "malformed cursor"),
                },
            };
            if since > z.seq || (since != 0 && since < z.floor) {
                return err(410, "token_expired", "cursor no longer valid");
            }
            let limit = get("limit")
                .and_then(|l| l.parse::<usize>().ok())
                .unwrap_or(200)
                .clamp(1, 200);
            let mut changed: Vec<&Rec> = z.records.values().filter(|r| r.seq > since).collect();
            changed.sort_by_key(|r| r.seq);
            let more = changed.len() > limit;
            changed.truncate(limit);
            let new_token = if more {
                changed.last().map(|r| r.seq).unwrap_or(since)
            } else {
                z.seq
            };
            MockResponse::json(
                200,
                &json!({
                    "records": changed.iter().map(|r| r.wire()).collect::<Vec<_>>(),
                    "deleted_record_names": [],
                    "new_token": new_token.to_string(),
                    "more_coming": more,
                }),
            )
        }
        ("GET", ["v1", "sync", "zones", zone, "records", name]) => {
            let Some(z) = st.zones.get(&(account, (*zone).to_string())) else {
                return err(404, "zone_missing", "no such zone");
            };
            MockResponse::json(
                200,
                &json!({ "record": z.records.get(*name).map(Rec::wire) }),
            )
        }
        ("POST", ["v1", "sync", "zones", zone, "save"]) => {
            let Some(z) = st.zones.get_mut(&(account, (*zone).to_string())) else {
                return err(404, "zone_missing", "no such zone");
            };
            let body = req.body_json();
            let policy = body["policy"].as_str().unwrap_or("").to_string();
            let Some(records) = body["records"].as_array() else {
                return err(400, "bad_batch", "records must be an array");
            };
            if records.len() > 100 {
                return err(400, "bad_batch", "too many records");
            }
            let mut outcomes = Vec::new();
            for r in records {
                let name = r["name"].as_str().unwrap_or("").to_string();
                let tag = r["change_tag"].as_str().map(str::to_string);
                let existing = z.records.get(&name).cloned();
                if policy == "if_server_unchanged" {
                    let stale = match (&existing, &tag) {
                        (None, None) => false,
                        (None, Some(_)) => true,
                        (Some(e), t) => t.as_deref() != Some(e.version.to_string().as_str()),
                    };
                    if stale {
                        outcomes.push(json!({
                            "name": name,
                            "status": "conflict",
                            "server_record": existing.as_ref().map(Rec::wire),
                        }));
                        continue;
                    }
                }
                let Some(updated_at) = r["updated_at"].as_i64() else {
                    outcomes.push(json!({
                        "name": name, "status": "error",
                        "code": "invalid_record", "message": "invalid updated_at",
                    }));
                    continue;
                };
                z.seq += 1;
                let rec = Rec {
                    name: name.clone(),
                    record_type: r["type"].as_str().unwrap_or("").to_string(),
                    seq: z.seq,
                    version: existing.map(|e| e.version + 1).unwrap_or(1),
                    updated_at,
                    deleted_at: r["deleted_at"].as_i64(),
                    enc: r["enc"].as_str().map(str::to_string),
                };
                outcomes.push(json!({
                    "name": name, "status": "ok",
                    "change_tag": rec.version.to_string(), "seq": rec.seq,
                }));
                z.records.insert(name, rec);
            }
            MockResponse::json(200, &json!({ "outcomes": outcomes }))
        }
        _ => err(404, "not_found", "no such route"),
    }
}
