//! `rightkit-workflow`: the shared scenario interpreter and state machine (RK8).
//!
//! A scenario is an ordered list of [`StepSpec`]s. A [`Run`] executes each step
//! under a [`Lease`] and records every transition in an append-only, fsynced
//! [`journal`]. The machine upholds these rules:
//!
//! - `perform` runs at most once per `(run, step, take)`. The `Dispatched`
//!   record is durable before `perform` is called, so a restart after dispatch
//!   never calls `perform` again.
//! - `observe` is polled until the postcondition holds, the deadline passes, or
//!   the step is cancelled. A delayed paint therefore never repeats an action.
//! - A step whose outcome was never observed is never declared `Completed`. It
//!   ends `Unresolved` or resumes into observe.
//! - Every worker write carries its fencing token and is rejected once the
//!   lease is superseded or expired.
//! - [`Run::retake`] replays a step's precondition into a new take and compares
//!   postconditions. A differing postcondition is flagged, never spliced.
//!
//! Remote workers run over [`controller::ControllerLink`] (the controller side,
//! which implements [`transport::WorkerTransport`]) and [`worker::WorkerEndpoint`]
//! (the GUI worker). Enrolment uses a one-time code. Both sides then hold a
//! device key. Each connection authenticates with that key and a fresh nonce
//! pair, and every frame carries a MAC and a sequence number. The machine
//! itself does no I/O beyond the journal file.

pub mod clock;
pub mod controller;
pub mod driver;
pub mod error;
pub mod journal;
pub mod machine;
pub mod net;
pub mod protocol;
#[cfg(feature = "sequence")]
pub mod sequence;
pub mod transport;
pub mod wire;
pub mod worker;

pub use clock::{Clock, ManualClock, SystemClock};
pub use controller::{Controller, ControllerLink, IssuedEnrolment, RemoteDriver};
pub use driver::{
    CancelToken, DriverError, IdempotencyKey, Observation, ObservePolicy, Scenario, StepDriver,
    StepSpec,
};
pub use error::{Error, Result};
pub use journal::{Event, Journal, Record};
pub use machine::{ExecContext, Lease, Phase, RetakeReport, Run, StepRef};
#[cfg(feature = "sequence")]
pub use sequence::{
    Gate, GateDecision, NoGate, ReplayPolicy, Sequence, SequenceConfig, SequenceOutcome,
    SequenceRun, StopReason,
};
pub use transport::{
    BundleRef, Capability, EnrolmentRequest, ReconnectState, TransferReport, TransportError,
    WorkerIdentity, WorkerTransport,
};
pub use worker::WorkerEndpoint;
