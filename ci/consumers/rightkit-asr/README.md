# rightkit-asr 0.3.0 registry consumer (PK18)

Publication qualification supplement, reviewed 2026-10-23; added after original
PK01–PK17 inventory. Standalone consumer pins registry `rightkit-asr =0.3.0` &
`rightkit-transcript =0.4.0`, outside workspace dependency resolution.

Release delta: transcript 0.4 type migration, including `TimedWord::source_ms`;
ASR 0.3.0 changelog records no decoder or algorithm change. Consumer checks
cross-crate type identity, legacy JSON omission/defaulting, present source-span
round-trip, public longform word assembly with default/disabled floor, cue
grouping & exact SRT/VTT output. Empty timed input produces no invented exports.
Assertions derive from `tests/longform_exports.rs`, `tests/whisper_contract.rs` &
public transcript type contract.

Strict production load receives an impossible model path beneath current
executable; it must return `Error::Config` naming missing `mel_nemo.onnx` before
any ONNX session. No downloads, model assets, files written, or inference.
Assetless checks qualify transcript/API compatibility & early model refusal;
they make no inference, decoder-accuracy, callback/window scheduling, cancellation,
accelerator, or platform-engine qualification claim. Default features are disabled;
HF tokenizer & optional engine features are outside this recipe.

Run only through CI consumer lane. Root integration owns execution evidence.
