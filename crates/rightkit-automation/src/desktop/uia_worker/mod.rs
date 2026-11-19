//! Windows UI Automation delivery worker (feature `desktop-context`, Windows
//! only). A supervised MTA worker owns native probes, retained editor pins
//! and focus restoration. Each operation has a bounded request deadline.
//!
//! Provides focused-element identity, bounded text readback (ValuePattern,
//! TextPattern, VS Code terminal rows), retained stop-time editor pins with
//! validate / gone / restore / web-container checks. The host keeps its own
//! target snapshot type (`StopTarget<T>`), its deadlines and size bounds
//! (`Config`) and the four pure decisions that stay with it (`Rules`).

mod api;
mod effects;
mod pins;
mod protocol;
mod supervisor;
mod worker;

pub use api::{Availability, Health, ShutdownReport, Uia};
pub use protocol::{
    Config, FocusedElementIdentity, NativeTarget, RestoreOutcome, Rules, StopTarget, TextProof,
};
