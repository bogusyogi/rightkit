//! Standalone HTTP workspace surface over the existing per-session browser
//! store (`rightkit_agent_tools::coding_tools::browser::BrowserStore`).
//!
//! Lane discipline (D3 — shared browser workspace):
//!
//! - This file owns `pub fn browser_workspace_routes(services)` and the
//!   `browser_workspace_openapi_*` helpers. The central router in
//!   `api_router.rs` and the central OpenAPI component assembly in
//!   `api_contract/*` are out of scope; Agent 4 wires the standalone
//!   fragment in by calling
//!   `browser_workspace_routes(services).with_state(state)` and merging
//!   `browser_workspace_openapi_components()`.
//!
//! - All session-affecting state lives behind the `BrowserWorkspaceServices`
//!   trait Agent 4 implements. The fragment itself holds the per-RightKit-
//!   session annotation store and the route handlers; the trait carries the
//!   `BrowserStore`, host policy, action log, pause flag, profile kind/label,
//!   viewport, capability gate, and the dispatch seam into the live agent.
//!   Agent 4's implementation is what reaches into `LiveSession::browser_*`,
//!   not this file.
//!
//! - Security boundaries preserved by the fragment's contract (Agent 4's
//!   implementation MUST honor these when wiring the trait impls):
//!     * SSRF guard (`rightkit_agent_tools::coding_tools::browser_fetch::ssrf_guard_reject_reason`)
//!       runs before any navigation. Host policy is additive on top.
//!     * Browser capability gate runs before any state mutation.
//!     * Page text and console output are wrapped via
//!       `rightkit_agent_tools::coding_tools::browser_fetch::wrap_untrusted_web_text`.
//!     * Browser profiles are ephemeral by default; named shared profiles
//!       require the operator to flip `profile_kind` through the consent
//!       endpoint below.
//!     * Page text is bounded by `MAX_TEXT_BYTES`, screenshots by
//!       `MAX_SCREENSHOT_BYTES`, console by RightKit's own 500-line cap.
//!     * Action log, host policy, pause, viewport, profile state all
//!       live with the session and are dropped with it.
//!
//! - Tests in this file are deterministic where they exercise pure
//!   shape/decision logic and gated on `RIGHTKIT_BROWSER_WORKSPACE_TESTS=1`
//!   for anything that would require a running daemon or live Chrome.
//!
//! ## Decomposition (ra-020)
//!
//! This module is a pure structural facade over six private submodules —
//! `types`, `policy`, `services`, `routes`, `openapi`, `annotations` — split
//! out of a single 3,325-line file per the Architect decomposition map
//! (`.audit/20260901-daemon-deep-r3/architect/decomposition-plans.md`). The
//! route inventory, `BrowserWorkspaceServices` trait, JSON wire shapes,
//! OpenAPI schemas, and lease/authority semantics (SSRF guard, capability
//! gate, `WORKSPACE_USER_OWNER` lease-owner contract) are unchanged — only
//! *where the code lives* changed. Every symbol this module re-exports below
//! is byte-identical in name and signature to the pre-decomposition file;
//! the four external call sites (`api_router.rs`, `api_contract/document.rs`,
//! `api_contract/request_schemas.rs`, `api_contract/response_schemas.rs`)
//! require no changes.

mod annotations;
mod openapi;
mod policy;
mod routes;
mod services;
mod types;

pub use services::daemon_browser_workspace_routes;

pub use openapi::{
    browser_workspace_openapi_components, browser_workspace_openapi_request_schema,
    browser_workspace_openapi_response_schema,
};
