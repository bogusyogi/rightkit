# Changelog


## 0.3.5

- Republished on the renamed dependencies: `rightkit-sidecar` (was `rightkit-framed-sidecar`). The 0.3.4 on crates.io was packaged before the renames and still named the old crates.
All notable changes to `rightkit-speech-client`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

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
