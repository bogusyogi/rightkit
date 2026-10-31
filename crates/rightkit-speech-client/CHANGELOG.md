# Changelog

All notable changes to `rightkit-speech-client`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.4.1 — unpublished staging

- Dependencies: rightkit-daemon 0.4.0 -> 0.5.0 (the daemon version published for HeardRight, rightkit#1322). No public item of this crate exposes a `rightkit_daemon` type (the daemon types stay private to `lib.rs`), so the public API is unchanged and the bump is patch-level.

## 0.4.0 — unpublished staging

- BREAKING (D23): rightkit-transcript 0.3 -> 0.4. `TimedWord` (re-exported from `protocol` and the crate root, and carried in `words: Vec<TimedWord>`) gains a public `source_ms` field, so struct literals and cross-version type identity break. Supersedes the unpublished 0.3.7 staging number.
- Wire: `words[].source_ms` is optional and skipped when `None`; a 0.3 host or client payload without it still parses, and a `None` word serialises exactly as before. Tests: `tests/contract.rs` (`timed_word_source_ms_is_optional_on_the_wire`).

- Dependency floors: rightkit-daemon 0.4.0 (unpublished staging), rightkit-sidecar 0.3.0 and rightkit-transcript 0.4.0.

## 0.3.6

- Protocol minor 4 (additive, capability-gated, version-negotiated; Orthic-Labs/rightkit#108): a cancel frame and capture ownership for the Pulse voice layer (HeardRight dictation, Pulse conversation, batch, meeting). New `capture.rs` with `CaptureRole`, `CaptureOwner`, `CaptureCommand`, `CaptureReply`, `CaptureStatus`, `decide_capture` and the reference host state machine `CaptureArbiter`: priority HeardRight dictation > Pulse conversation > batch, one capture at a time, dictation preempts Pulse and the preempted owner releases at once, a meeting is never silently preempted (dictation needs an explicit confirmation), and heartbeat expiry fences the owner without handing the mic on until release is proven (`prove_release`).
- Wire: schemas `request_cancel` and `capture_control`, capabilities `request_cancel` and `capture_ownership`, `SERVICE_PROTOCOL_MINOR_CAPTURE` (4), `ServiceEnvelope::{cancel_job, capture_request, capture_state_frame, capture_reply, request_extended}`, `ExtendedRequest`, `is_allowed_schema_for_minor`. `request()`, `ServiceRequest`, `SERVICE_ALLOWED_SCHEMAS` and `is_allowed_schema` are unchanged, so older hosts and exhaustive matches are untouched, and a client never sends the new frames to a host without the capability.
- Client: `SpeechClient::{supports_cancel, supports_capture, cancel_job, capture, stop_capture}`. A cancelled file request also writes `request_cancel {job_id}` to a minor-4 host that lists it, then closes the connection as before, without waiting for a reply. `stop_capture` is the local Stop: it closes capture and pending uploads without waiting for a server ack.
- Tests: `tests/capture.rs` (rules, fencing, preemption, frames, allowlist compatibility), and `tests/e2e.rs` runs the cancel frame and capture frames against `hr_fake_host` minor 4 and checks a minor-3 host never sees them. The fake host speaks minor 4.

## 0.3.5

- Republished on the renamed dependencies: `rightkit-sidecar` (was `rightkit-framed-sidecar`). The 0.3.4 on crates.io was packaged before the renames and still named the old crates.
## 0.3.4

- Input staging: `StageInput` (`Auto`, the default; `Always`; `Never`) on
  `TranscribeOptions::stage`, and `TranscribeOptions::stage_limit` (default
  `DEFAULT_STAGE_LIMIT_BYTES`, 4 GiB). `Auto` copies a file the host cannot be expected to
  read (outside the home and per-user temporary directories, under `/Volumes/` on macOS, or on
  another drive on Windows) into a client-owned staging directory under the temporary directory,
  submits the copy, and removes it when the request ends, on success, error or cancel. `Never`
  submits the path as given. `API.md`, "Staging", has the rule.
- Breaking for exhaustive matches on `ClientError` (0.3.4 is unpublished): new
  `InputUnreadable { path, message }`, returned when the host reports that it could not read the
  input (its `E_PERMISSION`, or FFmpeg's denied-read text) instead of the generic `Engine` error,
  and new `StageLimitExceeded { path, bytes, limit }`. `ClientError` is not `#[non_exhaustive]`.
- Launch policy: `Launch` (`Auto`, the default and the historical behaviour; `Never`
  uses a live host as found, never launches and never requests a takeover),
  `HostLauncher`, `HeardRightLauncher` (the default launcher) and
  `transcribe_via_service_policy`. `transcribe_via_service_with` is unchanged and
  delegates with `Launch::Auto`. New `ClientError::NoLiveHost`, returned by `Launch::Never`
  with no live host; code that matches `ClientError` exhaustively needs a new arm.
- Cancel outcome: `cancel_request` and `CancelOutcome`. The protocol has no cancel
  acknowledgement, so the outcomes report what a status query shows (finished, still
  active, no longer active, unknown). Closing a connection does not stop a job on
  HeardRight's queue host; the `CancelToken` docs now say so.
- Cues: `Cue`, `parse_srt_cues` and `FileTranscript::cues`, parsed from the host's own
  `srt` field, which the host already sends. No client-side regrouping.
- Input contract: `preflight_file` checks the two host-enforced conditions a client can
  check (UTF-8 path, existing absolute path). `API.md` lists every host check and its error.
- Added `API.md` and `INDEX.md` to the package.
- Folds the unpublished 0.3.3 entry into this release.

- `TimedWord` is now `rightkit_transcript::TimedWord` (re-exported at the same
  path). Wire fields (`text`, `start_ms`, `end_ms`) are unchanged; the Rust field
  type narrows from `i64` to `u64` (a host sending a negative time is now a
  decode error instead of a negative value). Adds a `rightkit-transcript` dependency.
