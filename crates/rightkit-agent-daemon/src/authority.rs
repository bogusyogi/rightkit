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
// domain crate depending on the binary crate. `rightkit-agent-server` re-exports
// these two functions from its `api_permissions` module so its ~58 existing call
// sites are unaffected.

use axum::http::{HeaderMap, StatusCode};

/// Name of the request header that carries the caller's source trust, unless the host sets its own.
pub const DEFAULT_SOURCE_TRUST_HEADER: &str = "x-rightkit-agent-source-trust";

static SOURCE_TRUST_HEADER: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the source-trust header name for the process (lower-case, a valid HTTP header name).
/// Call once at start-up; returns `false` when the name is invalid or one was already set.
pub fn set_source_trust_header(name: &str) -> bool {
    axum::http::HeaderName::from_bytes(name.to_ascii_lowercase().as_bytes()).is_ok()
        && SOURCE_TRUST_HEADER.set(name.to_ascii_lowercase()).is_ok()
}

/// The source-trust header name in force.
pub fn source_trust_header() -> &'static str {
    SOURCE_TRUST_HEADER
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_SOURCE_TRUST_HEADER)
}

fn normalized_source_trust(headers: &HeaderMap) -> Result<Option<String>, StatusCode> {
    let Some(source_trust) = headers.get(source_trust_header()) else {
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
