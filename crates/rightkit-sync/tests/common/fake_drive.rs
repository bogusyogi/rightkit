//! In-process fake of the slice of the Google Drive v3 REST API that `DriveBackend` uses
//! (`appDataFolder` space), on rightkit-http's loopback mock server. Modelled on the Drive
//! reference, not on observed Google behaviour: ids are opaque strings, `version` rises with every
//! content write, `changes.list` pages by an integer token, `files.list` honours the `q` forms the
//! backend sends. There is deliberately NO precondition on writes (Drive documents none); tests
//! use the hooks to land a competing write inside the check-then-write window.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use rightkit_http::testing::{MockResponse, MockServer, RecordedRequest};
use serde_json::{json, Value};

pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

#[derive(Clone)]
pub struct DFile {
    pub id: String,
    pub name: String,
    pub parent: String,
    pub mime: String,
    pub version: u64,
    pub content: Vec<u8>,
}

impl DFile {
    fn meta(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "mimeType": self.mime,
            "version": self.version.to_string(),
            "parents": [self.parent],
            "trashed": false,
        })
    }
}

type Hook = Box<dyn FnOnce(&mut DriveState) + Send>;

#[derive(Default)]
pub struct DriveState {
    tokens: BTreeSet<String>,
    pub files: BTreeMap<String, DFile>,
    next: u64,
    /// `(file id, removed)` in commit order; the change token is an index into it.
    changes: Vec<(String, bool)>,
    fail_next: VecDeque<(u16, Vec<(String, String)>, Value)>,
    before_patch: Option<Hook>,
    before_create: Option<Hook>,
    before_meta: Option<Hook>,
    /// Record-file creates and content patches the server accepted.
    pub writes: usize,
}

impl DriveState {
    /// Insert a file as if another client had created it. `id` overrides the generated one.
    pub fn insert(
        &mut self,
        parent: &str,
        name: &str,
        mime: &str,
        content: Vec<u8>,
        id: Option<&str>,
    ) -> String {
        let id = match id {
            Some(i) => i.to_string(),
            None => {
                self.next += 1;
                format!("id{:04}", self.next)
            }
        };
        self.files.insert(
            id.clone(),
            DFile {
                id: id.clone(),
                name: name.to_string(),
                parent: parent.to_string(),
                mime: mime.to_string(),
                version: 1,
                content,
            },
        );
        self.changes.push((id.clone(), false));
        id
    }

    /// Replace a file's content as if another client had written it.
    pub fn patch(&mut self, id: &str, content: Vec<u8>) {
        if let Some(f) = self.files.get_mut(id) {
            f.content = content;
            f.version += 1;
            self.changes.push((id.to_string(), false));
        }
    }

    pub fn remove(&mut self, id: &str) {
        if self.files.remove(id).is_some() {
            self.changes.push((id.to_string(), true));
        }
    }

    pub fn folder_id(&self, name: &str) -> Option<String> {
        self.files
            .values()
            .find(|f| f.mime == FOLDER_MIME && f.name == name)
            .map(|f| f.id.clone())
    }

    /// Ids of live files with this name, in id order.
    pub fn ids_named(&self, name: &str) -> Vec<String> {
        self.files
            .values()
            .filter(|f| f.name == name && f.mime != FOLDER_MIME)
            .map(|f| f.id.clone())
            .collect()
    }
}

pub struct FakeDrive {
    pub server: MockServer,
    pub state: Arc<Mutex<DriveState>>,
}

fn err(status: u16, reason: &str, message: &str) -> MockResponse {
    MockResponse::json(
        status,
        &json!({ "error": {
            "code": status,
            "message": message,
            "errors": [{ "domain": "global", "reason": reason, "message": message }],
        } }),
    )
}

