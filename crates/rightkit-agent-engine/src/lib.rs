//! Always-on agent contracts: typed task, authority, device and approval records.
//!
//! This crate holds serde types and pure functions. It has no runtime, no
//! journal, no IPC and no effects. Each type that is recorded or sent carries
//! a `schema_version`. Enums carry their vocabulary version as a module
//! constant, and the record that embeds them carries the record's version.
//!
//! The records follow the agent task contract: task envelope, authority, device and relay provenance.
//!
//! Modules
//! * `task`: task envelope, the ten statuses, legal transitions.
//! * `device`: portable device reference.
//! * `principal`: who submitted or authorized an act.
//! * `intent`: `intent.source` provenance (direct, delegated, unavailable
//!   pointer, revoked, out of scope, grant).
//! * `approval`: approval binding for consequential acts.
//! * `effect`: effect classification.
//! * `voice`: voice addressed CONTROL hand-off, mapped onto a task envelope.

pub mod approval;
pub mod device;
pub mod effect;
pub mod intent;
pub mod principal;
pub mod task;
pub mod task_cli;
pub mod voice;

pub use approval::{ApprovalBinding, ExecutorRef};
pub use device::{DevicePlatform, DeviceRef};
pub use effect::EffectClass;
pub use intent::{DelegationRef, GrantOrigin, Intent, IntentSource};
pub use principal::{Principal, PrincipalKind};
pub use task::{
    AuthorityKind, AuthorityRef, IllegalTransition, TaskEnvelope, TaskStatus, UtteranceRef,
    is_legal_transition, transition,
};
pub use voice::{VoiceError, VoiceHandoff};
