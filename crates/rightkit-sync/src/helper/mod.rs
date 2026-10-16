//! Backend for a helper process speaking the `rightkit-sync-helper/1`
//! line-JSON protocol. The macOS CloudKit helper (`rightkit-cloudkit-helper`,
//! Swift) is the production implementation; `rightkit-sync-fake-helper` is the
//! protocol fake used by the end-to-end tests. See `API.md` section 4.

mod process;
pub use crate::wire;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

pub use process::{HelperConfig, HelperProcess};
use wire::{b64_decode, b64_encode, WireRecord};

use crate::error::{SyncError, SyncResult};
use crate::record::RawRecord;
use crate::traits::{AuthProvider, Backend, ChangePage, RecordOutcome, SavePolicy, SyncObserver};

/// Seconds to wait before retrying when a page had per-record fetch
/// failures and the helper gave no guidance (iOS busy hint).
const FAILED_FETCH_RETRY_SECS: f64 = 30.0;

struct Shared {
    process: HelperProcess,
    scope: Mutex<Option<String>>,
    generation: AtomicU64,
    enabled: AtomicBool,
    observer: Arc<dyn SyncObserver>,
}

/// One helper process shared by the backend and the auth provider of a
/// single sync engine. Clone freely.
#[derive(Clone)]
pub struct CloudKitHelper {
    shared: Arc<Shared>,
}

impl CloudKitHelper {
    pub fn new(cfg: HelperConfig, observer: Arc<dyn SyncObserver>) -> Self {
        Self {
            shared: Arc::new(Shared {
                process: HelperProcess::new(cfg),
                scope: Mutex::new(None),
                generation: AtomicU64::new(0),
                enabled: AtomicBool::new(true),
                observer,
            }),
        }
    }

