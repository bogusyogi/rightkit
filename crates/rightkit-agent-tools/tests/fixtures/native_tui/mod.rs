//! Focused fixture helpers for the Windows native-TUI ConPTY acceptance
//! cases in `managed_pty_closure.rs`.
//!
//! These modules are deliberately evidence-only: they read the isolated
//! durable store, the fixture provider's request log, and encode raw ConPTY
//! input payloads. They carry no presentation-label assertions — every
//! acceptance claim is checked against observable screen reconstruction or
//! durable/provider-side records by the owning case.

pub mod durable;
pub mod input;
pub mod requests;
