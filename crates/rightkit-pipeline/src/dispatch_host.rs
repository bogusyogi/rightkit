//! The host seams of [`crate::dispatch`]: the channel store, the contract and text-gate calls it
//! makes, and the QA isolation answers. A host registers one [`DispatchHost`] at start-up with
//! [`set_host`].
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rightkit_db::rusqlite::Connection;
use rightkit_social_publish::protocol::Scope;
use rightkit_social_publish::Fail;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The deliverable key of an envelope accepted without one.
pub const DEFAULT_DELIVERABLE: &str = "";

/// One delivery envelope stored for an outgoing brief.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryRecord {
    pub scope: Scope,
    pub envelope_id: String,
    pub request_id: String,
    pub brief_id: String,
    pub brief_revision: i64,
    pub deliverable: String,
    pub document: Value,
    pub accepted_at: String,
}

/// An outgoing brief as the host's contract store returns it (`brief_out.get`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefOutRecord {
    pub scope: Scope,
    pub request_id: String,
    pub brief_id: String,
    pub revision: i64,
    pub state: String,
    pub document: Value,
    #[serde(default)]
    pub deliveries: Vec<DeliveryRecord>,
    pub created_at: String,
    pub updated_at: String,
}

/// One envelope of an engine result, with the deliverable key it belongs to.
#[derive(Debug, Clone)]
pub struct BatchEnvelope {
    pub deliverable: String,
    pub document: Value,
}

/// What the dispatch module needs from the host app.
pub trait DispatchHost: Send + Sync {
    /// Opens the channel store (schema migrated) under the data root.
    fn open_store(&self, root: &Path) -> Result<Connection, Fail>;
    /// Calls a contract-store method (`brief_out.get`, `brief_out.list`, `contract.check`).
    fn contracts(&self, root: &Path, method: &str, input: Value) -> Result<Value, Fail>;
    /// Calls the text gate (`gate.check`).
    fn gate(&self, root: &Path, method: &str, input: Value) -> Result<Value, Fail>;
    /// Accepts every envelope of one engine result for `request_id` in one transaction: all are
    /// stored or none. `is_final` is true for a `done` result and false for `partial`.
    fn accept_batch(
        &self,
        root: &Path,
        scope: &Scope,
        request_id: &str,
        envelopes: &[BatchEnvelope],
        is_final: bool,
        now: &str,
    ) -> Result<Vec<DeliveryRecord>, Fail>;
    /// Whether the process runs under QA isolation; an error means the mode cannot be decided.
    fn qa_isolation(&self) -> Result<bool, String> {
        Ok(false)
    }
    /// The validated QA suite root a QA-isolated child is bound to.
    fn qa_suite_root(&self) -> Result<PathBuf, String> {
        Err("no QA suite root".to_string())
    }
    /// Whether the process is QA-isolated (a failed decision counts as not isolated).
    fn qa_isolated(&self) -> bool {
        self.qa_isolation().unwrap_or(false)
    }
}

static HOST: OnceLock<Box<dyn DispatchHost>> = OnceLock::new();

/// Register the host once. Returns `false` when one is already registered.
pub fn set_host(host: Box<dyn DispatchHost>) -> bool {
    HOST.set(host).is_ok()
}

pub(crate) fn host() -> Result<&'static dyn DispatchHost, Fail> {
    HOST.get().map(|h| h.as_ref()).ok_or((
        "internal",
        "no dispatch host is registered (dispatch_host::set_host)".to_string(),
    ))
}
