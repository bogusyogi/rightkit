# rightkit-speech-client

Client and protocol contract for the Orthic Labs shared speech service, which runs one resident speech engine per machine. It owns endpoint discovery (`connect`, `read_live_record`, `plan_host`), bounded v1 JSON frames for health, capabilities, and transcription, the job queue status (`job_status`, `Progress`), and the takeover request helpers. It is built on `rightkit-daemon` and is wire-compatible with HeardRight's v1 host.

**Use it** in a Right Suite app that needs speech transcription from the shared service, or that must act as a compatible host.

**Do not use it** to run the speech engine. Hosting remains in the speech engine application; this crate provides the client contract.

## Input contract

The speech host opens the input path itself, so a path the client can read may still be denied
to the host (on macOS, a file on an external volume). By default the client copies such a file
into a client-owned staging directory under the per-user temporary directory and submits the
copy. A path under the user's home or temporary directory is submitted unchanged (on Windows, when it
is on the same drive; on macOS, unless it is under `/Volumes/`; `host_readable_by_default` in
`src/stage.rs`). The copy is removed when the request ends. Staged files over 4 GiB (the limit
is set with `stage_limit`) are refused with `StageLimitExceeded`. A host read denial returns `InputUnreadable` with the caller's
path. `API.md`, "Staging" and "Input contract", has the rule and the host's own checks.

## Features

None. The crate has no Cargo features. Dependencies: `rightkit-daemon`, `rightkit-framed-sidecar`, `rightkit-transcript`, `serde`, `serde_json` (`Cargo.toml:13-17`).

## Platform support

`src/lib.rs` has no `cfg` gates. `endpoint_for` in `src/protocol.rs:183-194` picks a named pipe on Windows (`cfg!(windows)`) and a Unix socket elsewhere; the `cfg(windows)` and `cfg(unix)` transport gates are in `rightkit-daemon` (`src/transport.rs`). Transport details are defined by `rightkit-daemon`.

Published version: see the generated `INDEX.md` at the repository root. Reference: [API.md](API.md) (every public item, its errors, and the host's input contract); file map: [INDEX.md](INDEX.md). Release notes: [CHANGELOG.md](CHANGELOG.md).

## Example

The compiled example is `examples/hr_fake_host.rs`, which implements both host sides from raw JSON. For wire-level acceptance, see `tests/contract.rs`, `tests/e2e.rs`, and `tests/windows_pipe.rs`. Run the example with `rightkit cargo run -p rightkit-speech-client --example hr_fake_host`.

## Contract extensions (capability-gated)

A client uses an extension only when the host lists its capability in `engine_capabilities`; requests to older hosts stay byte-identical to v1.

| Capability | Minor | Client | Host must |
|---|---|---|---|
| `file_transcription_verbatim` | 2 | `TranscribeOptions::verbatim()` sends `verbatim: true`; refused (`Unsupported`) if the host does not list it | Decode with `DetokPolicy { strip_fillers: false, .. }` through the one shared formatter; keep the flag on the queued job through every window and dictation pause; skip all later cleanup/polish; text and word timings from the same policy |
| `file_transcription_queue` | 3 | No stall timeout; typed `Progress::Queued {position, reason}` / `Running {fraction}`; `job_status(id)`; resubmits with the same `request_id` when the host is gone (closed connection, dead pid, withdrawn/replaced `service.json`) | Answer each file request at once with `queued` (`job_id` = `request_id`, `position`, `reason`: `dictation_active` / `behind_jobs` / `loading_model`); re-send state on change and at least every 2 s; `running` + `fraction`; treat `request_id` as idempotent (attach, or replay a result finished within 10 min); answer `file_transcription_status {job_id}` with one frame or `speech_service_job_unknown`; allowlist `file_transcription_status` |

`examples/hr_fake_host.rs` implements both host sides from raw JSON; `tests/e2e.rs` runs the wire-level checks against `hr_fake_host` on Unix (`#![cfg(unix)]`); `tests/windows_pipe.rs` runs a named-pipe round trip on Windows against a raw pipe server written in the test.
