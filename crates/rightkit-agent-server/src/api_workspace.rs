// Standalone workspace files/artifacts module — D1 deliverable.
//
// Exposes eight typed daemon routes for the desktop "live workspace"
// surface: lazy list, lazy expand, bounded read, fingerprint-guarded
// atomic write, cancellable search (cooperative request-id registry),
// a workspace artifact list, a recents endpoint, and a coalesced SSE
// watcher backed by a poll+snapshot state machine.
//
// Policies enforced server-side (every path that resolves through the
// trust-root check goes through `resolve_within_trust_root`):
//   - traversal: `..`, absolute paths outside the root, and NUL bytes
//     are rejected before the FS is touched
//   - symlink: canonicalize-then-prefix; a symlink that escapes the
//     trust root is rejected with 403
//   - secret deny-list: casefolded, matched as full leaf, `.env*` family,
//     `id_rsa*` / `id_ed25519*` family, `*.pem` / `*.key` / `*.p12` /
//     `*.pfx` / `*.crt`, `secrets.{json,yaml,toml,yml}` and `.netrc`
//   - size: regular files > `MAX_TEXT_BYTES` reject the read/edit;
//     writes exceeding the same cap reject the PUT before fs write
//   - binary: a null-byte within the first `BINARY_SNIFF_BYTES` marks
//     the file as binary; reads return metadata only, edits refuse
//     outright
//   - permission: a read attempted with no read bit on the owner returns
//     403; an edit attempted without write bit returns 403
//   - fingerprint: every PUT includes the SHA-256 the client observed;
//     a mismatch returns 409 so stale-buffer writes can never clobber
//     a file the user no longer holds
//   - cancellation: a search carries a request id; a registry stores
//     the in-flight cancel flag per id and a second request with the
//     same id flips the original cancel (cooperative, shared registry)
//   - coalescing: the watcher registry keeps one entry per workspace;
//     multiple subscribers receive the same `WorkspaceWatchEvent` stream
//     and the underlying snapshot poll is shared (no blind rescan
//     timer per subscriber)
//
// The contract surfaces are kept free of central-router imports; Agent 4
// extends `api_contract/document.rs` and `api_contract/response_schemas.rs`
// with one call into the helpers below.
//
// ── Module map (ra-019 decomposition) ──────────────────────────────────────
// This file is now a facade only: route/OpenAPI wiring live here (via
// re-export) and every other responsibility lives in a submodule. The
// public/crate-visible surface — `workspace_routes()`, the three OpenAPI
// helper fns, route paths, wire formats, and every policy boundary listed
// above — is unchanged; only the on-disk organization moved.
//   - `types`   — shared bounds/constants, RFC3339 helpers, `PolicyError`,
//                 and every request/response DTO
//   - `policy`  — trust-root derivation, path resolution, secret-leaf,
//                 permission, and binary/hash helpers
//   - `routes`  — route registration plus the tree/file/artifact/recent
//                 handlers
//   - `search`  — the cancellation registry, the bounded streaming walker,
//                 and the search handler
//   - `watcher` — the coalesced SSE watcher registry/state-machine and the
//                 watch handler
//   - `openapi` — OpenAPI component/request/response schemas

mod openapi;
mod policy;
mod routes;
mod search;
mod types;
mod watcher;

pub use routes::workspace_routes;

pub(crate) use openapi::{
    workspace_openapi_components, workspace_openapi_request_schema,
    workspace_openapi_response_schema,
};

// ── Tests ────────────────────────────────────────────────────────────────────
// Unit tests exercise internals across every submodule (policy checks,
// the search walker, the watcher registry, OpenAPI coverage). Rather than
// duplicate them per-module, the facade re-imports what the suite below
// needs — the tests themselves are the original, unmodified assertions.