    pub fn backend(&self) -> HelperBackend {
        HelperBackend {
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn auth(&self) -> HelperAuth {
        HelperAuth {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Number of helper processes started so far.
    pub fn spawn_count(&self) -> u64 {
        self.shared.process.spawn_count()
    }

    /// Raw protocol call for diagnostics and tests (for example the memory
    /// backend's `debug_write`). Not part of the sync contract.
    #[doc(hidden)]
    pub fn debug_call(&self, cmd: &str, params: Value) -> SyncResult<Map<String, Value>> {
        let params = self.shared.scoped(params);
        self.shared.call(cmd, params)
    }

    /// Memory backend only: another device writes `record` (wire JSON).
    #[doc(hidden)]
    pub fn backend_debug_write(&self, record: Value) -> SyncResult<()> {
        self.debug_call("debug_write", json!({ "record": record }))
            .map(|_| ())
    }

    /// Stop the helper process; the next call starts a fresh one.
    pub fn shutdown(&self) {
        self.shared.process.shutdown();
    }
}

impl Shared {
    fn call(&self, cmd: &str, params: Map<String, Value>) -> SyncResult<Map<String, Value>> {
        let resp = self.process.call(cmd, params)?;
        if resp.ok {
            Ok(resp.rest)
        } else {
            let err = resp.error.unwrap_or_else(|| {
                wire::WireError::new("transient", "helper reported failure without detail", None)
            });
            Err(err.into_sync_error())
        }
    }

    fn scoped(&self, params: Value) -> Map<String, Value> {
        let mut m = match params {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        let scope = self.scope.lock().unwrap().clone();
        m.insert(
            "expected_scope".into(),
            scope.map(Value::String).unwrap_or(Value::Null),
        );
        m
    }

    fn note_scope(&self, scope: Option<String>) {
        let mut cur = self.scope.lock().unwrap();
        if *cur != scope {
            if cur.is_some() {
                // Account/container/zone changed: fence in-flight passes.
                self.generation.fetch_add(1, Ordering::SeqCst);
                self.observer
                    .event("account_scope_changed", "helper scope changed");
            }
            *cur = scope;
        }
    }
}

/// Account state and identity from the helper's `status` command.
pub struct HelperAuth {
    shared: Arc<Shared>,
}

impl HelperAuth {
    /// The user's sync toggle. Disabling cancels an in-flight pass.
    pub fn set_enabled(&self, enabled: bool) {
        self.shared.enabled.store(enabled, Ordering::SeqCst);
        if !enabled {
            self.shared.generation.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl AuthProvider for HelperAuth {
    fn enabled(&self) -> bool {
        self.shared.enabled.load(Ordering::SeqCst)
    }

    fn availability(&self) -> SyncResult<()> {
        let body = self.shared.call("status", Map::new())?;
        let scope = body
            .get("scope")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.shared.note_scope(scope);
        match body.get("status").and_then(Value::as_str) {
            Some("ready") => Ok(()),
            _ => Err(SyncError::Unavailable(
                body.get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("iCloud is unavailable")
                    .to_string(),
            )),
        }
    }

    fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::SeqCst)
    }

    fn account_scope(&self) -> Option<String> {
        self.shared.scope.lock().unwrap().clone()
    }
}

/// Zone I/O through the helper.
pub struct HelperBackend {
    shared: Arc<Shared>,
}

fn strings(body: &Map<String, Value>, key: &str) -> Vec<String> {
    body.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn malformed(what: &str) -> SyncError {
    SyncError::Transient(format!("malformed helper response: {what}"))
}

impl HelperBackend {
    fn call_scoped(&self, cmd: &str, params: Value) -> SyncResult<Map<String, Value>> {
        let params = self.shared.scoped(params);
        let body = self.shared.call(cmd, params)?;
        if let Some(scope) = body.get("scope").and_then(Value::as_str) {
            // A response scope that differs from the one we sent means the
            // account moved under an in-flight call.
            let expected = self.shared.scope.lock().unwrap().clone();
            if expected.as_deref() != Some(scope) {
                self.shared.note_scope(Some(scope.to_string()));
                return Err(SyncError::Cancelled);
            }
        }
        Ok(body)
    }
}

impl Backend for HelperBackend {
    fn prepare(&self) -> SyncResult<()> {
        let body = self.call_scoped("prepare", json!({}))?;
        if let Some(e) = body.get("subscription_error").and_then(Value::as_str) {
            // Subscription failure is non-fatal: zone sync still works, only
            // silent pushes are lost.
            self.shared.observer.event("subscription_failed", e);
        }
        Ok(())
    }

    fn fetch_changes(&self, since: Option<&[u8]>) -> SyncResult<ChangePage> {
        let since = since.map(b64_encode);
        let body = self.call_scoped("fetch_changes", json!({ "since": since }))?;
        // H13: a per-record fetch failure inside an otherwise good page must
        // fail the page, or the cursor would advance past a record the feed
        // never lists again.
        let failed = strings(&body, "failed_records");
        if !failed.is_empty() {
            let secs = body
                .get("failed_retry_after_secs")
                .and_then(Value::as_f64)
                .filter(|s| s.is_finite() && *s > 0.0)
                .unwrap_or(FAILED_FETCH_RETRY_SECS);
            return Err(SyncError::RetryAfter {
                secs,
                detail: format!("{} record(s) failed to fetch", failed.len()),
            });
        }
        let undecodable = strings(&body, "undecodable_records");
        if !undecodable.is_empty() {
            self.shared
                .observer
                .event("records_undecodable", &undecodable.join(","));
        }
        let mut records = Vec::new();
        for v in body
            .get("records")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("records"))?
        {
            let wire: WireRecord =
                serde_json::from_value(v.clone()).map_err(|e| malformed(&e.to_string()))?;
            records.push(wire.into_raw().map_err(|e| malformed(&e))?);
        }
        let new_token = match body.get("token").and_then(Value::as_str) {
            Some(t) => Some(b64_decode(t).map_err(|e| malformed(&e))?),
            None => None,
        };
        Ok(ChangePage {
            records,
            deleted_record_names: strings(&body, "deleted"),
            new_token,
            more_coming: body
                .get("more_coming")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    fn fetch_record(&self, record_name: &str) -> SyncResult<Option<RawRecord>> {
        let body = self.call_scoped("fetch_record", json!({ "record_name": record_name }))?;
        match body.get("record") {
            None | Some(Value::Null) => Ok(None),
            Some(v) => {
                let wire: WireRecord =
                    serde_json::from_value(v.clone()).map_err(|e| malformed(&e.to_string()))?;
                Ok(Some(wire.into_raw().map_err(|e| malformed(&e))?))
            }
        }
    }

    fn save_records(
        &self,
        records: &[RawRecord],
        policy: SavePolicy,
    ) -> SyncResult<Vec<RecordOutcome>> {
        let policy = match policy {
            SavePolicy::Overwrite => "overwrite",
            SavePolicy::IfServerUnchanged => "if_server_unchanged",
        };
        let wire: Vec<WireRecord> = records.iter().map(WireRecord::from).collect();
        let body =
            self.call_scoped("save_records", json!({ "policy": policy, "records": wire }))?;
        let outcomes = body
            .get("outcomes")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed("outcomes"))?;
        let mut out = Vec::with_capacity(outcomes.len());
        for o in outcomes {
            let name = o
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("outcome name"))?
                .to_string();
            let result = if o.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                Ok(())
            } else {
                let err: wire::WireError = o
                    .get("error")
                    .cloned()
                    .and_then(|e| serde_json::from_value(e).ok())
                    .unwrap_or_else(|| wire::WireError::new("transient", "save failed", None));
                Err(err.into_sync_error())
            };
            let server_record = match o.get("server_record") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let wire: WireRecord =
                        serde_json::from_value(v.clone()).map_err(|e| malformed(&e.to_string()))?;
                    Some(wire.into_raw().map_err(|e| malformed(&e))?)
                }
            };
            out.push(RecordOutcome {
                record_name: name,
                result,
                server_record,
            });
        }
        Ok(out)
    }
}
