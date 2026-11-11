//! The agent task contract: typed task, authority, device and approval records.
//!
//! This crate holds serde types and pure functions. It has no runtime, no
//! journal, no IPC and no effects. Each type that is recorded or sent carries
//! a `schema_version`. Enums carry their vocabulary version as a module
//! constant, and the record that embeds them carries the record's version.
//!
//! It answers "who asked, with what authority, on which device, and what happens when the
//! outcome is unknown". The turn loop that runs a task is `rightkit-agent-engine`; its
//! protocol uses these records for authority.
//!
//! Modules
//! * `task`: task envelope, the ten statuses, legal transitions.
//! * `device`: portable device reference.
//! * `principal`: who submitted or authorized an act.
//! * `intent`: `intent.source` provenance (direct, delegated, unavailable
//!   pointer, revoked, out of scope, grant).
//! * `approval`: approval binding for consequential acts, on the approvals core's
//!   [`rightkit_approvals::Binding`].
//! * `effect`: effect classification.
//! * `voice`: voice addressed CONTROL hand-off, mapped onto a task envelope with
//!   the chat label and policy reference the caller supplies.
//! * `task_cli` (feature `cli`): the task command line over a host trait.

pub mod approval;
pub mod device;
pub mod effect;
pub mod intent;
pub mod principal;
pub mod task;
#[cfg(feature = "cli")]
pub mod task_cli;
pub mod voice;

pub use approval::{ApprovalBinding, ExecutorRef};
pub use device::{DevicePlatform, DeviceRef};
pub use effect::EffectClass;
pub use intent::{DelegationRef, GrantOrigin, Intent, IntentSource};
pub use principal::{Principal, PrincipalKind};
pub use rightkit_approvals::Binding;
pub use task::{
    AuthorityKind, AuthorityRef, IllegalTransition, TaskEnvelope, TaskStatus, UtteranceRef,
    is_legal_transition, transition,
};
pub use voice::{VoiceError, VoiceHandoff, VoiceLabels};
