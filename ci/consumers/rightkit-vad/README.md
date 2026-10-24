# rightkit-vad 0.1.2 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-vad =0.1.2` exactly with its default
features, which includes `silero` (the HeardRight feature set; CutRight enables the same feature on
macOS and Windows). It runs on Windows and macOS in hosted qualification.

What it proves, in plain sentences, through the public API only:

- The frame constants `FRAME_SAMPLES`, `FRAME_MS` and `SAMPLE_RATE` and their relation, the
  `VadConfig` defaults, `Span::len_ms` and `Span::sample_range` (including clamping), and the
  `Display` text of both `Error` variants.
- `scores_to_spans`: the start threshold, the lower end threshold (a dip between the two keeps a
  segment open), `min_silence` (a short gap does not split), `min_speech` (a short burst is dropped),
  padding, merging of padded neighbours, clamping to the signal duration, a segment that runs to the
  end of the signal, and empty input.
- `detect` over a `FrameScorer` test double: it resets the scorer first, zero-pads a trailing partial
  frame, and refuses inverted thresholds and non-finite samples with `Error::Input` before the scorer
  is touched, and non-finite or failing scores with `Error::Model`.
- `chunk_spans` and `ChunkConfig`: merging across a small gap (and not across exactly the gap),
  dropping short chunks, splitting over-long spans at the limit, and splitting on the quietest frame
  of the last quarter of the window when scores are supplied.
- `FrameStream`: whole 512-sample frames out of arbitrary pushes with the remainder carried over,
  `aggregate`, `threshold` and `set_threshold`, a scorer failure or non-finite score making `push`
  return `None` (unknown, not silence) with the stream failed until `reset`, and `reset` rewinding
  the scorer.
- `energy_scores` separating silence from a tone with no model, and feeding the same span policy.
- `SpanOptions`, `RmsCalibration` and `ResolvedSpans`: defaults resolve to `VadConfig::default()`,
  the calibrated threshold is eight times the ambient RMS with a noise floor, a minimum and a clamp
  to one, the silence-threshold cap and its opt-out (refused rather than silently clamped), and
  refusal of missing, empty, non-finite or inverted inputs.
- The Silero layer without a model: `SileroOptions` defaults, `Warmup` and `Spinning` defaults,
  `ExecutionProvider` support per target (CPU everywhere, CoreML on Apple, DirectML on Windows),
  `Backend::name`, and the `Silero::load` and `SpeechScorer::from_path` paths that fail before ONNX
  Runtime is touched: a model file that does not exist (a path under the running executable, which is
  a file) gives `Error::Model` containing "model not found", and a provider this OS cannot host is
  refused as "not available". With `energy_fallback` both become `SpeechScorer::Energy` with
  `Backend::Energy` and a recorded reason, and `SpeechScorer::scores` and `detect` equal the free
  functions.

What it deliberately does not prove: a real Silero model load or any inference, ONNX Runtime
binding, CoreML or DirectML execution (the model file and the runtime are supplied by an app and are
not in the archive), `SpeechScorer::from_bytes`, the `ort-tracing`, `download-binaries`, `tls-native`
and `copy-dylibs` features, and live audio capture.

State: in-memory values only, plus one path under the running executable that cannot exist. No
network, no files written, no process spawn, no user data. Runs in well under a second. Run only by
the CI consumer lane.
