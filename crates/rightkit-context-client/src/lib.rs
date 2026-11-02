//! One-shot client for the Membrane resident endpoint.
//!
//! Every command first asks the resident, with a 100 ms connect budget. A
//! `ClientError::NotRunning` means nothing answered and nothing was sent; the
//! caller then runs the same request in process against the stores. Any other
//! error means the resident was reached, so a write must not be replayed.

use rightkit_context_protocol::{
    ResidentAnswerV1, ResidentOp, ResidentRequestV1, RESIDENT_APP, RESIDENT_ENDPOINT_VERSION,
    RESIDENT_MAX_FRAME_BYTES, RESIDENT_SERVICE,
};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

pub mod error;
pub use error::ClientError;

/// How long a one-shot command waits for the resident before running in process.
pub const CONNECT_BUDGET: Duration = Duration::from_millis(100);

/// Bound on one request once connected. Pull can take real time; this keeps a
/// wedged resident from hanging a hook.
pub const DEFAULT_REQUEST_DEADLINE: Duration = Duration::from_secs(30);

pub struct ResidentClient {
    client: rightkit_daemon::Client,
}

impl ResidentClient {
    /// Connect to the resident whose discovery record lives under
    /// `<state_root>/runtime/membrane/service.json`. The endpoint itself is
    /// owner-only; the daemon checks the peer on both sides.
    pub fn connect(state_root: &Path) -> Result<Self, ClientError> {
        Self::connect_within(state_root, CONNECT_BUDGET)
    }

    pub fn connect_within(state_root: &Path, budget: Duration) -> Result<Self, ClientError> {
        let not_running = |reason: String| ClientError::NotRunning { reason };
        let suite = rightkit_daemon::Suite::at(state_root)
            .map_err(|error| not_running(error.to_string()))?;
        // The resident is `membrane serve`, the one binary every launcher starts (the engine's
        // `Helpers/membrane`, the Mac notch's copy of the same file, a development build), so the
        // program behind the endpoint must be this executable: the OS names it before a byte is
        // written. A resident of another install is refused and the caller runs in process.
        let exe = std::env::current_exe().map_err(|error| not_running(error.to_string()))?;
        let config = rightkit_daemon::ClientConfig::new(RESIDENT_SERVICE, RESIDENT_APP)
            .suite(suite)
            .deadline(Some(budget));
        let client = rightkit_daemon::Client::connect_expecting(
            &config,
            &rightkit_daemon::HostExpectation::executable(exe),
        )
        .map_err(|error| not_running(error.into_service_error().message))?;
        client.set_deadline(Some(DEFAULT_REQUEST_DEADLINE));
        Ok(Self { client })
    }

    pub fn set_deadline(&self, deadline: Option<Duration>) {
        self.client.set_deadline(deadline);
    }

    /// Send one request and return the answer's `data`.
    pub fn call(&self, op: ResidentOp, args: Value) -> Result<Value, ClientError> {
        let request = ResidentRequestV1::new(op, args);
        let params = serde_json::to_value(&request).map_err(|error| ClientError::Protocol {
            message: error.to_string(),
        })?;
        if params.to_string().len() > RESIDENT_MAX_FRAME_BYTES {
            return Err(ClientError::Protocol {
                message: "request exceeds frame limit".into(),
            });
        }
        let reply = self
            .client
            .call(op.as_str(), params)
            .map_err(transport_error)?;
        let answer: ResidentAnswerV1 =
            serde_json::from_value(reply).map_err(|error| ClientError::Protocol {
                message: error.to_string(),
            })?;
        if answer.v != RESIDENT_ENDPOINT_VERSION {
            return Err(ClientError::Protocol {
                message: format!("unsupported answer version {}", answer.v),
            });
        }
        match (answer.ok, answer.data, answer.error) {
            (true, Some(data), _) => Ok(data),
            (true, None, _) => Ok(Value::Null),
            (false, _, Some(error)) => Err(ClientError::Refused {
                code: error.code,
                message: error.message,
                retryable: error.retryable,
            }),
            (false, _, None) => Err(ClientError::Protocol {
                message: "failure answer carries no error".into(),
            }),
        }
    }
}

/// Connect and make one call. `NotRunning` is the caller's cue to run in process.
pub fn try_resident(state_root: &Path, op: ResidentOp, args: Value) -> Result<Value, ClientError> {
    ResidentClient::connect(state_root)?.call(op, args)
}

fn transport_error(error: rightkit_daemon::ServiceError) -> ClientError {
    if error.code == rightkit_daemon::ERROR_TIMEOUT {
        ClientError::Timeout {
            message: error.message,
        }
    } else {
        ClientError::Transport {
            message: format!("{}: {}", error.code, error.message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_maps_to_timeout_and_keeps_the_message() {
        let error = transport_error(rightkit_daemon::ServiceError::new(
            rightkit_daemon::ERROR_TIMEOUT,
            "no reply in 30s",
        ));
        assert_eq!(
            error,
            ClientError::Timeout {
                message: "no reply in 30s".into()
            }
        );
        assert_eq!(error.code(), "deadline_exceeded");
        assert!(!error.is_not_running());
    }

    #[test]
    fn other_daemon_errors_map_to_transport_with_their_code() {
        let error = transport_error(rightkit_daemon::ServiceError::new("peer_closed", "gone"));
        assert_eq!(
            error,
            ClientError::Transport {
                message: "peer_closed: gone".into()
            }
        );
        assert_eq!(error.code(), "transport_failed");
    }

    #[test]
    fn a_state_root_with_no_resident_is_not_running() {
        let root = std::env::temp_dir().join(format!(
            "rightkit-context-client-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let outcome = ResidentClient::connect(&root);
        let _ = std::fs::remove_dir_all(&root);
        let error = outcome
            .err()
            .expect("no resident answers under an empty state root");
        assert!(error.is_not_running(), "{error:?}");
        assert_eq!(error.code(), "not_running");
        assert!(error
            .to_string()
            .starts_with("membrane resident not running: "));
    }

    #[test]
    fn budgets_are_the_documented_values() {
        assert_eq!(CONNECT_BUDGET, Duration::from_millis(100));
        assert_eq!(DEFAULT_REQUEST_DEADLINE, Duration::from_secs(30));
    }
}
