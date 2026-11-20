# Changelog

## 0.1.3
- Add `vocabulary_limits` (`Limits`, `validate_term`, `validate_aliases`, `check_import_count`; moved from HeardRight `commands/vocabulary.rs`).
- Add `text_pipeline::capitalize_first_alpha` (moved from HeardRight; same body).

## 0.1.2 (unpublished)

- Add opt-in `store`: host-configured lazy vocabulary loading & atomic ordered saves, retaining existing JSON format & operation signatures. Unreadable or malformed JSON permanently refuses persistence; saves release store locks before waiting.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight.
