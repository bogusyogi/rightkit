# rightkit-service

Local service discovery and authentication for Right Suite apps. It owns the convention for `service.json` discovery, a current-user-only transport (Unix socket on macOS and Unix, named pipe on Windows), per-app request allowlists, rank-based takeover, and the framed request, response, and event protocol built on `rightkit-framed-sidecar`.

**Use it** when two or more local processes must share one resident service on a machine, or when a local service must accept calls only from known apps running as the current user.

**Do not use it** for network services. The transport is local only: no TCP, no tokens.

## Features

None. The crate has no Cargo features.

## Platform support

- macOS and the BSDs: socket in a 0700 directory, mode 0600, peer uid checked with `getpeereid` on every accept and by every client.
- Linux and Android: same, with `SO_PEERCRED` (generic ABI; the mips, powerpc and sparc families report `Unsupported` rather than guess).
- Other Unix: the peer check reports `PeerError::Unsupported`, so the host refuses every peer. It never assumes the peer is you.
- Windows: named pipe with a DACL that allows only the current user SID, remote clients rejected, first-instance flag set, at most `HostOptions::max_pipe_instances` instances, client token SID checked on every accept, `SECURITY_IDENTIFICATION` clients that check the server SID.
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
| Ranking | Lower rank wins. A higher-ranked host writes `takeover.json`; the incumbent stops admitting work, lets in-flight requests reach a boundary (they return the retryable `host_handoff`), releases, and the requester binds. Clients re-read discovery and resubmit (`Client::call_retrying`). The incumbent looks for a request from another process once a second (a stat of `takeover.json`, no faster polling), so a takeover starts up to about a second after the request is written; a request written in the incumbent's own process wakes it at once. |
| Roots | `Suite::resolve()` fails closed when no absolute app-data root exists; `Suite::per_run_temp()` is the explicit alternative. Never the working directory. Default `<app data>/Orthic Labs/shared` (the root HeardRight, ScrapeRight and CutRight already use); `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR` override. |

Why a crate of its own and not a module of `rightkit-framed-sidecar`: the sidecar crate is a published, dependency-light, `unsafe`-free framing library. The service needs OS FFI (`getpeereid`, named pipes, SDDL), threads and a filesystem convention. Keeping them apart lets stdio-only consumers stay minimal; the service depends on the sidecar for its frames.

Tests are end-to-end: real host processes, real clients over the real socket. The `examples/` directory has `svc_host.rs` (host), `svc_call.rs` and `svc_claim.rs` (clients), `svc_mute.rs`, and `svc_race.rs`. `RIGHTKIT_SUITE_ROOT` (aliases `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR`) isolates a run from the user's installed apps.

## Threat model

**Defended against:** another local user (or a service running as one) connecting to the endpoint to call the service; another local user squatting on the endpoint name or path so a client sends its `hello` and requests to an impostor; a remote client (Windows pipes reject remote clients); a server that tries to impersonate the calling user through the pipe (clients open with `SECURITY_IDENTIFICATION`); connection floods growing the host without bound (Windows: `max_pipe_instances`).

**Layers.** (1) Filesystem or DACL: 0700 directory and 0600 socket on Unix, a protected DACL granting only the current user SID on Windows (`owner_only_security_attributes`, also for mutexes and events). (2) Independent kernel identity check of the peer on every accept and every connect: `verify_peer_is_current_user` (Unix) or `verify_pipe_peer_is_current_user` / `verify_pipe_server_is_current_user` (Windows). The check runs before any byte is read or written, fails closed (an unidentifiable peer is rejected, an unsupported platform rejects everyone), and a rejection is reported to the host as `ServiceEvent::PeerRejected`.

**Not defended against:** code already running as the current user. It passes every check by definition; the per-app allowlist is a key for routing, not a boundary against it. Root or Administrator. A process that hijacks the pid between the kernel recording it and the token lookup on Windows (the pipe DACL is the primary boundary there and the SID check is defense in depth). Local denial of service by the same user. A Unix peer is identified by effective uid; a setuid binary a different user runs appears as its owner.

**Platform note.** Cross-user behaviour cannot be exercised by the unit tests (it needs a second account). The decision function is tested with a foreign uid, the listener's rejection path (peer closed unread, reported as `Rejected`) is tested with an injected verifier, and an ignored test accepts a socket served by another account (`RIGHTKIT_SERVICE_FOREIGN_SOCKET`).

## Compatibility

Compatibility with HeardRight's speech service (see `rightkit-speech-client`): `service.json`/`takeover.json` tolerate peers that publish only `role` (rank via `ServiceConfig::known_roles`; an unresolvable rank is never displaced and never obliges a handoff), `engine_version` mirrors `version`, `ServiceConfig::endpoint` accepts a legacy endpoint name, `open_stream` gives raw line streams over the same user-only transport, and `pid_alive` is hardened (no process-group signals, Windows exit-code check).

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 22.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: undeclared.
- Disk operations ceiling: 1 operations per second.
- Required scale: 64 connections or more.
- Required hosts: macOS and Windows.
- Status exposure: `Hosted::status()` in `src/server.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.
