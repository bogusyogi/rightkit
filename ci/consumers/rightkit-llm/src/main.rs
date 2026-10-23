// rightkit-llm 0.3.1 consumed by exact registry pin, default features, outside the workspace.
//
// Proves the attempt-billing contract a host relies on to decide whether a failed call may have cost money:
// a failure that never reached the provider (connect-phase transport or timeout, a request that could not be
// built, a missing credential) is Spend::NotSpent; a failure after the request was dispatched (headers, body or
// redirect phase) is Spend::Unknown; a 4xx other than 408 is NotSpent and a 5xx is Unknown. Public API only,
// through the crate's re-exported HTTP types.
// Does not prove: providers, failover, streaming, tools, pricing, or the CLI route.
// State: in-memory values only. No network, no files, no process spawn, no user data.
use rightkit_llm::http::Phase;
use rightkit_llm::{HttpError, LlmError, Spend};

fn transport(phase: Phase) -> LlmError {
    LlmError::Http(HttpError::Transport {
        message: "fixture".into(),
        connect_phase: phase == Phase::Connect,
        phase,
    })
}

fn timeout(phase: Phase) -> LlmError {
    LlmError::Http(HttpError::Timeout {
        message: "fixture".into(),
        phase,
    })
}

fn main() {
    assert_eq!(transport(Phase::Connect).billing().spend, Spend::NotSpent);
    assert_eq!(timeout(Phase::Connect).billing().spend, Spend::NotSpent);
    assert_eq!(
        LlmError::Http(HttpError::InvalidRequest("fixture".into()))
            .billing()
            .spend,
        Spend::NotSpent
    );
    for phase in [Phase::Headers, Phase::Body, Phase::Redirect] {
        assert_eq!(
            transport(phase).billing().spend,
            Spend::Unknown,
            "{phase:?}"
        );
        assert_eq!(timeout(phase).billing().spend, Spend::Unknown, "{phase:?}");
    }
    assert_eq!(
        LlmError::Status {
            status: 400,
            message: "fixture".into()
        }
        .billing()
        .spend,
        Spend::NotSpent
    );
    assert_eq!(
        LlmError::Status {
            status: 503,
            message: "fixture".into()
        }
        .billing()
        .spend,
        Spend::Unknown
    );
    println!("OK rightkit-llm 0.3.1 (registry consumer): attempt billing by phase and status");
}
