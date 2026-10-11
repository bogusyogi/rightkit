# Changelog

## 0.1.0 - 2026-10-11

Breaking changes to the alignment API (unpublished crate, no compatibility shim). Implements RK4 items a-e of `docs/plans/2026-10-11-cutright-contract-asks-c2-rk9-rk3-rk4.md`.

- **Identity.** `align` takes a `ScriptSpec` of `ScriptToken { word_id, step_id, token_index, text }` in place of one string. `ScriptSpec::from_text(step_id, text)` generates ids `w{index}`. Designated anchors are `DesignatedAnchor { word_id, target_id }` in place of `ControlAnchor { token_id, label }`. Every row echoes `word_id`, `step_id` and `target_id`. Coverage failures and boundary truth are keyed by `word_id`. `ScriptSpec::validate` rejects empty or duplicate word ids and non-increasing token indices. `align` panics on an invalid script.
- **Units.** `Span { start_ms, end_ms, start_sample, end_sample, sample_rate }` gives every accepted span in milliseconds and samples. `align` takes the ASR `asr_sample_rate` and panics on zero.
- **Timing confidence.** For a model-timed word on a lattice with frame step `lattice_ms`, boundary trust is `clamp(tolerance_ms / lattice_ms, 0, 1)`. The token's `timing_confidence` is that times its lexical match. Parakeet (80 ms, 50 ms tolerance) gives 0.625 where 0.0.1 reported a flat 1.0. `AlignOptions { tolerance_ms (default 50), lattice_ms (default Some(80)) }`. `lattice_ms: None` asserts continuous boundaries. The formula is documented in `timing`.
- **Clauses.** `Alignment::clauses: Vec<Clause { clause_id, step_id, first_word_id, last_word_id, start, end }>`, split on `. , ; : ? !` (after closing quotes or brackets), on a newline, and on a step change. A clause is emitted only when its first and last spoken tokens are accepted.
- **Contract.** `Alignment::to_contract()` returns `AlignmentContract`, identified by `contract: "rightkit.alignment"` and `contractVersion: "1.0.0"`, the keys the sibling contracts use. CutRight's proposed `schema: "rightkit.alignment.v1"` maps onto these two keys. The schema is `contracts/alignment.v1.schema.json`. Serde rejects any other contract name, and any contract version other than 1.0.x.
- `Alignment` no longer derives serde; the wire type is `AlignmentContract`. `Alignment` carries `timing_tolerance_ms`, and `boundary_errors` uses it.
- `EndSource::SourceBoundary` (added in rightkit-transcript) takes the model-timed bound: the end is the model's own, clamped to the media duration.
- Dev-dependency `serde_json` for the round-trip and schema tests. Fixtures on real annotated audio (RK4 item f) are not included. They wait on CutRight audio, and `tests/alignment.rs` uses synthetic lattice audio.

## 0.0.1 - 2026-10-10

- Metadata alignment only, no code change: `license` is `MIT OR Apache-2.0` (was `MIT`) and `repository` is `https://github.com/bogusyogi/rightkit`, matching the other RightKit crates; `rust-version` was already `1.98.0`. Add `LICENSE-MIT` and `LICENSE-APACHE`.
