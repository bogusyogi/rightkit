# rightkit-transcript

Product-neutral word-timed transcript contract for Right Suite applications. It owns the `Transcript`, `Word`, `Span`, and `ModelRef` types with their schema constants (`SCHEMA`, `LEGACY_SCHEMA`), cue segmentation (`build_cues` with `CaptionConfig`), alignment checks (`check_alignment`), and import and export for SRT, WebVTT, timing JSON, and plain text.

**Use it** to hold word-level timings from a transcription engine, turn them into captions, and read or write SRT, VTT, or timing JSON.

**Do not use it** to transcribe audio. It holds and converts timings produced by a transcription engine such as `rightkit-asr`, or returned in `rightkit-speech-client` file results (`FileTranscript::words`).

## Public surface

- Contract: `Transcript`, `Word`, `Span`, `ModelRef`, `EndSource`, `TranscriptError`, `SCHEMA`, `LEGACY_SCHEMA`, `ACCEPTED_SCHEMAS`.
  Reads: `Transcript::from_json` (lenient, defaults missing fields), `Transcript::from_json_strict` (CutRight rules: rejects unknown fields, requires `source_blake3`, `language`, `vad`, `model`).
  Writes: `Transcript::to_json`, `Transcript::to_json_with_schema_id` (readable ids only).
  Editing: `Transcript::constrain_duration` (clamp to media duration).
- Words: `words_from_seconds`, `words_from_transcript`, `TimedWord` (`new`, `from_seconds`, `start_secs`, `end_secs`, `From<(text, start_ms, end_ms)>`; field `source_ms`), `MAP_DURATION_TOLERANCE_MS`.
- Segmentation: `normalize_words`, `build_cues`, `build_cues_with_metrics`, `wrap_lines`, `LineMetrics`, `CaptionConfig`, `Cue`, `Profile`; lightweight engine-side grouping `group_words`, `GroupRules`, `cues_from_spans`.
- Alignment: `check_alignment`, `AlignmentIssue`, `ALIGNMENT_TOLERANCE_MS`.
- Export: `to_srt`, `to_vtt`, `to_plain_text`, `to_timing_json`, `Newline`.
- Parsing: `parse_srt`, `parse_vtt`, `parse_timing_json`, `words_from_cues`, `ParsedCue`, `ParseError`.

## Schema ids

| Id | Constant | Read | Written |
|---|---|---|---|
| `rightkit.transcript.v1` | `SCHEMA` | yes | by default (`from_words`) |
| `cutright.transcript.v1` | `LEGACY_SCHEMA` | yes | only through `to_json_with_schema_id` |

`ACCEPTED_SCHEMAS` lists exactly these ids. Any other id is rejected on read. A
parsed transcript keeps the id it was read with.

## Features

No features. Dependencies: `serde`, `serde_json`, `thiserror`.

## Platform support

The crate has no platform-specific code. It is pure Rust.

Release notes: see `CHANGELOG.md` in this crate. The crate is published to crates.io.

## Example

Illustrative; not compiled as a doctest. For compiled round trips, see `tests/roundtrip.rs`; the engine-backed round trip lives in `rightkit-asr` (`tests/transcript_roundtrip.rs`), because `rightkit-asr` depends on this crate.

```rust,ignore
use rightkit_transcript::{build_cues, parse_srt, to_srt, words_from_seconds, CaptionConfig, Newline};

let words = words_from_seconds([("hello", 0.0, 0.4), ("world", 0.5, 0.9)]);
let cues = build_cues(&words, &CaptionConfig::default());
let srt = to_srt(&cues, Newline::Lf);
let reparsed = parse_srt(&srt)?;
```

## Measured caption layout

`TimedWord::source_ms` records original `(start_ms, end_ms)` when
`words_from_transcript` maps source time into output time. `normalize_words`
preserves it, including when output overlaps are clamped. Constructors default
it to `None`; serde accepts old payloads & omits absent spans. Existing struct
literals need `source_ms: None` or should use `TimedWord::new` in 0.4.0.

`LineMetrics` takes fallible `measure_width: &dyn Fn(&str) -> Result<u64, String>`.
Capture font face & size in that callback. It measures whole candidate lines,
including spaces; widths must use `max_width_px` units. Identical text should
produce identical widths. RightKit has no font or renderer dependency.

```rust
use rightkit_transcript::{
    build_cues_with_metrics, wrap_lines, CaptionConfig, LineMetrics, TimedWord,
};

// Substitute your renderer's whole-line width measurement here.
let measure = |text: &str| -> Result<u64, String> {
    Ok(text.chars().map(|c| if c == 'W' { 12 } else { 5 }).sum())
};
let metrics = LineMetrics {
    measure_width: &measure,
    max_width_px: 80,
    uppercase: false,
};
let words = [TimedWord::new("Wide", 0, 800), TimedWord::new("words", 800, 1600)];
let cues = build_cues_with_metrics(&words, &CaptionConfig::default(), Some(&metrics))?;
let lines = wrap_lines("Wide words", 42, Some(&metrics))?;
assert_eq!(cues[0].lines, lines);
# Ok::<(), String>(())
```

Both character & width limits apply to cue splitting, short-cue merging &
two-line balancing. `uppercase: true` measures uppercase text & counts expanded
characters while preserving original case in returned lines. Overlong words
split at Unicode scalar boundaries; an overwide glyph or callback failure
returns `Err(String)`. Single-word cues keep their timings & word indices even
when wrapping exceeds `max_lines`. Without metrics, `wrap_lines` keeps overlong
words intact & `build_cues_with_metrics(..., None)` matches `build_cues`.

Profile mapping for CutRight: YouTube → `Profile::Standard`, TikTok →
`Profile::Vertical`, Reels → `Profile::Compact`.
