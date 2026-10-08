# rightkit-align

Product-neutral alignment of a written script to the timed words an ASR engine returned. It normalises pronunciation, aligns script tokens to ASR words with edit distance, gives each token separate lexical and timing confidence, rejects mismatched or ambiguous anchors, and reports coverage for designated control names.

Catalog capability: `script-alignment`. Not published (`publish = false`, version `0.0.0`).

## What it owns

- `align(script, asr, designated)`: returns one `Anchor` per script token, in script order, plus an `Alignment` with `Coverage`.
- `asr_words_from_seconds(items)`: adapts `(text, start_s, end_s)` tuples from `rightkit-asr` output into `rightkit_transcript::Word` values, marked as model-timed with no confidence.
- Rejection reasons (`RejectReason`) and coverage failures (`CoverageFailure`). A rejected anchor never satisfies coverage.
- Timing checks (`timing` module): boundary errors against a tolerance on the ASR lattice.

Script-token identity is kept through normalisation. The crate holds no audio, models or I/O. It depends on `rightkit-transcript` only for the `Word`, `Span` and seconds adapter types.

## When to use it

- Use it to check which script tokens the ASR actually said, and where, before timing narration or control cues.
- Use it when a mismatch or an ambiguous repeat must be rejected rather than guessed.

Do not use it to transcribe audio (use `rightkit-asr`) or to store transcripts (use `rightkit-transcript`).

## Cargo features

None. The crate has no `[features]` table.

## Usage

Illustrative. The function names and signatures match `src/lib.rs`, and the fixtures in `tests/alignment.rs` use the same types:

```rust
use rightkit_align::{align, asr_words_from_seconds};

let asr = asr_words_from_seconds([
    ("open", 0.00, 0.40),
    ("the", 0.40, 0.56),
    ("file", 0.56, 0.96),
]);
let alignment = align("Open the file", &asr, &[]);
for anchor in &alignment.anchors {
    println!("{} {:?}", anchor.text, anchor.outcome);
}
```

## Platform support

Pure Rust with no platform-specific code. Not tied to an operating system.

## Version and changes

Not published. See [INDEX.md](../../INDEX.md). No CHANGELOG.md in this crate.
