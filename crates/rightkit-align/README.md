# rightkit-align

Product-neutral alignment of a written script to the timed words an ASR engine returned. It normalises pronunciation, aligns script tokens to ASR words with edit distance, gives each token separate lexical and timing confidence, rejects mismatched or ambiguous anchors, and reports coverage for designated control names.

Catalog capability: `script-alignment`. Not published (`publish = false`, version `0.1.0`).

## What it owns

- `ScriptSpec` / `ScriptToken`: the script as addressable tokens (`word_id`, `step_id`, `token_index`, `text`). `ScriptSpec::from_text(step_id, text)` generates ids `w{index}`.
- `align(script, asr, asr_sample_rate, designated, options)`: returns one `Anchor` per script token, in script order, with `Clause`s and a `Coverage` report. Every row echoes `word_id`, `step_id` and `target_id`.
- `Span`: an accepted span in milliseconds and in ASR samples at the aligner's sample rate.
- `AlignOptions`: `tolerance_ms` (default 50) and `lattice_ms` (default `Some(80)`, Parakeet). Timing confidence is derived in the `timing` module.
- `Alignment::to_contract()`: the versioned document (`AlignmentContract`), identified by the sibling-style keys `contract: "rightkit.alignment"` and `contractVersion: "1.0.0"`. The JSON Schema is `contracts/alignment.v1.schema.json`. CutRight's proposed `schema: "rightkit.alignment.v1"` maps onto those two keys; this crate does not emit a `schema` key.
- `asr_words_from_seconds(items)`: adapts `(text, start_s, end_s)` tuples from `rightkit-asr` output into `rightkit_transcript::Word` values, marked as model-timed with no confidence.
- Rejection reasons (`RejectReason`) and coverage failures (`CoverageFailure`). A rejected anchor never satisfies coverage.
- Timing checks (`timing` module): boundary errors against the alignment's tolerance on the ASR lattice.

Script-token identity is kept through normalisation. The crate holds no audio, models or I/O. It depends on `rightkit-transcript` only for the `Word` and `EndSource` types and the seconds adapter (`words_from_seconds`). `Span` is defined in this crate (`src/lib.rs:81`).

## When to use it

- Use it to check which script tokens the ASR actually said, and where, before timing narration or control cues.
- Use it when a mismatch or an ambiguous repeat must be rejected rather than guessed.

Do not use it to transcribe audio (use `rightkit-asr`) or to store transcripts (use `rightkit-transcript`).

## Cargo features

None. The crate has no `[features]` table.

## Usage

Illustrative. The names and signatures match `src/lib.rs`, and `tests/alignment.rs` uses the same types:

```rust
use rightkit_align::{align, asr_words_from_seconds, AlignOptions, DesignatedAnchor, ScriptSpec};

let asr = asr_words_from_seconds([
    ("open", 0.00, 0.40),
    ("the", 0.40, 0.56),
    ("file", 0.56, 0.96),
]);
let script = ScriptSpec::from_text("step-1", "Open the file");
let designated = [DesignatedAnchor {
    word_id: "w2".into(),
    target_id: "file-field".into(),
}];
let alignment = align(&script, &asr, 16_000, &designated, &AlignOptions::default());
for anchor in &alignment.anchors {
    println!("{} {} {:?}", anchor.word_id, anchor.text, anchor.outcome);
}
let document = alignment.to_contract(); // contract "rightkit.alignment", contractVersion "1.0.0"
```

## Platform support

Pure Rust with no platform-specific code. Not tied to an operating system.

## Version and changes

Not published. See [CHANGELOG.md](CHANGELOG.md) for the 0.1.0 API change. Fixtures on real annotated narration audio are pending on CutRight audio.
