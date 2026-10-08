# rightkit-transcript

Product-neutral word-timed transcript contract for Right Suite applications. It owns the `Transcript`, `Word`, `Span`, and `ModelRef` types with their schema constants (`SCHEMA`, `LEGACY_SCHEMA`), cue segmentation (`build_cues` with `CaptionConfig`), alignment checks (`check_alignment`), and import and export for SRT, WebVTT, timing JSON, and plain text.

**Use it** to hold word-level timings from a transcription engine, turn them into captions, and read or write SRT, VTT, or timing JSON.

**Do not use it** to transcribe audio. It holds and converts timings produced by an engine such as `rightkit-speech-client` or `rightkit-asr`.

## Public surface

- Contract: `Transcript`, `Word`, `Span`, `ModelRef`, `EndSource`, `TranscriptError`, `SCHEMA`, `LEGACY_SCHEMA`.
- Words: `words_from_seconds`, `words_from_transcript`, `TimedWord`, `MAP_DURATION_TOLERANCE_MS`.
- Segmentation: `normalize_words`, `build_cues`, `CaptionConfig`, `Cue`, `Profile`.
- Alignment: `check_alignment`, `AlignmentIssue`, `ALIGNMENT_TOLERANCE_MS`.
- Export: `to_srt`, `to_vtt`, `to_plain_text`, `to_timing_json`, `Newline`.
- Parsing: `parse_srt`, `parse_vtt`, `parse_timing_json`, `words_from_cues`, `ParsedCue`, `ParseError`.

## Features

One test-only feature, `asr-roundtrip`, which enables the engine-backed end-to-end test in `tests/asr_roundtrip.rs` (pulls `rightkit-asr`). It is off by default. Dependencies: `serde`, `serde_json`, `thiserror`.

## Platform support

The crate has no platform-specific code. It is pure Rust.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`. The crate's manifest sets `publish = false`, and its version is `0.0.0`.

## Example

Illustrative; not compiled as a doctest. For compiled round trips, see `tests/roundtrip.rs`; the engine-backed round trip is `tests/asr_roundtrip.rs` (run with `--features asr-roundtrip`).

```rust,ignore
use rightkit_transcript::{build_cues, parse_srt, to_srt, words_from_seconds, CaptionConfig, Newline};

let words = words_from_seconds([("hello", 0.0, 0.4), ("world", 0.5, 0.9)]);
let cues = build_cues(&words, &CaptionConfig::default());
let srt = to_srt(&cues, Newline::Lf);
let reparsed = parse_srt(&srt)?;
```
