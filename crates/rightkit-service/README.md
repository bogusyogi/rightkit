# rightkit-service

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

Tests are end-to-end: real host processes (`examples/svc_host.rs`), real clients over the real socket. `RIGHTKIT_SUITE_ROOT` (aliases `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR`) isolates a run from the user's installed apps.

Compatibility with HeardRight's speech service (see `rightkit-speech-client`): `service.json`/`takeover.json` tolerate peers that publish only `role` (rank via `ServiceConfig::known_roles`; an unresolvable rank is never displaced and never obliges a handoff), `engine_version` mirrors `version`, `ServiceConfig::endpoint` accepts a legacy endpoint name, `open_stream` gives raw line streams over the same user-only transport, and `pid_alive` is hardened (no process-group signals, Windows exit-code check).
