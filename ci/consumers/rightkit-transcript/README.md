# rightkit-transcript 0.4.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-transcript =0.4.0` exactly with its
default feature set (the crate has no features). It also depends on `serde_json = "1"` to assert the
JSON wire form of `TimedWord` and the timing JSON. It runs on Windows and macOS in hosted
qualification; the crate has no platform-specific code.

What it proves, in plain sentences, through the public API only:

- The schema constants `SCHEMA`, `LEGACY_SCHEMA` and `ACCEPTED_SCHEMAS`, plus the two tolerance
  constants `MAP_DURATION_TOLERANCE_MS` and `ALIGNMENT_TOLERANCE_MS`.
- `TimedWord`: `new`, `from_seconds`, `start_secs`, `end_secs`, the tuple `From`, the `source_ms`
  field defaulting to `None`, and its wire form (omitted when absent, written when present, optional
  on read). `words_from_seconds` rounds to the nearest millisecond and clamps negative and
  non-finite times to zero.
- `normalize_words`: trims and collapses whitespace, drops empty and zero-length words, and clamps
  an overlapping previous word.
- `Transcript`: `from_words` (including the rule that the duration never undercuts the last word),
  `text`, `timed_words`, `validate`, `to_json`, lenient `from_json`, strict `from_json_strict`,
  `to_json_with_schema_id` for the legacy id and its refusal of unknown ids, and
  `constrain_duration` (never grows, marks moved words `SourceBoundary`, drops emptied VAD spans,
  refuses and leaves the value untouched when there is less room than words). Each error path is
  asserted by exact `TranscriptError` variant: `Schema`, `EmptyWord`, `Unsorted`, `PastEnd`,
  `BadVad`, `Json`, `MissingField`.
- Caption building: `build_cues` for all three `Profile` values and `CaptionConfig::for_profile`,
  determinism, `check_alignment` (clean and each `AlignmentIssue` variant), `build_cues_with_metrics`
  with a fixed width table, the no-metrics equivalence with `build_cues`, `wrap_lines` (including an
  overlong word, a failing width callback and a glyph wider than the line), `group_words` with
  `GroupRules`, and `cues_from_spans`.
- Export: `to_srt` (LF and CRLF), `to_vtt`, `to_plain_text`, `to_timing_json`, and the hostile-text
  rules (`-->` is neutralised, VTT escapes markup).
- Import: `parse_srt`, `parse_vtt`, `parse_timing_json` and `words_from_cues`, including the BOM,
  CRLF, markup, entity, cue-setting, NOTE-block and short-timestamp quirks, `ParseError::Timestamp`
  and `ParseError::Json`, and an export-then-import round trip that keeps text and times.
- `words_from_transcript`: cut-boundary mapping that drops a word inside a removed range, records the
  original source span, keeps it through normalisation, cue building and a JSON round trip, and
  applies the duration tolerance at and just beyond its limit.

What it deliberately does not prove: transcription (an engine such as `rightkit-asr` produces the
words), audio decoding, and any real font or renderer; the width callback is a fixed table.

State: in-memory strings only. No network, no files, no process spawn, no user data. Runs in well
under a second. Run only by the CI consumer lane.
