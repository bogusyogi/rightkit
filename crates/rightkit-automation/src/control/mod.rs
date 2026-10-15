//! The control plane for effects that do not go through [`crate::Automation`]:
//! raw background input (`macos::background`), off-screen launch (`offscreen`) and
//! any caller-built controller. Moved here from `rightkit-qa-driver` so the
//! generic input code has one home; the QA crates re-export it.
//!
//! - [`admission`]: EFF-001 `EffectGate`. Validate, approve through a host
//!   [`admission::AdmissionHook`], execute, settle. A denial happens before any
//!   side effect. The process-wide kill switch (`policy::kill`) is enforced here,
//!   so nothing admitted through a gate can run while it is engaged.
//! - [`policy_hook`]: [`policy_hook::PolicyAdmission`], the allowlist as an
//!   admission hook (deny by default, deny list, host-process protection).
//! - [`lease`]: PTY-002 `InputLease` (epoch fencing, acknowledged input sequence,
//!   durable state) and `ControlSession`, which composes lease and gate.
//! - [`lease_store`]: the durable lease store trait and the atomic file store.
//! - [`events`]: content-free `ControlEvent`s for the caller's journal.
//!
//! The typed per-app layer ([`crate::admission`], [`crate::lease`], [`crate::events`])
//! belongs to `Automation` and is a different model (per-pid, in-memory, tied to
//! the engine's receipts); the two are deliberately not merged.

pub mod admission;
pub mod events;
pub mod lease;
pub mod lease_store;
pub mod policy_hook;
