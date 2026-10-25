# Changelog

All notable changes to `rightkit-asr`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.3.1 — unpublished staging

- New (plan 2.8, gap C5-08): `WhisperCpp::load_verified(library, model, threads, deadline)` and `Whisper::load_verified(model, options)` verify the whisper.cpp library and the ggml files beside it against the native-library catalog for this host before dlopen (`rightkit_ffmpeg::verified_libraries_in`), all or nothing, and load the canonical verified path with the existing `load`. The set is the `whisper-cpp` row matching the library's file name plus every non-stale `ggml` row of this host, each of which must sit in the same directory: a digest mismatch, an absent file or a row that is still `planned` refuses the load. An `approved` or `pinned` row serves. The error is the new `whisper::VerifiedLoadError` (`Verification(NativeCatalogError)` before anything is opened, `Load(Error)` once every file matched; `From<VerifiedLoadError> for Error` exists), a separate type because `Error` gets no variant before the next breaking release. `WhisperCpp::load_verified_in(catalog, os, arch, library, model, threads, deadline)` takes a catalog and platform, for tests. `WhisperCpp::load` and `Whisper::load` are unchanged and documented as the unverified path for tests and development builds; `Whisper::load_verified` loads a `WhisperModel::CoreMlDir` as `Whisper::load` does (no native library of ours to verify). Not built for Android (`cfg(not(target_os = "android"))`, like the loader itself). The digests are checked at the call, not while the loader maps the files, the same window `rightkit_ffmpeg::verified_library` documents; the model file is not verified. Tests use digest-only fixture files (no real library): tampered whisper file and ggml sibling, absent sibling, planned rows, a relative path and the canonical-path rule, and the verified path reaching the open-library error for fixture bytes.
- `WhisperCpp::load_verified` and the `load_verified` README section now say that the Windows whisper.cpp and ggml catalog rows are `planned`, so the verified load fails closed on Windows today, and that Windows host proof is pending for the verbatim (`\\?\`) canonical path it would load (`std::fs::canonicalize` returns that form on Windows; nothing here has run it through the loader). The ggml set filter now asks the new public `NativeRecord::serves_host(os, arch)` of `rightkit-ffmpeg` instead of a private copy of its platform rule. No behaviour change.
- Dependency: `rightkit-ffmpeg` becomes a regular dependency on `cfg(not(target_os = "android"))` targets (it was a dev-dependency only), at floor `0.2.11`: the first release that carries the API this loader uses (`verified_libraries_in`, `serving_file`, `hostProof`). The dev-dependency entry is unchanged.

## 0.3.0 (2026-10-23, crates.io)

- BREAKING (D23): the rightkit-transcript dependency moves from 0.3 to 0.4. `rightkit_asr::longform::TimedWord` re-exports `rightkit_transcript::TimedWord`, which gains a public `source_ms: Option<(u64, u64)>` field in 0.4, and `assemble_words`, `timed_pieces_to_words*`, `group_cues` and `render_subtitles` take or return it, so struct literals and cross-version type identity break. No decoder or algorithm change: the 57 production source files and 9 packaged tests are byte-identical to 0.2.4 (G13 review).
- Packaging: `include` now ships `tests/**` (`tests/common/mod.rs`, `tests/coreml_cache/mod.rs` and `tests/fixtures/*`), which the packaged tests already reference (`e2e_whisper.rs` `mod common`, `e2e_parakeet.rs` detok fixture). Dev-dependency rightkit-speech-client floor 0.4.0.

## 0.2.4

Patch for Orthic-Labs/rightkit#207 (Parakeet ONNX for Android). NOT COMPILED OR RUN when written, and never built for an Android target.

- Orthic-Labs/rightkit#325 (additive, NOT COMPILED OR RUN): `ExecutionConfig` (`ModelConfig`; the issue called it `ExecutionOptions`, no such type exists) gains `decoder_intra_threads: Option<usize>` and `with_decoder_intra_threads(n)`. When set, the decoder/joint session of Parakeet TDT, transducer, EOU, unified, multitalker and Nemotron uses it instead of `intra_threads` (on the auxiliary provider for the transducer and Nemotron, as before); the encoder keeps `intra_threads`. Unset (the default) leaves every session as before. The Cohere decoder is not changed. The new public field means a downstream struct literal of `ModelConfig` needs `..Default::default()`; none exists in this repo.
- Orthic-Labs/rightkit#324 (additive, NOT COMPILED OR RUN): `LanguageTranscription { result: TranscriptionResult, language: ReportedLanguage }` and `Transcriber::transcribe_samples_with_language` / `transcribe_file_with_language`, so the language comes back with the result on every backend. `TranscriptionResult` is unchanged (no struct-literal break). Whisper (whisper.cpp and CoreML) reports `Forced` or `Detected`; `CoreMlParakeet` reports its bundle's fixed language (`ModelFixed`) or `Unknown`; every other engine (ONNX Parakeet) takes the default, `Unknown` with no code. An unknown language is never guessed, so callers must not substitute `"en"`.

- ort `=2.0.0-rc.14` (via `rightkit-onnx` 0.2.6, Orthic-Labs/ember#230). NOT BUILT OR RUN. rc.14 sessions need an environment handle, so every `Session::builder()` call (Parakeet transducer, TDT, EOU, Nemotron, multitalker, unified, Cohere, Sortformer, Whisper-ONNX model files) is now `Session::builder(rightkit_onnx::ort_environment()?)`; those builds also pick up `rightkit-onnx`'s environment options (telemetry off). `Cargo.toml` requires `rightkit-onnx` 0.2.6. No public API change; the `ort` types asr re-exposes (`SessionBuilder`, `ort::Error`) are rc.14's.
- whisper.cpp is no longer compiled for `target_os = "android"`. `whisper/cpp.rs` (the `libloading` FFI) is replaced there by `whisper/cpp_android.rs`: `WhisperCpp` keeps its name and `load` signature but is uninhabited, and `WhisperCpp::load` (so `WhisperModel::Ggml`) returns the typed `Error::unsupported` refusal (`Error::is_unsupported()`), as before. `Whisper` and the other public types are unchanged.
- `libloading` moved to `[target.'cfg(not(target_os = "android"))'.dependencies]`; no dlopen dependency remains in the Android build. `tests/e2e_whisper_ggml.rs` is `cfg(not(target_os = "android"))` because it uses `libloading`.
- Review result, no change needed: Apple dependencies and modules were already `cfg(target_os = "macos")`; `ort` is reached only through `rightkit_onnx::ort` (no direct `ort` dependency), and this crate never loads ONNX Runtime itself, so on Android the app binds `libonnxruntime.so` through `rightkit_onnx::configure_android_runtime` (or `init_environment_default_loader`) before the first model load.
- For an Android build use `default-features = false` (optionally `xnnpack`): the default `whisper-tokenizers` feature pulls the C `tokenizers`/`onig` build, which the Parakeet TDT path does not need. Without it `Parakeet` (CTC) and `ParakeetEOU` are absent; `ParakeetTDT` and `Transducer` remain. Not built for Android.

## 0.2.3

Additive patch for Orthic-Labs/rightkit#196. NOT COMPILED OR RUN when written.

- `ReportedLanguage { code: Option<String>, source: LanguageSource }` and `LanguageSource` (`Forced`, `Detected`,
  `ModelFixed`, `Unknown`), a sibling of `TranscriptionResult` (its fields are unchanged, so struct literals keep compiling).
- `CoreMlParakeet::language()` and `PipelineMeta::language()`: the fixed language a bundle declares through an optional
  `"language"` key in `pipeline.json`, else `Unknown` with no code. Parakeet has no language input or output (the shipped v3
  export is multilingual), so CoreML Parakeet never guesses; callers must not substitute `"en"`.
- `Whisper::reported_language(&transcript)`: `Forced` when a code was set, `Detected` under `Auto`.

## 0.2.2

Additive patch for the Android plan (Orthic-Labs/rightkit#151). NOT COMPILED OR RUN, here or for Android: every
`target_os = "android"` path below is unverified.

- Static audit, no code change needed: every Apple dependency (`objc2`, `objc2-core-ml`, `objc2-foundation`) and every
  Apple module (`parakeet_coreml`, `whisper::coreml`, `ComputeUnits`, `WhisperCoreMl`) is already behind
  `cfg(target_os = "macos")`, so none of it is compiled for Android. The issue named `cfg(target_vendor = "apple")`; the
  gates stay `macos` on purpose, because the CoreML modules and their dependencies are macOS-only today and widening to
  iOS would compile code that is not built there. No AVFoundation or Speech code exists in this crate.
- `platform::EngineSupport` (`parakeet_onnx`, `parakeet_coreml`, `whisper_coreml`, `whisper_cpp`; `current()` and the pure
  `for_target(macos, android, whisper_tokenizers)`), re-exported as `rightkit_asr::EngineSupport`.
- `Error::unsupported(what)` and `Error::is_unsupported()` (message prefix `UNSUPPORTED_PREFIX`). It is an `Error::Config`
  carrying that prefix, so no `Error` variant was added and an exhaustive downstream `match` keeps compiling; a dedicated
  variant waits for a breaking release.
- Android refusals, clean `Error::unsupported` instead of a loader failure: `WhisperCpp::load` / `WhisperModel::Ggml` on
  Android (Whisper on Android is deferred; whisper.cpp's build and ABI are not verified there). The CoreML Whisper refusal
  off macOS now uses the same error (still an `Error::Config`; its text gains the `unsupported: ` prefix).
- Opt-in feature `xnnpack` (forwards `rightkit-onnx/xnnpack`) adds `ExecutionProvider::Xnnpack`: ORT's XNNPACK provider
  ahead of the CPU provider, session intra-op pool cut to one thread with spinning off, XNNPACK's pool sized from
  `intra_threads`. Falls back to plain CPU when it cannot register, like the other accelerators. Needs a runtime that
  contains XNNPACK; unmeasured. NNAPI is untouched and not claimed to work.
- Left as is, to verify on a device: `DEFAULT_EMIT_REWARD` on every non-Windows target is the macOS-tuned value, so Android
  decodes with the macOS beam reward; the `tokenizers` crate's `onig` C build under the NDK.
- Host-runnable unit tests for the pure rules (`EngineSupport`, the Android refusal, the error tag, the XNNPACK pool size).

## 0.2.1

- Republished on the renamed dependencies: `rightkit-ffmpeg` (was `rightkit-media`, dev), `rightkit-onnx` (was `rightkit-ort`). The 0.2.0 on crates.io was packaged before the renames and still named the old crates.
Additive on 0.2.0: existing defaults, signatures and behaviour for default options are unchanged.
Closes the behaviour ScrapeRight lost when it moved from spawning `whisper-cli` to this crate.
Not compiled or run when written.

### Audio

- `load_wav_mono_16k_head(path, max_duration: Duration) -> Result<Vec<f32>, WavLoadError>`: the first
  `max_duration` of a WAV file as mono 16 kHz `f32`, for language identification. Reading stops once the
  requested frames are in hand. Accepts 16, 24 and 32-bit integer PCM and 32-bit float, multi-channel
  averaged to mono. The crate has no resampler, so a rate other than 16 kHz is
  `WavLoadError::UnsupportedSampleRate`. `WavLoadError` (`Io`, `Malformed`, `UnsupportedFormat { float,
  bits_per_sample }`, `UnsupportedSampleRate`, `NoChannels`, `NonFiniteSample`) implements `Error` and
  converts into `Error::Audio`. `load_wav_mono_16k` is unchanged.

### Whisper

- Progress probe: `WhisperOptions::progress_probe: Option<ProgressProbe>` (`Whisper::set_progress_probe`).
  `ProgressProbe` is a cloneable, `Send + Sync` handle with `completed_windows()`, `total_windows()`,
  `samples_completed()`, `samples_total()` and `fraction()`, updated by the decode loop with atomics. It
  needs no callback, so a borrowed non-`Sync` host closure is not required. `ProgressCallback` is unchanged.
  Adding the field means struct literals of `WhisperOptions` need `..Default::default()`.
- Cancel: `WhisperOptions::cancel: Option<CancelToken>` (`Whisper::set_cancel`). Polled in whisper.cpp's abort
  callback (an in-flight window stops promptly) and between windows. A stopped run returns the new
  `Error::Interrupted(Box<WhisperInterrupted>)` with `reason: InterruptReason::Cancelled`;
  `Error::is_cancelled()` is true for it and for `Error::Cancelled`. The CoreML backend stops at window boundaries.
- Partial results: `WhisperInterrupted { reason, partial: WhisperTranscript, windows_completed, windows_estimated }`.
  Cancel and deadline both return the windows that finished instead of discarding them
  (before, a deadline abort was `Error::Model("whisper_full aborted after deadline ...")`).
- Progress: `WhisperOptions::progress: Option<ProgressCallback>` (`Whisper::set_progress`) reports `WhisperProgress`
  after each window and from whisper.cpp's own progress callback inside a window. `fraction` never decreases;
  the callback is never invoked after the call returns.
- Decode quality: `WhisperOptions::quality: DecodeQuality { beam_size, best_of, carry_context }`.
  Default stays beam 1, best-of 1, no context (greedy, as before). `DecodeQuality::whisper_cli()` is
  beam 5, best-of 5, context carried, with the whisper.cpp source lines it was read from in its docs.
  Values above 8 are rejected (whisper.cpp's decoder limit). The first window never carries context.
- Deadlines: `WhisperOptions::total_deadline` bounds the whole recording (default `None`) and also caps each
  window. `recording_deadline(audio)` is the old route's policy, 120 s plus 4x the audio, and `audio_duration(samples)`
  sizes it. `window_deadline` is now read from the options on every run, not only at load.
- Language detection: honors `cancel` and the new `detect_deadline`. whisper.cpp offers no abort hook for
  `whisper_pcm_to_mel` / `whisper_lang_auto_detect`, so both are checked before, between and after the native
  calls (worst case one mel plus one encoder pass).
- Text: `WhisperSegment::raw_text` and `WhisperTranscript::raw_text` keep whisper.cpp's untrimmed text. `text` joins
  segments without inserted spaces for `UNSPACED_LANGUAGES` (`ja`, `zh`, `yue`, `th`, `lo`, `km`); other
  languages are joined with one space as before. `join_segment_text` and `is_unspaced_language` are public.
- Audio loading: `load_audio` (and `WavSpec`) and the new `load_wav_mono_16k` are public.

Source-level notes: `Error` gains the `Interrupted` variant, and `WhisperOptions`, `WhisperSegment` and
`WhisperTranscript` gain fields, so exhaustive `match`es on `Error` and struct literals without
`..Default::default()` need an update.

## 0.2.0

Dependency: `rightkit-vad` is `0.1.0` (was `0.0.3`). 0.0.3 no longer resolves on main, so
`rightkit-asr` 0.2.0 requires `rightkit-vad` 0.1.0.

Dependency: `rightkit-transcript` is `0.3.0`. It adds `EndSource::SourceBoundary` and
`TranscriptError::MissingField`. `rightkit-asr` matches neither exhaustively (it only constructs
`EndSource` values), so no new arm was needed in this crate.

Additive on the 0.1.0 surface, with two behaviour notes below. Whisper's
`transcribe_samples` and `TranscriptionResult` are unchanged for default options.

### Whisper (whisper.cpp 1.9.3 and CoreML)

- `ort-defaults` (in `default`) no longer enables `rightkit-ort/tracing`; use the new opt-in `ort-tracing` feature to forward ONNX Runtime log lines into `tracing`. The forwarding is process-wide and verbose (thousands of INFO lines per model load).
- `Whisper::detect_language(&self, pcm16k) -> LanguageDetection { lang, probability, candidates }`:
  identification on the first 30 s with `whisper_pcm_to_mel` + `whisper_lang_auto_detect`, no text decode.
  whisper.cpp backend only; CoreML returns `Error::Config`.
- `WhisperOptions::detect_language: bool` (default `false`): identify on the first window, lock it for
  the call, and report `WhisperTranscript::detection`. Requires `Language::Auto`.
- `Whisper::transcribe_detailed(AudioSource) -> WhisperTranscript { text, language, detection, segments, words }`.
  `segments` and `words` carry `start_sample` / `end_sample` (`u64`), `sample_rate` (16000) and the `f32`
  seconds (segments) or `TimedWord` milliseconds (words).
- Word timing: `WhisperOptions::token_timestamps`, `max_len`, `split_on_word`. Words are assembled from
  token timestamps (see the field docs for whisper.cpp's caveats). Probability is the mean token probability.
- `DecodeMode::{Timed, TextOnly}` (default `Timed`). `TextOnly` sets `no_timestamps`, keeps `no_context`
  and runs greedy. Whisper.cpp only.
- Language lock: `Language::validated(&str)` rejects codes outside the table; `Language::parse` is unchanged.
- Public language table `WHISPER_LANGUAGES` (100 codes, including `yue`), `is_whisper_language`,
  `validate_language`. The table matches `whisper_lang_str(0..=99)` of the pinned library.
- `window_deadline` (existing since 0.1.0) is documented as caller-chosen. The default stays 180 s per window.
- `AudioSource::{Wav(PathBuf), Pcm16k(Vec<f32>)}`, consumed by `Whisper::transcribe_detailed`; callers decode
  other formats themselves. No ffmpeg dependency.
- `samples_to_ms(samples, sample_rate)` (integer arithmetic).
- FFI symbols added, each cited against whisper.h v1.9.3 in `src/whisper/cpp.rs`:
  `whisper_pcm_to_mel`, `whisper_lang_auto_detect`, `whisper_token_eot`, `whisper_full_n_tokens`,
  `whisper_full_get_token_text`, `whisper_full_get_token_id`, `whisper_full_get_token_t0`,
  `whisper_full_get_token_t1`, `whisper_full_get_token_p`. `whisper_full_get_token_data` is not bound
  (it returns a struct by value).
- CoreML route: `AudioEncoder.mlmodelc`, `TextDecoder.mlmodelc` and `tokenizer.json` are loaded as before.
  The one-time ANE prepare step is not performed by this crate.

### Long-form and Parakeet

- `LongFormOptions::word_assembly_floor_ms: Option<u32>` (default `Some(80)`, as before). `None` disables
  the floor. New `timed_pieces_to_words_with(pieces, floor_ms)` and `LongFormOptions::assemble_words`.
  `timed_pieces_to_words` is unchanged. **Breaking for struct literals** of `LongFormOptions`: add the field
  or use `..Default::default()`.
- `BeamOptions::frames_only_ends: bool` (default `false`): token ends come from the next token's frame
  instead of `start + duration * 0.08`.
- `BeamOptions::max_symbols` (default 10) and the greedy cap default (5, from `TransducerOptions::max_tokens_per_step`)
  are documented.
- `CoreMlParakeetOptions::strict: bool` (default `false`): at load, require each stage as
  `<Stage>.mlmodelc`; refuse the `.mlpackage` fallback and missing stages.

### Timing fix: TDT token end times (affects SRT / VTT and timing JSON)

- **Behaviour change.** `Transducer::transcribe_result` (and `transcribe`, `transcribe_with_timestamps`,
  and the TDT decoder path) previously set each token's end to the next token's start, discarding the
  predicted TDT duration. A token now ends at `(frame + predicted_duration) * 0.08 s`, clamped to the
  next token's start only when the duration would overlap it. Last tokens keep their duration; a
  zero-length last token gets 10 ms. RNN-T models (no duration head) keep the old next-start ends.
  Subtitle end times in SRT, VTT and timing JSON change accordingly. Predicted durations are the
  decoder's capped predictions (`TransducerOptions::duration_cap`, default 2 frames).
- The greedy path and the beam path with width 1 now share one span function
  (`decoder_tdt::token_span`), so they report identical start and end times for the same lattice
  (tested bit for bit).
- `BeamOptions::frames_only_ends` keeps its meaning (ends from frame indices, durations ignored).

### Tie-break rules (unchanged from 0.1.0, documented not fixed)

- Greedy path (`argmax`, `argmax_token` in `model_transducer`): `Iterator::max_by`, which returns the
  LAST maximal index on an exact float tie. Unchanged, so locked benchmarks do not move.
- Beam path (`rank_top_k` in `parakeet_core::tdt_beam`): the FIRST (lowest) index wins on an exact tie.
  Unchanged.
- The two rules therefore differ only on exact float ties. A width-1 beam and the greedy path can pick
  different symbols on such a tie; their timings match for the same lattice (see the timing fix above).

### Sample-exact pieces (`TimedPiece`), additive

- New `TimedPiece { token_id, text, frame: u32, start_sample: u64, end_sample: u64, sample_rate: u32 }`,
  exported at the crate root. `TimedToken` and every existing method's return value are **unchanged**.
- Pieces are computed from the decoder's frame indices BEFORE detokenisation, so they are not merged or
  re-spaced. Samples are integers: a frame is `8 * hop` samples (1280 at a 10 ms hop, 16 kHz). Ends follow
  the same rules as `TimedToken` (predicted durations, next-start clamp on overlap, zero-length last token
  gets 10 ms). Special tokens are skipped as in `TimedToken`.
- New methods: `Transducer::transcribe_result_with_pieces`, `Transducer::transcribe_window_beam_with_pieces`,
  `Transducer::transcribe_long_with_pieces` (ONNX), and `CoreMlParakeet::transcribe_window_with_pieces`,
  `transcribe_long_with_pieces` (CoreML).
- `frame` is relative to the decoded window. `start_sample` / `end_sample` on long-form output are absolute
  samples in the recording. Window offsets are accumulated as sample counts. Seconds are derived at the edge,
  so `start_sample as f32 / sample_rate as f32` equals the `TimedToken` start (tested).
- Pieces are committed as decoded, with no seam de-duplication. With `SeamPolicy::DedupOverlap` the text may
  drop a repeated word at a seam that the pieces keep. Use `SeamPolicy::Exact` if the two must agree.
- Also: the long-form driver gains `transcribe_padded_window_with_pieces` and `PaddedWindowCursor::step_with_pieces`.
  `transcribe_padded_window` and `PaddedWindowCursor::step` keep their signatures.
- `TimedPiece` also carries `dur_frames: u32` (the model's own duration, unclamped; 0 for a model with no
  duration head), `frame_samples: u32` (samples per encoder frame, `8 * hop`, not a constant) and
  `TimedPiece::raw_end_sample() = start_sample + max(dur_frames, 1) * frame_samples`, the unclamped end that
  CutRight's pre-port assembly used (it can exceed the next piece's start). `start_sample` / `end_sample`
  keep their clamped values and meaning.
- One piece per token: `pieces.len() == result.tokens.len()`, same order, `pieces[i].text == result.tokens[i].text`
  (guarded digit boundary, special-token skip and, where the policy asks, N0 filler stripping now apply to
  pieces too; before, `text` was the raw piece with `▁` as a space, so "like" + "1" could weld). New
  `token_index: u32` (position in the token list) and `raw: String` (the vocabulary piece as stored, `▁` kept).
  A vocabulary id with no text is an empty piece (`text` and `raw` empty) on the beam and CoreML paths, which
  keep such an id as an empty token; the greedy ONNX decoder drops it from both outputs. In long-form output
  `token_index` is offset by the pieces of earlier windows (equal to the result token index under
  `SeamPolicy::Exact`).
- Source break for code that builds `TimedPiece { .. }` by struct literal: the new fields are required
  (0.2.0 is unpublished).
- `LongFormProgress::windows_estimated_raw`: the estimate before it is raised to `windows_done`, so
  `done / (estimated + 1)` can be reproduced. `windows_estimated` is unchanged.

### Parakeet long-form, strict layout, feature gating

- `longform::pad_terminal_silence(segment, opts)` is public. `LongFormOptions::pad_policy: PadPolicy`
  (`FinalSegmentOnly`, default and the 0.1.0 behaviour; or `EveryWindow`, which pads every window
  that ends in speech, leaving offsets and word times unchanged).
- Window offsets are integer sample counts in the long-form driver. Seconds are derived once at the
  edge with `longform::samples_to_secs` (f64 division, rounded to f32). Cancellation (`AtomicBool`) and
  progress (`samples_done`, `samples_total`) already counted samples and are unchanged.
- `Error::WindowTooLong { samples, max_samples }`: a single-window decode above the model window plus one
  mel hop now returns this typed error (it was `Error::Audio`). **Breaking for exhaustive matches** on
  `Error`. The limit is exposed as `Transducer::max_window_samples()`.
- `StrictLayout { require_mel_nemo, allow_int8, require_static_window }` with `StrictLayout::PRODUCTION`
  (mel required, fp32 only, static encoder). `Transducer::load_strict(dir, layout)` and
  `load_strict_with_options(dir, config, options, layout)` refuse: a missing `mel_nemo.onnx` (no Rust-mel
  fallback), any int8 encoder or decoder when `allow_int8` is false (an int8-only directory is an error),
  and a dynamic encoder when `require_static_window` is set. `Transducer::static_window()` is public.
  `load_with_options` is unchanged.
- `vocab.txt` ids and token lines tolerate CR/LF (Windows-edited files).
- Cargo: `tokenizers` is optional behind feature `whisper-tokenizers` (default on, so default builds are
  unchanged). It gates the Whisper CoreML decoder, the CTC `ParakeetDecoder` (now in `decoder_ctc`),
  `Parakeet` (CTC), `ParakeetEOU` and `cohere`. Parakeet CoreML and ONNX TDT consumers can build with
  `default-features = false`. `decoder.rs` keeps `TimedToken` and `TranscriptionResult`.
  Verify the no-feature build on the target before relying on it.

### TDT decoder input checks and the last-token minimum

- Behaviour change, input checks. The TDT decoder (`ParakeetTDT` and the `Transducer` TDT path) now
  returns an error instead of panicking or guessing. `Error::Model` when `frame_indices` does not have one
  entry per token (it panicked with index out of bounds), or when `durations` is neither empty (RNN-T) nor
  one entry per token (it was silently treated as absent). `Error::Config` when `hop_length` or
  `sample_rate` is zero. Well-formed lattices decode as before. No internal caller passes a mismatched lattice.
- New public constant `rightkit_asr::LAST_TOKEN_MIN_DURATION_MS` (`10`). A zero-length last token is widened
  to this many milliseconds, so it is never a zero-length span that cannot be shown as a cue or highlighted.
  The value is the one already in use, now named and documented. A consumer that kept `start == end` for a
  zero-length last token will see this width instead.

### Not in 0.2.0

- Sample-exact fields on `TimedToken` itself. `TimedToken` is unchanged; see `TimedPiece` above for the
  sample-carrying output.

- Sample-accurate fields on Parakeet outputs: `TimedWord` is owned by `rightkit-transcript`, which this
  release does not change. Whisper outputs carry the sample fields instead.
- Text-only and token-timestamp decode on the CoreML backend: they return `Error::Config`.

## 0.1.0

- Breaking: `TimedWord` (`longform::TimedWord`, re-exported at the crate root) is now
  `rightkit_transcript::TimedWord`: millisecond `start_ms` / `end_ms` (`u64`) in place
  of `start` / `end` (`f32` seconds). Use `start_secs()` / `end_secs()` for seconds.
- `longform` SRT / VTT / timing-JSON rendering and cue grouping now call
  `rightkit-transcript` (`group_words`, `to_srt`, `to_vtt`, `to_timing_json`).
  `group_cues`, `render_subtitles`, `render_timing_json`, `timed_exports` keep their
  signatures. SRT/VTT output is byte-identical (golden test `longform_exports`);
  timing-JSON seconds are now the exact millisecond value (`1.52`, was `1.5199999809265137`).
  VTT text now escapes `&`, `<`, `>` and cue text rewrites `-->` to `->` (transcript export rules).
- Added a `rightkit-transcript` dependency. The engine-backed round trip that lived in
  `rightkit-transcript` moved here (`tests/transcript_roundtrip.rs`).

## 0.0.2

First crates.io release line. Ships the Parakeet ONNX decoder, the Whisper
decoder (whisper.cpp, and CoreML on macOS), and the decoding primitives, built
on `rightkit-ort` and `rightkit-vad`. The crate is a derivative of `parakeet-rs`
(MIT); see `PATCHES.md`, `UPSTREAM.toml`, and `THIRD_PARTY_NOTICES.md`. Model
artifacts are not packaged.

Earlier history is in the git log of this repository; this file starts at the
publication point.
