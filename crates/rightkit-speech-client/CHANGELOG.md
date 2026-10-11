# Changelog

All notable changes to `rightkit-speech-client`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.3.4

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
