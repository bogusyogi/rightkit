use std::{error::Error, fmt, io, time::Duration};

/// Operation that failed. `Spawn` means no child was created; `Setup` may follow creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RunnerPhase {
    Spawn,
    Setup,
    Wait,
    Terminate,
    Stdout,
    Stderr,
    Stdin,
    Drain,
    Cleanup,
}

/// Evidence about the leader only; this does not certify escaped descendants or pipe EOF.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaderTermination {
    NotStarted,
    Confirmed,
    Unconfirmed,
}

/// Structured runner failure retained inside `io::Error`, with its original source intact.
#[derive(Debug)]
pub struct RunnerFailure {
    phase: RunnerPhase,
    termination: LeaderTermination,
    source: io::Error,
}

impl RunnerFailure {
    /// Downcast runner provenance from the outer I/O error; terminal pass-through has none.
    pub fn from_io(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }

    /// Operation that failed; no message parsing is needed.
    pub fn phase(&self) -> RunnerPhase {
        self.phase
    }

    /// Evidence about the leader only, independent of escaped descendants or pipe EOF.
    pub fn termination(&self) -> LeaderTermination {
        self.termination
    }

    /// Original I/O error, including its OS code when one exists. The outer error is a wrapper.
    pub fn source_io_error(&self) -> &io::Error {
        &self.source
    }
}

impl fmt::Display for RunnerFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.phase {
            RunnerPhase::Stdout => write!(
                f,
                "stdout capture failed, output is incomplete: {}",
                self.source
            ),
            RunnerPhase::Stderr => write!(
                f,
                "stderr capture failed, output is incomplete: {}",
                self.source
            ),
            _ => fmt::Display::fmt(&self.source, f),
        }
    }
}

impl Error for RunnerFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

pub(crate) fn failure(
    phase: RunnerPhase,
    termination: LeaderTermination,
    source: io::Error,
) -> io::Error {
    if RunnerFailure::from_io(&source).is_some() {
        return source;
    }
    io::Error::new(
        source.kind(),
        RunnerFailure {
            phase,
            termination,
            source,
        },
    )
}

/// Cleanup after an operation failed while a leader might still be running. Never infer
/// termination from the original error or rely on Drop's best-effort cleanup as proof.
pub(crate) fn stop_after_failure(
    child: &mut crate::OwnedChild,
    phase: RunnerPhase,
    source: io::Error,
) -> io::Error {
    let termination = match child.terminate_tree_bounded(Duration::from_secs(5)) {
        Ok(crate::ReapOutcome::Exited(_)) => LeaderTermination::Confirmed,
        _ => LeaderTermination::Unconfirmed,
    };
    failure(
        if termination == LeaderTermination::Unconfirmed {
            RunnerPhase::Cleanup
        } else {
            phase
        },
        termination,
        source,
    )
}

pub(crate) fn terminate_runner(
    child: &mut crate::OwnedChild,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    match child.terminate_tree_bounded(timeout) {
        Ok(crate::ReapOutcome::Exited(status)) => Ok(status),
        Ok(crate::ReapOutcome::TimedOut) => Err(failure(
            RunnerPhase::Cleanup,
            LeaderTermination::Unconfirmed,
            io::Error::new(
                io::ErrorKind::TimedOut,
                "the terminated process was not reaped within its bound",
            ),
        )),
        Err(error) => Err(stop_after_failure(child, RunnerPhase::Terminate, error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_spawn_is_never_reclassified_as_not_started() {
        let error = crate::owned::spawn_setup_failure(io::Error::from_raw_os_error(5));
        let error = failure(RunnerPhase::Spawn, LeaderTermination::NotStarted, error);
        let detail = RunnerFailure::from_io(&error).unwrap();
        assert_eq!(detail.phase(), RunnerPhase::Setup);
        assert_eq!(detail.termination(), LeaderTermination::Unconfirmed);
        assert_eq!(detail.source_io_error().raw_os_error(), Some(5));
        assert!(error
            .get_ref()
            .unwrap()
            .downcast_ref::<RunnerFailure>()
            .is_some());
    }

    #[test]
    fn confirmed_read_failure_retains_original_os_error() {
        let error = failure(
            RunnerPhase::Stdout,
            LeaderTermination::Confirmed,
            io::Error::from_raw_os_error(5),
        );
        let detail = RunnerFailure::from_io(&error).unwrap();
        assert_eq!(detail.phase(), RunnerPhase::Stdout);
        assert_eq!(detail.termination(), LeaderTermination::Confirmed);
        assert_eq!(detail.source_io_error().raw_os_error(), Some(5));
    }
}