impl FakeDrive {
    /// `tokens`: the access tokens the server accepts.
    pub fn start(tokens: &[&str]) -> Self {
        let state = Arc::new(Mutex::new(DriveState::default()));
        for t in tokens {
            state.lock().unwrap().tokens.insert((*t).to_string());
        }
        let st = state.clone();
        let server = MockServer::start(move |req, _| handle(&st, req));
        Self { server, state }
    }

    pub fn base_url(&self) -> String {
        self.server.base_url()
    }

    /// The next request answers with this error (Google error body with `reason`).
    pub fn fail_next(&self, status: u16, headers: &[(&str, &str)], reason: &str) {
        self.state.lock().unwrap().fail_next.push_back((
            status,
            headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            json!({ "error": {
                "code": status,
                "message": "injected",
                "errors": [{ "domain": "global", "reason": reason, "message": "injected" }],
            } }),
        ));
    }

    pub fn accept(&self, token: &str) {
        self.state.lock().unwrap().tokens.insert(token.to_string());
    }

    pub fn revoke(&self, token: &str) {
        self.state.lock().unwrap().tokens.remove(token);
    }

    /// Run `f` just before the next content PATCH is applied (inside the backend's
    /// check-then-write window: after its version check, before its write).
    pub fn hook_before_patch(&self, f: impl FnOnce(&mut DriveState) + Send + 'static) {
        self.state.lock().unwrap().before_patch = Some(Box::new(f));
    }

    /// Run `f` just before the next multipart create is applied (after the backend's
    /// "does not exist" listing).
    pub fn hook_before_create(&self, f: impl FnOnce(&mut DriveState) + Send + 'static) {
        self.state.lock().unwrap().before_create = Some(Box::new(f));
    }

    /// Run `f` just before the next file-metadata read (the backend's version check), so a
    /// competing write lands BEFORE the check and is detected as a conflict.
    pub fn hook_before_metadata(&self, f: impl FnOnce(&mut DriveState) + Send + 'static) {
        self.state.lock().unwrap().before_meta = Some(Box::new(f));
    }

    /// Run `f` against the state (another client acting on the account).
    pub fn with<R>(&self, f: impl FnOnce(&mut DriveState) -> R) -> R {
        let mut guard = self.state.lock().unwrap();
        f(&mut *guard)
    }

    pub fn writes(&self) -> usize {
        self.state.lock().unwrap().writes
    }

    /// Number of live non-folder files.
    pub fn record_files(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .files
            .values()
            .filter(|f| f.mime != FOLDER_MIME)
            .count()
    }

    pub fn folders(&self, name: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .files
            .values()
            .filter(|f| f.mime == FOLDER_MIME && f.name == name)
            .count()
    }

    /// Ids of live files named `name` (for example `n1.json`).
    pub fn ids_named(&self, name: &str) -> Vec<String> {
        self.state.lock().unwrap().ids_named(name)
    }

    /// Parsed JSON content of the first file with this name.
    pub fn content_json(&self, name: &str) -> Option<Value> {
        let st = self.state.lock().unwrap();
        let id = st.ids_named(name).into_iter().next()?;
        serde_json::from_slice(&st.files[&id].content).ok()
    }

    pub fn version_of(&self, name: &str) -> Option<u64> {
        let st = self.state.lock().unwrap();
        let id = st.ids_named(name).into_iter().next()?;
        Some(st.files[&id].version)
    }

    /// Every user clears the app's hidden Drive data: all files and the folder are removed.
    pub fn delete_app_data(&self) {
        let mut st = self.state.lock().unwrap();
        let ids: Vec<String> = st.files.keys().cloned().collect();
        for id in ids {
            st.remove(&id);
        }
    }
}

