# Changelog

## 0.1.1

- Session transcripts of any agent server's host, not only CodeRight's (`rightkit-agent-server` names its rows after the app it runs in, `<app>_event_v1`): `session_transcript` append accepts a row whose type is `<host>_event_v1` for the host its `host` and provenance source name, records that host as the provenance source, and refuses a batch whose rows name a host other than the stream's. `adapt mine-session` normalizes a stream for the host its rows name (`rightkit-agent-transcript` 0.1.3 `parse_absorbed_rows_for_host`) and records it on the taste review source; reverification parses for the recorded host (CodeRight when none is recorded). CodeRight streams behave as before.
- Depends on `rightkit-adapt` 0.2.0 instead of `rightkit-adapt-taste`: the same code under the name the shape review gives it (`rightkit-adapt-taste` 0.1.2 re-exports `rightkit-adapt`, so the adapt types this crate exposes, such as `PreferenceDeliveryCandidateV1`, are the same items under either path). Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.

- `paths::resolve` no longer calls `Options::qa_overrides` and `Options::test_root`, which rightkit-fs 0.4.11 removed with the test-mode residue.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `membrane-runtime` (engine).
