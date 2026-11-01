//! The durable publish-job store as the host provides it. The host owns the schema, the job table, the
//! optimistic revision rule and the transactions; the modules here (`quota`, `resolve`, ...) read and
//! write jobs only through [`PublishJobStore`].

use serde_json::Value;

use crate::protocol::{PublishJob, Scope};
use crate::util::ChannelLookup;
use crate::Fail;

/// What the host app provides for the job-driven modules.
pub trait PublishJobStore: ChannelLookup {
    /// The job `job_id` of exactly `scope`, if stored (a job of another scope reads as absent).
    fn load_job(&self, scope: &Scope, job_id: &str) -> Result<Option<PublishJob>, Fail>;
    /// Writes `job` back on the caller's transaction, compare-and-swap on `job.revision - 1`, and
    /// advances `job.revision` and `job.updated_at` (`stamp`).
    fn save_job(&self, job: &mut PublishJob, stamp: &str) -> Result<(), Fail>;
    /// The stored `remote` JSON text of every job of `scope` and `platform` other than `except_job`.
    fn other_remote_json(
        &self,
        scope: &Scope,
        platform: &str,
        except_job: &str,
    ) -> Result<Vec<String>, Fail>;
    /// `role_forbidden` unless the host runs in the role that may publish.
    fn require_role(&self) -> Result<(), Fail>;
    /// Runs `body` in one write transaction: commit on `Ok`, roll back on `Err`.
    fn write_txn(&self, body: &mut dyn FnMut() -> Result<(), Fail>) -> Result<(), Fail>;
}

/// Reads `post_id` out of a stored `remote` JSON text.
pub(crate) fn post_id_of(remote_json: &str) -> Option<String> {
    serde_json::from_str::<Value>(remote_json)
        .ok()
        .and_then(|v| v.get("post_id").and_then(Value::as_str).map(str::to_string))
}
