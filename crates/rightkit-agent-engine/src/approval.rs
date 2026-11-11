//! Approval binding: bind approval to principal, task/step, effect, exact
//! consequential arguments, destination, executor, scope, expiry and revision; recheck after waits and
//! before execution. Also "no double asking":
//! the binding covers a retry so the owner is asked once for the same act.

use serde::{Deserialize, Serialize};

use super::device::DeviceRef;
use super::effect::EffectClass;
use super::principal::Principal;

/// Version of the [`ApprovalBinding`] record.
pub const APPROVAL_BINDING_SCHEMA_VERSION: u32 = 1;
/// Version of the [`ExecutorRef`] record.
pub const EXECUTOR_REF_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalBinding {
    pub schema_version: u32,
    /// Who approves. The agent asks the owner for irreversible classes.
    pub principal: Principal,
    pub task_id: String,
    /// Step within the task, when the task has steps.
    pub step_id: Option<String>,
    pub effect: EffectClass,
    /// Digest of the canonical serialization of the exact consequential
    /// arguments, written as `sha256:<64 lowercase hex>`. Any change to those
    /// arguments invalidates the approval.
    pub args_hash: String,
    /// Destination the effect reaches (repository remote, recipient, service).
    pub destination: String,
    pub executor: ExecutorRef,
    /// Scope tokens the approval grants. Nothing outside them is covered.
    pub scope: Vec<String>,
    /// Epoch milliseconds after which the approval is void.
    pub expires_at_ms: u64,
    /// Monotonic revision. A revised or withdrawn approval has a higher revision
    /// and supersedes earlier ones.
    pub revision: u64,
}

/// Executor that will perform the act: a device plus an executor name on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutorRef {
    pub schema_version: u32,
    pub device: DeviceRef,
    /// Executor name on that device, for example a chat session or a right-ctl command set.
    pub executor: String,
}

impl ApprovalBinding {
    /// This binding as the approvals core's [`rightkit_approvals::Binding`], the one record every
    /// approval store and rule uses: the arguments digest (without its `sha256:` prefix), the
    /// destination, the expiry and the revision. Principal, task, effect, executor and scope stay
    /// on this record; the core binds what the act does, this record also says who may do it.
    pub fn core_binding(&self) -> rightkit_approvals::Binding {
        rightkit_approvals::Binding {
            args: self
                .args_hash
                .strip_prefix("sha256:")
                .unwrap_or(&self.args_hash)
                .to_string(),
            destination: self.destination.clone(),
            expires_ms: self.expires_at_ms,
            revision: self.revision,
        }
    }
}
