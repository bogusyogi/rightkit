# Changelog

## 0.1.1

- Depends on `rightkit-adapt` 0.2.0 instead of `rightkit-adapt-taste`: the same code under the name the shape review gives it (`rightkit-adapt-taste` 0.1.2 re-exports `rightkit-adapt`, so the adapt types this crate exposes, such as `PreferenceDeliveryCandidateV1`, are the same items under either path). Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.

- `paths::resolve` no longer calls `Options::qa_overrides` and `Options::test_root`, which rightkit-fs 0.4.11 removed with the test-mode residue.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `membrane-runtime` (engine).
