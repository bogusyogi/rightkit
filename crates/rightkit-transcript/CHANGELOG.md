# Changelog

All notable changes to `rightkit-transcript`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## Unreleased

- Fixed: the `span()` helper behind cue splitting no longer underflows on a reversed group (last end
  before first start) or an empty group. Both measure zero, as `group_words` does. The helper is
  private, so no public signature changed and no checked variant was added.

## 0.3.0

Existing values serialise exactly as before. This is a minor (0.x) bump because two
enums gained variants: `EndSource::SourceBoundary` and `TranscriptError::MissingField`.
Downstream exhaustive `match`es on either enum need a new arm.

- Fixed: `group_words` no longer underflows (a panic in debug builds) on a word whose
  end precedes its start; such a span is treated as zero length.
- Added `EndSource::SourceBoundary` (serde tag `"source_boundary"`), the variant
  CutRight writes when a word is clamped to the media boundary. CutRight project
  files now parse. Downstream exhaustive `match`es on `EndSource` need a new arm;
  the workspace's `rightkit-align::word_timing_confidence` is one.
- Added `Transcript::constrain_duration`, ported from CutRight: clamps to a media
  duration (never grows), reserves one millisecond per trailing word, marks
  changed words `SourceBoundary`, clamps and drops VAD spans, and fails without
  mutation when the cap is below the word count.
- Added `Transcript::from_json_strict`, which matches CutRight: it rejects unknown
  fields at every level and requires `source_blake3`, `language`, `vad` and `model`
  (with `model.id`, `model.revision`, `model.backend`), refusing an empty
  `source_blake3`. Absence is reported as the new `TranscriptError::MissingField`.
  `from_json` stays lenient and keeps its defaults.
- Added `Transcript::to_json_with_schema_id` and `ACCEPTED_SCHEMAS`. The writer
  accepts only readable ids, so a consumer can keep emitting
  `cutright.transcript.v1` and the output still reads back.

## 0.2.0

- `TimedWord` is now the single word-timing owner for the suite: added
  `TimedWord::from_seconds`, `start_secs`, `end_secs` and
  `From<(text, start_ms, end_ms)>`. `words_from_seconds` is unchanged.
- Added `group_words` / `GroupRules` (the lightweight single-line cue grouper
  `rightkit-asr` used to hand-roll) and `cues_from_spans`, so engines export
  through `to_srt` / `to_vtt` / `to_timing_json` instead of private copies.
- Removed the test-only `asr-roundtrip` feature and its optional
  `rightkit-asr` dependency (it would cycle now that `rightkit-asr` depends on
  this crate). The engine-backed round trip moved to `rightkit-asr`
  (`tests/transcript_roundtrip.rs`).

## 0.1.0

First crates.io release line. Ships the word-timed `Transcript` contract, cue
segmentation (`build_cues`, `CaptionConfig`), alignment checks, and SRT, WebVTT,
timing JSON, and plain-text import and export. The engine-backed round trip
(`asr-roundtrip` feature) is test-only and off by default.

Earlier history is in the git log of this repository; this file starts at the
publication point.