/// Undo `\\` and `\'` in a Drive query string literal.
fn unquote(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Evaluate the `q` forms the backend sends: `name = '..'`, `mimeType = '..'`,
/// `'..' in parents`, `trashed = false`, joined by ` and `.
fn eval_q(q: &str, f: &DFile) -> bool {
    q.split(" and ").all(|clause| {
        let clause = clause.trim();
        if let Some(rest) = clause.strip_prefix("name = '") {
            rest.strip_suffix('\'').map(unquote).as_deref() == Some(f.name.as_str())
        } else if let Some(rest) = clause.strip_prefix("mimeType = '") {
            rest.strip_suffix('\'').map(unquote).as_deref() == Some(f.mime.as_str())
        } else if let Some(rest) = clause.strip_prefix('\'') {
            rest.strip_suffix("' in parents").map(unquote).as_deref() == Some(f.parent.as_str())
        } else {
            clause == "trashed = false"
        }
    })
}

/// Split a `multipart/related` body into its JSON metadata part and its media part.
fn parse_multipart(content_type: &str, body: &[u8]) -> Option<(Value, Vec<u8>)> {
    let boundary = content_type.split("boundary=").nth(1)?.trim();
    let text = std::str::from_utf8(body).ok()?;
    let delim = format!("--{boundary}");
    let parts: Vec<&str> = text.split(delim.as_str()).collect();
    if parts.len() < 4 {
        return None;
    }
    let payload = |part: &str| -> Option<String> {
        let part = part.strip_prefix("\r\n")?;
        let (_, rest) = part.split_once("\r\n\r\n")?;
        Some(rest.strip_suffix("\r\n")?.to_string())
    };
    let meta: Value = serde_json::from_str(&payload(parts[1])?).ok()?;
    let media = payload(parts[2])?.into_bytes();
    Some((meta, media))
}

fn handle(state: &Mutex<DriveState>, req: &RecordedRequest) -> MockResponse {
    let mut st = state.lock().unwrap();
    if let Some((status, headers, body)) = st.fail_next.pop_front() {
        let mut r = MockResponse::json(status, &body);
        for (k, v) in headers {
            r = r.header(&k, &v);
        }
        return r;
    }
    let authorised = req
        .header("authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .is_some_and(|t| st.tokens.contains(t));
    if !authorised {
        return err(401, "authError", "Invalid Credentials");
    }
    let q = req.query_pairs();
    let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let decoded: Vec<String> = req
        .path()
        .trim_start_matches('/')
        .split('/')
        .map(rightkit_http::percent_decode)
        .collect();
    let segs: Vec<&str> = decoded.iter().map(String::as_str).collect();
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["drive", "v3", "files"]) => {
            if get("spaces").as_deref() != Some("appDataFolder") {
                return err(400, "invalidArgument", "spaces=appDataFolder expected");
            }
            let query = get("q").unwrap_or_default();
            let size = get("pageSize")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(100)
                .clamp(1, 1000);
            let offset = match get("pageToken") {
                None => 0,
                Some(t) => match t.parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => return err(400, "invalid", "Invalid Value"),
                },
            };
            let all: Vec<&DFile> = st.files.values().filter(|f| eval_q(&query, f)).collect();
            let page: Vec<Value> = all.iter().skip(offset).take(size).map(|f| f.meta()).collect();
            let mut body = json!({ "kind": "drive#fileList", "files": page });
            if offset + size < all.len() {
                body["nextPageToken"] = json!((offset + size).to_string());
            }
            MockResponse::json(200, &body)
        }
        ("GET", ["drive", "v3", "files", id]) => {
            if get("alt").as_deref() != Some("media") {
                let hook = st.before_meta.take();
                if let Some(h) = hook {
                    h(&mut *st);
                }
            }
            let Some(f) = st.files.get(*id) else {
                return err(404, "notFound", "File not found");
            };
            if get("alt").as_deref() == Some("media") {
                MockResponse::new(200)
                    .header("Content-Type", "application/json")
                    .body(f.content.clone())
            } else {
                MockResponse::json(200, &f.meta())
            }
        }
        ("DELETE", ["drive", "v3", "files", id]) => {
            if !st.files.contains_key(*id) {
                return err(404, "notFound", "File not found");
            }
            st.remove(id);
            MockResponse::new(204)
        }
        ("POST", ["drive", "v3", "files"]) => {
            let body = req.body_json();
            let name = body["name"].as_str().unwrap_or("").to_string();
            let mime = body["mimeType"].as_str().unwrap_or("").to_string();
            let parent = body["parents"][0].as_str().unwrap_or("appDataFolder").to_string();
            if parent != "appDataFolder" && !st.files.contains_key(&parent) {
                return err(404, "notFound", "Parent not found");
            }
            let id = st.insert(&parent, &name, &mime, Vec::new(), None);
            MockResponse::json(200, &json!({ "id": id }))
        }
        ("POST", ["upload", "drive", "v3", "files"]) => {
            let hook = st.before_create.take();
            if let Some(h) = hook {
                h(&mut *st);
            }
            let Some((meta, media)) = parse_multipart(
                req.header("content-type").unwrap_or(""),
                &req.body,
            ) else {
                return err(400, "badRequest", "bad multipart body");
            };
            let name = meta["name"].as_str().unwrap_or("").to_string();
            let mime = meta["mimeType"].as_str().unwrap_or("").to_string();
            let parent = meta["parents"][0].as_str().unwrap_or("appDataFolder").to_string();
            if parent != "appDataFolder" && !st.files.contains_key(&parent) {
                return err(404, "notFound", "Parent not found");
            }
            let id = st.insert(&parent, &name, &mime, media, None);
            st.writes += 1;
            let version = st.files[&id].version;
            MockResponse::json(200, &json!({ "id": id, "version": version.to_string() }))
        }
        ("PATCH", ["upload", "drive", "v3", "files", id]) => {
            let hook = st.before_patch.take();
            if let Some(h) = hook {
                h(&mut *st);
            }
            if !st.files.contains_key(*id) {
                return err(404, "notFound", "File not found");
            }
            let Some((meta, media)) = parse_multipart(
                req.header("content-type").unwrap_or(""),
                &req.body,
            ) else {
                return err(400, "badRequest", "bad multipart body");
            };
            st.patch(id, media);
            st.writes += 1;
            if let Some(name) = meta["name"].as_str() {
                if let Some(f) = st.files.get_mut(*id) {
                    f.name = name.to_string();
                }
            }
            let version = st.files[*id].version;
            MockResponse::json(200, &json!({ "id": id, "version": version.to_string() }))
        }
        ("GET", ["drive", "v3", "changes", "startPageToken"]) => MockResponse::json(
            200,
            &json!({ "kind": "drive#startPageToken", "startPageToken": st.changes.len().to_string() }),
        ),
        ("GET", ["drive", "v3", "changes"]) => {
            if get("spaces").as_deref() != Some("appDataFolder") {
                return err(400, "invalidArgument", "spaces=appDataFolder expected");
            }
            let size = get("pageSize")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(100)
                .clamp(1, 1000);
            let from = match get("pageToken").and_then(|t| t.parse::<usize>().ok()) {
                Some(n) if n <= st.changes.len() => n,
                _ => return err(400, "invalid", "Invalid Value"),
            };
            let end = (from + size).min(st.changes.len());
            let page: Vec<Value> = st.changes[from..end]
                .iter()
                .map(|(id, removed)| {
                    if *removed {
                        json!({ "kind": "drive#change", "fileId": id, "removed": true })
                    } else {
                        let file = st.files.get(id).map(DFile::meta);
                        json!({ "kind": "drive#change", "fileId": id, "removed": file.is_none(), "file": file })
                    }
                })
                .collect();
            let mut body = json!({ "kind": "drive#changeList", "changes": page });
            if end < st.changes.len() {
                body["nextPageToken"] = json!(end.to_string());
            } else {
                body["newStartPageToken"] = json!(end.to_string());
            }
            MockResponse::json(200, &body)
        }
        _ => err(404, "notFound", "no such route"),
    }
}
