# Changelog

All notable changes to `rightkit-speech-client`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.3.3

- `TimedWord` is now `rightkit_transcript::TimedWord` (re-exported at the same
  path). Wire fields (`text`, `start_ms`, `end_ms`) are unchanged; the Rust field
  type narrows from `i64` to `u64` (a host sending a negative time is now a
  decode error instead of a negative value). Adds a `rightkit-transcript` dependency.
