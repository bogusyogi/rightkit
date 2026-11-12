//! Governed intake for sealed Adapt remediation proposals (PL-011).
//!
//! Adapt approval is evidence, not mutation authority. One daemon-lifetime
//! host consumes each typed proposal through canonical intake, an explicit
//! policy gate & typed application receipts.

use axum::{
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::post,
    Router,
};
use rightkit_agent_engine_core::adapt_consumer::{ApplicationReceipt, SealedRemediationProposalV1};

use crate::api::DaemonState;

#[path = "adapt_application_host.rs"]
pub mod adapt_application_host;

use adapt_application_host::daemon_adapt_application_host;
pub use adapt_application_host::{install_adapt_application_host, AdaptApplicationHost};

pub fn adapt_routes() -> Router<DaemonState> {
    Router::new().route("/adapt/proposals/apply", post(apply_adapt_proposal_handler))
}

async fn apply_adapt_proposal_handler(
    headers: HeaderMap,
    Json(proposal): Json<SealedRemediationProposalV1>,
) -> Result<Json<ApplicationReceipt>, StatusCode> {
    // Route authentication is independent from proposal approval. The
    // daemon's auth middleware stamps this header; missing/invalid identity
    // never reaches the mutation host.
    if headers
        .get(crate::host_identity::header("authenticated"))
        .and_then(|value| value.to_str().ok())
        != Some("true")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(daemon_adapt_application_host().apply(&proposal)))
}
