use std::fmt;

use crate::machine::{Phase, StepRef};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The journal could not be parsed or breaks its sequence. A torn final
    /// line is not corruption; it is truncated on open.
    Corrupt {
        line: usize,
        reason: String,
    },
    /// The journal belongs to a different run.
    WrongRun {
        expected: String,
        found: String,
    },
    /// A journal write failed part-way. The handle refuses further appends so a
    /// torn record can never be followed by a valid one.
    Poisoned,
    /// The presented lease is not the current grant for the step.
    Fenced {
        step: StepRef,
        presented: u64,
        current: u64,
        reason: String,
    },
    /// The lease expired before the write. The step keeps its durable phase, so
    /// the next acquirer resumes into observe.
    LeaseExpired {
        step: StepRef,
        fence: u64,
    },
    /// Another unexpired lease holds the step.
    Held {
        step: StepRef,
        worker: String,
    },
    /// The step is in a terminal phase and cannot be leased again.
    Terminal {
        step: StepRef,
        phase: Phase,
    },
    /// A retake needs a completed original take to compare against.
    NoOriginal {
        step: StepRef,
    },
    /// Scenario index is out of range.
    Index {
        index: usize,
        len: usize,
    },
    /// Scenario or record content is invalid.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "journal I/O: {e}"),
            Error::Corrupt { line, reason } => write!(f, "journal line {line} corrupt: {reason}"),
            Error::WrongRun { expected, found } => {
                write!(f, "journal belongs to run {found}, not {expected}")
            }
            Error::Poisoned => write!(f, "journal handle poisoned by an earlier failed write"),
            Error::Fenced {
                step,
                presented,
                current,
                reason,
            } => write!(
                f,
                "write to {step} fenced (presented fence {presented}, current {current}): {reason}"
            ),
            Error::LeaseExpired { step, fence } => {
                write!(f, "lease fence {fence} on {step} expired")
            }
            Error::Held { step, worker } => write!(f, "{step} is leased by {worker}"),
            Error::Terminal { step, phase } => write!(f, "{step} is terminal ({phase:?})"),
            Error::NoOriginal { step } => write!(f, "no completed original take at {step}"),
            Error::Index { index, len } => write!(f, "step index {index} out of {len}"),
            Error::Invalid(reason) => write!(f, "invalid: {reason}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
