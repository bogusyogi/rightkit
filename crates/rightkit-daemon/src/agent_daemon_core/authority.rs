// Trusted-source authority gate: `x-host_source-trust`.
//
// ## Decision matrix — why "header absent" defaults to trusted
//
// `enforce_trusted_source_authority` returns `Ok(())` (trusted) whenever the
// `x-host_source-trust` header is ABSENT. This is deliberate, not an
// oversight — every call site in this daemon sits BEHIND the global bearer-
// token gate (`auth::require_token` in `api_router.rs`, applied to every
// route except `/health`), so "no trust header" already means "an
// authenticated holder of the daemon's launch token made this call." The
// header exists to let an ALREADY-authenticated caller VOLUNTARILY declare a
// request as lower-trust (e.g. the scheduler/spec-runner marking a
// workspace-generated prompt, or a test asserting rejection) — it is not an
// attacker-facing signal, because an attacker who can't pass the bearer
// token can't reach these handlers at all regardless of this header.
//
// Verified 2026-07-04 (audit lane A-2, item 4): a blanket default-deny
// (treat "header absent" as untrusted) would break every legitimate local
// caller — the Tauri UI, the CLI, and the scheduler's OWN internal calls
// never send this header and rely on the bearer token alone. Tightening the
// default would 403 the entire local product for no security gain, since
// the bearer token is the actual authority boundary here.
//
// ## Where a stricter check WOULD matter: remote-channel ingest
//
// The `api_remote/*` family (`/remote/channels/{id}/fanout`,
// `/remote/channels/{id}/outbound`, `/remote/bindings`,
// `/remote/channels/{id}/profile`) is the one place a "remote channel" name
// could be misread as "reachable directly from the internet." It is not:
//
// - the host app's Telegram integration is OUTBOUND long-polling
//   (`telegram_payloads.rs` calls Telegram's `getUpdates` — the daemon pulls,
//   Telegram never pushes to an HTTP route on this daemon). There is no
//   webhook-receiver endpoint on this daemon's router that accepts inbound
//   content directly from an external network peer.
// - Every `/remote/*` POST handler is a LOCAL management-plane call (create
//   a binding, fan text out to bound sessions, send an outbound webhook) —
//   the caller is always the Tauri UI or CLI, gated by the same bearer
//   token as every other route, and each handler already calls
//   `enforce_trusted_source_authority` explicitly (verified: `fanout`,
//   `outbound`, `bindings`, `profile` — see `tests_fanout.rs` for the
//   rejection-path test shape).
// - The actual untrusted-content boundary is `submit_remote_channel_text`
//   (`api_state_impl_05.rs`), which ALREADY unconditionally stamps every
//   fanned-out message as `ContentTrust::RemoteAgentUntrusted` regardless of
//   this header — the message content trust does not depend on the
//   authority-header default at all.
//
// Conclusion: no remote-ingest HTTP route accepts unauthenticated external
// input today, so there is no lower-trust default to add for "requests that
// provably originated from a remote channel" beyond what already exists. If
// a future channel adds a genuine inbound webhook receiver (something an
// external service POSTs to directly), THAT route must treat the ABSENCE of
// this header as lower trust, not blanket-trusted — it would sit outside
// the bearer-token boundary this analysis relies on.
//
// Moved out of `host-app/src/api_permissions/authority.rs` into this
// binary-independent crate so both the daemon binary and any extracted
// domain crate (e.g. `rightkit-agent-engine-attention`) can call the gate without the
// domain crate depending on the binary crate. The bin re-exports these two
// functions from `crate::agent_daemon_core::api_permissions` so its ~58 existing call sites are
// unaffected.

use axum::http::{HeaderMap, StatusCode};

fn normalized_source_trust(headers: &HeaderMap) -> Result<Option<String>, StatusCode> {
    let Some(source_trust) = headers.get("x-rightkit-agent-source-trust") else {
        return Ok(None);
    };
    let source_trust = source_trust.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Some(
        source_trust
            .trim()
            .chars()
            .filter(|ch| !matches!(ch, '-' | '_' | ' '))
            .collect::<String>()
            .to_ascii_lowercase(),
    ))
}

pub fn lower_source_trust(
    headers: &HeaderMap,
) -> Result<Option<rightkit_agent_engine_protocol::ContentTrust>, StatusCode> {
    let Some(normalized) = normalized_source_trust(headers)? else {
        return Ok(None);
    };
    match normalized.as_str() {
        "remoteagentuntrusted" | "untrusted" => Ok(Some(
            rightkit_agent_engine_protocol::ContentTrust::RemoteAgentUntrusted,
        )),
        "tooluntrusted" => Ok(Some(
            rightkit_agent_engine_protocol::ContentTrust::ToolUntrusted,
        )),
        _ => Ok(None),
    }
}

pub fn enforce_trusted_source_authority(headers: &HeaderMap) -> Result<(), StatusCode> {
    if lower_source_trust(headers)?.is_some() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

#[cfg(test)]
mod authority_decision_matrix_tests {
    use super::{enforce_trusted_source_authority, lower_source_trust};
    use axum::http::{HeaderMap, StatusCode};

    #[test]
    fn absent_header_is_trusted_by_design() {
        // Documents the decision-matrix conclusion above: no header at all
        // (the shape of every local Tauri/CLI call) is treated as trusted
        // because it already passed the daemon's bearer-token gate.
        let headers = HeaderMap::new();
        assert_eq!(lower_source_trust(&headers), Ok(None));
        assert!(enforce_trusted_source_authority(&headers).is_ok());
    }

    #[test]
    fn explicit_untrusted_header_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-rightkit-agent-source-trust",
            "untrusted".parse().unwrap(),
        );
        assert_eq!(
            enforce_trusted_source_authority(&headers),
            Err(StatusCode::FORBIDDEN)
        );
    }

    #[test]
    fn explicit_remote_agent_untrusted_header_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-rightkit-agent-source-trust",
            "remote-agent-untrusted".parse().unwrap(),
        );
        assert_eq!(
            enforce_trusted_source_authority(&headers),
            Err(StatusCode::FORBIDDEN)
        );
    }

    #[test]
    fn unrecognized_header_value_does_not_grant_lower_trust() {
        // An unrecognized value normalizes to None (unknown label), so the
        // gate does not spuriously reject on typos/garbage — it only rejects
        // recognized lower-trust labels.
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-rightkit-agent-source-trust",
            "bogus-value".parse().unwrap(),
        );
        assert_eq!(lower_source_trust(&headers), Ok(None));
        assert!(enforce_trusted_source_authority(&headers).is_ok());
    }
}
