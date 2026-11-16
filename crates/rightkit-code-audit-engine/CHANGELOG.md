# Changelog

## 0.2.0

- Documentation fixes the doc build refused under `-D warnings`: two intra-doc links to items that are private or no longer exist are plain code, and an `adjudicated:<candidateId>` placeholder no longer reads as an unclosed HTML tag (Mac release compile 2026-11-07).
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.
- The package no longer ships `src/native_providers/security/history_qualification_record.rs`, the emitter of a qualification record for a history test module (`history_qualification_tests`) that does not exist here; no `mod` declared the file. No public item, feature or behaviour changes; the compiled library is unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `legion-audit` (engine).
