# rightkit-service

Local service discovery and authentication for Right Suite apps. It owns the convention for `service.json` discovery, a current-user-only transport (Unix socket on macOS and Unix, named pipe on Windows), per-app request allowlists, rank-based takeover, and the framed request, response, and event protocol built on `rightkit-framed-sidecar`.

**Use it** when two or more local processes must share one resident service on a machine, or when a local service must accept calls only from known apps running as the current user.

**Do not use it** for network services. The transport is local only: no TCP, no tokens.

## Features

None. The crate has no Cargo features.

## Platform support

- macOS and Unix: socket in a 0700 directory, mode 0600, peer uid checked with `getpeereid`.
- Windows: named pipe with a DACL that allows only the current user SID, remote clients rejected, first-instance flag set.
- Platform gates: `cfg(windows)`, `cfg(unix)`, `cfg(target_os = "macos")`, and `cfg(target_os = "linux")` in `src/discovery.rs`, `src/record.rs`, `src/suite.rs`, and `src/transport.rs`.

Published version: see `INDEX.md` at the repository root. Release notes: see `CHANGELOG.md`, if present; this crate has none, so use `INDEX.md`.

## Example

The compiled examples live in `examples/` (`svc_host.rs`, `svc_call.rs`, `svc_claim.rs`, `svc_mute.rs`, `svc_race.rs`). Run them through `rightkit cargo run -p rightkit-service --example <name>`.

## Convention

The one discovery and authentication convention for local Right Suite services: the shared speech service, the in-app `rightkit-control` server, `right-ctl`, and any future one.

| Concern | Convention |
|---|---|
| Discovery | `<suite>/runtime/<service>/service.json` (atomic, owner-only). Stale when its endpoint does not answer. |
| Transport | macOS/Unix: socket in a 0700 dir, mode 0600, peer uid checked (`getpeereid`). Windows: named pipe, DACL = current user SID only, remote clients rejected, first-instance flag. No TCP, no tokens. |
| Frames | `rightkit-framed-sidecar` JSON lines: `hello`, `request`, `event`, `response`; errors carry `retryable`. |
| Allowlist | Per calling app: which methods it may call. Unknown app rejected at `hello`. `ping` always allowed. |
| Ranking | Lower rank wins. A higher-ranked host writes `takeover.json`; the incumbent stops admitting work, lets in-flight requests reach a boundary (they return the retryable `host_handoff`), releases, and the requester binds. Clients re-read discovery and resubmit (`Client::call_retrying`). |
| Roots | `Suite::resolve()` fails closed when no absolute app-data root exists; `Suite::per_run_temp()` is the explicit alternative. Never the working directory. Default `<app data>/Orthic Labs/shared` (the root HeardRight, ScrapeRight and CutRight already use); `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR` override. |

Why a crate of its own and not a module of `rightkit-framed-sidecar`: the sidecar crate is a published, dependency-light, `unsafe`-free framing library. The service needs OS FFI (`getpeereid`, named pipes, SDDL), threads and a filesystem convention. Keeping them apart lets stdio-only consumers stay minimal; the service depends on the sidecar for its frames.

Tests are end-to-end: real host processes, real clients over the real socket. The `examples/` directory has `svc_host.rs` (host), `svc_call.rs` and `svc_claim.rs` (clients), `svc_mute.rs`, and `svc_race.rs`. `RIGHTKIT_SUITE_ROOT` (aliases `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR`) isolates a run from the user's installed apps.

## Compatibility

Compatibility with HeardRight's speech service (see `rightkit-speech-client`): `service.json`/`takeover.json` tolerate peers that publish only `role` (rank via `ServiceConfig::known_roles`; an unresolvable rank is never displaced and never obliges a handoff), `engine_version` mirrors `version`, `ServiceConfig::endpoint` accepts a legacy endpoint name, `open_stream` gives raw line streams over the same user-only transport, and `pid_alive` is hardened (no process-group signals, Windows exit-code check).
