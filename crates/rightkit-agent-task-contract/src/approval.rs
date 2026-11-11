//! Approval binding: bind approval to principal, task/step, effect, exact
//! consequential arguments, destination, executor, scope, expiry and revision; recheck after waits and
//! before execution. Also "no double asking":
//! the binding covers a retry so the owner is asked once for the same act.
//!
//! What the act does (arguments digest, destination, expiry, revision) is the approvals core's
//! [`Binding`], the one record every approval store and rule uses. This record adds who may do
//! it: principal, task, effect class, executor and scope.

use rightkit_approvals::Binding;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::device::DeviceRef;
use super::effect::EffectClass;
use super::principal::Principal;

/// Version of the [`ApprovalBinding`] record.
pub const APPROVAL_BINDING_SCHEMA_VERSION: u32 = 1;
/// Version of the [`ExecutorRef`] record.
pub const EXECUTOR_REF_SCHEMA_VERSION: u32 = 1;
/// Prefix of the arguments digest on the wire (`args_hash`).
pub const ARGS_HASH_PREFIX: &str = "sha256:";

/// The binding of one approval to one act.
///
/// On the wire the core binding's four fields are flat, as they always were: `args_hash`
/// (`sha256:<64 lowercase hex>`), `destination`, `expires_at_ms` and `revision`. In memory they
/// are [`ApprovalBinding::binding`], with the digest's hex in [`Binding::args`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "WireApprovalBinding", try_from = "WireApprovalBinding")]
pub struct ApprovalBinding {
    pub schema_version: u32,
    /// Who approves. The agent asks the owner for irreversible classes.
    pub principal: Principal,
    pub task_id: String,
    /// Step within the task, when the task has steps.
    pub step_id: Option<String>,
    pub effect: EffectClass,
    /// What is approved: the hex SHA-256 of the canonical serialization of the exact
    /// consequential arguments (any change to them invalidates the approval), the destination
    /// the effect reaches (repository remote, recipient, service), the epoch milliseconds after
    /// which the approval is void, and the monotonic revision (a revised or withdrawn approval
    /// has a higher revision and supersedes earlier ones). Checked at use by [`Binding::check`].
    pub binding: Binding,
    pub executor: ExecutorRef,
    /// Scope tokens the approval grants. Nothing outside them is covered.
    pub scope: Vec<String>,
}

/// Executor that will perform the act: a device plus an executor name on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutorRef {
    pub schema_version: u32,
    pub device: DeviceRef,
    /// Executor name on that device, for example a chat session or a command set.
    pub executor: String,
}

/// Why an approval binding record was refused when read.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ApprovalBindingError {
    /// `args_hash` is not `sha256:` followed by 64 lowercase hex characters.
    #[error("args_hash must be sha256: followed by 64 lowercase hex characters")]
    ArgsHash,
}

impl ApprovalBinding {
    /// The approvals core's binding this record carries.
    pub fn core_binding(&self) -> &Binding {
        &self.binding
    }

    /// The arguments digest as written on the wire: `sha256:<hex>`.
    pub fn args_hash(&self) -> String {
        format!("{ARGS_HASH_PREFIX}{}", self.binding.args)
    }
}

/// The record as it is written: the core binding's fields flat, under their contract names.
#[derive(Serialize, Deserialize)]
struct WireApprovalBinding {
    schema_version: u32,
    principal: Principal,
    task_id: String,
    step_id: Option<String>,
    effect: EffectClass,
    args_hash: String,
    destination: String,
    executor: ExecutorRef,
    scope: Vec<String>,
    expires_at_ms: u64,
    revision: u64,
}

impl From<ApprovalBinding> for WireApprovalBinding {
    fn from(record: ApprovalBinding) -> Self {
        let args_hash = record.args_hash();
        WireApprovalBinding {
            schema_version: record.schema_version,
            principal: record.principal,
            task_id: record.task_id,
            step_id: record.step_id,
            effect: record.effect,
            args_hash,
            destination: record.binding.destination,
            executor: record.executor,
            scope: record.scope,
            expires_at_ms: record.binding.expires_ms,
            revision: record.binding.revision,
        }
    }
}

impl TryFrom<WireApprovalBinding> for ApprovalBinding {
    type Error = ApprovalBindingError;

    fn try_from(wire: WireApprovalBinding) -> Result<Self, Self::Error> {
        let hex = wire
            .args_hash
            .strip_prefix(ARGS_HASH_PREFIX)
            .filter(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or(ApprovalBindingError::ArgsHash)?;
        Ok(ApprovalBinding {
            schema_version: wire.schema_version,
            principal: wire.principal,
            task_id: wire.task_id,
            step_id: wire.step_id,
            effect: wire.effect,
            binding: Binding {
                args: hex.to_string(),
                destination: wire.destination,
                expires_ms: wire.expires_at_ms,
                revision: wire.revision,
            },
            executor: wire.executor,
            scope: wire.scope,
        })
    }
}
