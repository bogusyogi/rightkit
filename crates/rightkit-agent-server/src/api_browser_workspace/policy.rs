//! SSRF and capability policy — the guard that runs before any navigation or
//! state mutation. Kept independent from the router per the Architect
//! decomposition map (ra-020): pure decision logic, no `RoutesState`
//! dependency.
//!
//! Re-implemented (not re-used) from `rightkit_agent_tools::coding_tools::browser_fetch::ssrf_guard_reject_reason`
//! because the guard is `pub(crate)` inside `rightkit_agent_tools::coding_tools`; re-using it
//! from the standalone fragment would require widening the crate boundary,
//! which is out of scope. The policy is the same: loopback-permit /
//! metadata-host-block / DNS-resolve-private-target-reject.

use axum::http::StatusCode;
use axum::response::Json;
use rightkit_agent_policy::{
    CapabilityClass, PermissionDecision, PermissionEngine, PermissionMode,
};
use rightkit_agent_tools::coding_tools::browser::{
    browser_navigation_reject_reason_with_allowlist, BrowserNavigationTrustV1,
};
use serde_json::Value;

use super::types::{workspace_error, CapabilityVerdict};

pub(crate) fn browser_capability_verdict(
    mode: PermissionMode,
    trust: rightkit_agent_policy::WorkspaceTrust,
) -> CapabilityVerdict {
    let verdict =
        PermissionEngine::new().evaluate(CapabilityClass::Browser, mode, trust, None, false);
    CapabilityVerdict {
        allowed: verdict.decision == PermissionDecision::Allow,
        reason: if verdict.decision == PermissionDecision::Allow {
            format!("authenticated live session ({})", verdict.policy_source)
        } else {
            format!("{} ({})", verdict.reason, verdict.policy_source)
        },
    }
}

pub(crate) async fn workspace_ssrf_check(
    url: &str,
    navigation_trust: &BrowserNavigationTrustV1,
    host_allowlisted: bool,
) -> Result<(), (StatusCode, Json<Value>)> {
    if url.trim().is_empty() {
        return Err(workspace_error(
            StatusCode::BAD_REQUEST,
            "ssrf_refused",
            "URL is empty.".to_string(),
        ));
    }
    if let Some(reason) = browser_navigation_reject_reason_with_allowlist(
        "open",
        url,
        navigation_trust,
        host_allowlisted,
    )
    .await
    {
        return Err(workspace_error(
            StatusCode::FORBIDDEN,
            "ssrf_refused",
            reason,
        ));
    }
    Ok(())
}
