//! Keyword-spotting parity of `rightkit_asr::kws` with Sherpa-ONNX 1.13.4.
//!
//! The reference is `tests/fixtures/kws/reference.json` (schema `rightkit.kws-reference.v1`):
//! unpatched upstream Sherpa from the Linux GCC build, replayed here through the public API
//! with the same feeding protocol. Each case's audio is its segments (WAV files or digital
//! silence) plus `tail_silence_samples` of silence, fed in `feed_samples` chunks; after every
//! accept the stream is decoded while ready and every non-empty result is recorded with the
//! 0-based index of the decode that produced it.
//!
//! The exact test feeds Sherpa's own filterbank frames for the case (`fbank`), injected as
//! the waveform protocol would have produced them, so the encoder, beam search and keyword
//! graph are compared bit for bit. Sherpa's GCC build computes its FFT with `-ffast-math`,
//! so a filterbank computed here from the waveform differs in the last ulps; that can flip
//! which of two near-tied merged beam paths keeps its timestamps. The waveform test therefore
//! requires the same decodes, detections and last-token time, and reports first-token drift.
//!
//! Gates (a missing asset prints a SKIP line and the test passes; `RIGHTKIT_REQUIRE_ASSETS=1`
//! makes it fail):
//! - `RK_ORT_TEST_DYLIB`: the ONNX Runtime library (the reference ran ORT 1.27.0).
//! - `RK_KWS_MODEL_DIR`: the sherpa-onnx-kws-zipformer-zh-en-3M chunk-16 model, holding the
//!   encoder, decoder and joiner named in `reference.json` and `tokens.txt`. Their SHA-256
//!   must match the reference; a mismatch fails.
//! - `RK_KWS_FIXTURES` (optional): a fixtures root to use instead of `tests/fixtures`.
#[macro_use]
mod common;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use rightkit_asr::kws::{
    JoinerBatching, KeywordDetection, KeywordSpotter, KeywordSpotterConfig, KeywordStream,
    Keywords, KwsModel, MergeTimestamps, OnsetConfig, ThresholdTokens,
};
use rightkit_asr::Error;
use serde::Deserialize;

const SCHEMA: &str = "rightkit.kws-reference.v1";
/// Encoder frames per decode for the chunk-16 model (32 feature frames, subsampling 4).
const FRAMES_PER_DECODE: usize = 8;
/// HeardRight's active-path expiry (38 frames, 1.52 s).
const PRODUCTION_TTL_FRAMES: i32 = 38;
/// Encoder frame length in seconds (10 ms feature shift, subsampling 4).
const FRAME_SECONDS: f64 = 0.04;

#[derive(Debug, Deserialize)]
struct Reference {
    schema: String,
    #[serde(default)]
    reference: serde_json::Value,
    model: ModelRef,
    audio: BTreeMap<String, AudioRef>,
    keyword_sets: BTreeMap<String, String>,
    #[serde(default)]
    encoder: Vec<EncoderRef>,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct ModelRef {
    encoder: String,
    encoder_sha256: String,
    decoder: String,
    decoder_sha256: String,
    joiner: String,
    joiner_sha256: String,
    tokens_sha256: String,
}

#[derive(Debug, Deserialize)]
struct AudioRef {
    path: String,
    samples: usize,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct EncoderRef {
    frames_per_chunk: usize,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Segment {
    Audio(String),
    Silence { silence_samples: usize },
}

#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    segments: Vec<Segment>,
    keyword_set: String,
    max_active_paths: usize,
    num_trailing_blanks: i32,
    keywords_score: f32,
    keywords_threshold: f32,
    feed_samples: usize,
    tail_silence_samples: usize,
    decodes: usize,
    detections: Vec<RefDetection>,
    /// Sherpa's filterbank frames for the whole fed audio, relative to `kws/`.
    #[serde(default)]
    fbank: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RefDetection {
    decode: usize,
    keyword: String,
    tokens: Vec<String>,
    /// C++ `float` values written as JSON doubles.
    timestamps: Vec<f64>,
}

/// One non-empty result from the Rust spotter.
#[derive(Debug, Clone, PartialEq)]
struct Hit {
    decode: usize,
    keyword: String,
    tokens: Vec<String>,
    timestamps: Vec<f32>,
    frames: Vec<i32>,
}

struct Harness {
    fixtures: PathBuf,
    model_dir: PathBuf,
    reference: Reference,
}

struct Missing {
    what: &'static str,
    looked_at: String,
}

/// `harness!()`: the loaded fixtures and model, or a SKIP (panic under
/// `RIGHTKIT_REQUIRE_ASSETS=1`) and a return from the test.
macro_rules! harness {
    () => {
        match Harness::load() {
            Ok(h) => h,
            Err(missing) => {
                common::skip_missing(missing.what, &missing.looked_at);
                return;
            }
        }
    };
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

/// Bind and initialise the ONNX Runtime named by `RK_ORT_TEST_DYLIB` once per test binary.
fn bind_runtime(runtime: &Path) {
    static BOUND: OnceLock<Result<String, String>> = OnceLock::new();
    let outcome = BOUND.get_or_init(|| {
        let selection = rightkit_onnx::configure_runtime([rightkit_onnx::explicit_file_candidate(
            runtime,
        )])
        .map_err(|e| format!("select {}: {e}", runtime.display()))?;
        rightkit_onnx::init_environment(&selection, &rightkit_onnx::EnvironmentOptions::default())
            .map(|report| report.runtime_info)
    });
    if let Err(e) = outcome {
        panic!("bind the ONNX Runtime test library: {e}");
    }
}

fn check_sha256(path: &Path, expected: &str) {
    let got = rightkit_ffmpeg::sha256_file(path)
        .unwrap_or_else(|e| panic!("hash {}: {e}", path.display()));
    assert!(
        got.eq_ignore_ascii_case(expected),
        "{} has sha256 {got}; reference.json expects {expected}",
        path.display()
    );
}

impl Harness {
    fn load() -> Result<Self, Missing> {
        let fixtures = env_path("RK_KWS_FIXTURES")
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"));
        let reference_path = fixtures.join("kws/reference.json");
        if !reference_path.is_file() {
            return Err(Missing {
                what: "KWS Sherpa reference fixture",
                looked_at: reference_path.display().to_string(),
            });
        }
        let model_dir = env_path("RK_KWS_MODEL_DIR").ok_or(Missing {
            what: "KWS zipformer model directory",
            looked_at: "RK_KWS_MODEL_DIR (encoder, decoder, joiner named in reference.json, tokens.txt)"
                .into(),
        })?;
        let runtime = env_path("RK_ORT_TEST_DYLIB").ok_or(Missing {
            what: "ONNX Runtime dylib",
            looked_at: "RK_ORT_TEST_DYLIB (absolute path to the native ONNX Runtime library)".into(),
        })?;
        bind_runtime(&runtime);

        let text = std::fs::read_to_string(&reference_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", reference_path.display()));
        let reference: Reference = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("parse {}: {e}", reference_path.display()));
        assert_eq!(reference.schema, SCHEMA, "{}", reference_path.display());
        let m = &reference.model;
        for (file, sha) in [
            (&m.encoder, &m.encoder_sha256),
            (&m.decoder, &m.decoder_sha256),
            (&m.joiner, &m.joiner_sha256),
        ] {
            check_sha256(&model_dir.join(file), sha);
        }
        check_sha256(&model_dir.join("tokens.txt"), &m.tokens_sha256);
        for audio in reference.audio.values() {
            check_sha256(&fixtures.join(&audio.path), &audio.sha256);
        }
        if let Some(fpc) = reference.encoder.first().map(|e| e.frames_per_chunk) {
            assert_eq!(fpc, FRAMES_PER_DECODE, "encoder frames per chunk in the reference");
        }
        Ok(Self {
            fixtures,
            model_dir,
            reference,
        })
    }

    fn wav(&self, name: &str) -> Vec<f32> {
        let entry = self
            .reference
            .audio
            .get(name)
            .unwrap_or_else(|| panic!("segment `{name}` is not in the reference audio map"));
        let path = self.fixtures.join(&entry.path);
        let mut reader = hound::WavReader::open(&path)
            .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        let spec = reader.spec();
        assert_eq!(
            (spec.sample_rate, spec.channels, spec.bits_per_sample),
            (16_000, 1, 16),
            "{} must be 16 kHz mono int16",
            path.display()
        );
        let samples: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| s.expect("wav sample") as f32 / 32768.0)
            .collect();
        assert_eq!(samples.len(), entry.samples, "{} sample count", path.display());
        samples
    }

    /// The case's segments followed by its tail silence.
    fn case_audio(&self, case: &Case) -> Vec<f32> {
        let mut audio = Vec::new();
        for segment in &case.segments {
            match segment {
                Segment::Audio(name) => audio.extend(self.wav(name)),
                Segment::Silence { silence_samples } => {
                    audio.resize(audio.len() + *silence_samples, 0.0)
                }
            }
        }
        audio.resize(audio.len() + case.tail_silence_samples, 0.0);
        audio
    }

    /// Sherpa's filterbank frames for the case's whole fed audio, when the reference has them.
    fn case_fbank(&self, case: &Case) -> Option<Vec<f32>> {
        let rel = case.fbank.as_ref()?;
        let path = self.fixtures.join("kws").join(rel);
        // The published crate leaves the fbank dumps out; those cases run waveform-only.
        if !path.is_file() {
            return None;
        }
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("case {}: read {}: {e}", case.id, path.display()));
        let (values, rest) = bytes.as_chunks::<4>();
        assert!(rest.is_empty(), "{} is not f32le", path.display());
        Some(values.iter().map(|b| f32::from_le_bytes(*b)).collect())
    }

    fn keywords(&self, case: &Case) -> String {
        self.reference
            .keyword_sets
            .get(&case.keyword_set)
            .unwrap_or_else(|| panic!("case {}: keyword set `{}` missing", case.id, case.keyword_set))
            .clone()
    }

    fn base_config(&self, keywords: String) -> KeywordSpotterConfig {
        let m = &self.reference.model;
        KeywordSpotterConfig::new(
            self.model_dir.join(&m.encoder),
            self.model_dir.join(&m.decoder),
            self.model_dir.join(&m.joiner),
            self.model_dir.join("tokens.txt"),
            Keywords::Text(keywords),
        )
    }

    /// Unpatched upstream Sherpa as built by GCC on Linux, with the case's decoder settings.
    fn parity_config(&self, case: &Case) -> KeywordSpotterConfig {
        let mut config = self.base_config(self.keywords(case));
        config.threads = 1;
        config.max_active_paths = case.max_active_paths;
        config.num_trailing_blanks = case.num_trailing_blanks;
        config.keywords_score = case.keywords_score;
        config.keywords_threshold = case.keywords_threshold;
        config.active_path_ttl_frames = None;
        config.trailing_blank_reset = true;
        config.softmax_f64 = true;
        config
    }

    /// HeardRight's shipped decoder policy on the case's keyword set.
    fn production_config(&self, case: &Case) -> KeywordSpotterConfig {
        let mut config = self.base_config(self.keywords(case));
        config.threads = 1;
        config.max_active_paths = 8;
        config.num_trailing_blanks = 0;
        config.keywords_score = case.keywords_score;
        config.keywords_threshold = case.keywords_threshold;
        config.active_path_ttl_frames = Some(PRODUCTION_TTL_FRAMES);
        config.trailing_blank_reset = false;
        config.softmax_f64 = false;
        config
    }
}

/// Feature frames a 16 kHz stream holds after `samples` samples without a flush
/// (25 ms / 10 ms frames, `snip_edges = false`).
fn ready_frames(samples: usize) -> usize {
    if samples < 280 {
        0
    } else {
        (samples - 280) / 160 + 1
    }
}

/// What the stream is fed.
#[derive(Clone, Copy)]
enum Input<'a> {
    Wave(&'a [f32]),
    /// Precomputed frames for `samples` of audio, injected after each feed chunk up to the
    /// frames that chunk would have made ready.
    Features { frames: &'a [f32], samples: usize },
}

/// Feed `input` in `feed`-sample chunks and record every non-empty result. Returns the
/// number of decodes and the hits.
fn run(config: &KeywordSpotterConfig, input: Input<'_>, feed: usize) -> (usize, Vec<Hit>) {
    let dim = config.features.num_bins;
    let mut spotter = KeywordSpotter::new(config).expect("create keyword spotter");
    let mut stream = spotter.create_stream();
    let mut decodes = 0usize;
    let mut hits = Vec::new();
    let total = match input {
        Input::Wave(audio) => audio.len(),
        Input::Features { frames, samples } => {
            assert_eq!(
                frames.len(),
                ready_frames(samples) * dim,
                "injected fbank must hold the frames {samples} samples make ready"
            );
            samples
        }
    };
    let (mut fed, mut injected) = (0usize, 0usize);
    while fed < total {
        let n = feed.max(1).min(total - fed);
        match input {
            Input::Wave(audio) => stream
                .accept_waveform(16_000, &audio[fed..fed + n])
                .expect("accept 16 kHz audio"),
            Input::Features { frames, .. } => {
                let ready = ready_frames(fed + n);
                stream.accept_features(&frames[injected * dim..ready * dim]);
                injected = ready;
            }
        }
        fed += n;
        while spotter.is_ready(&stream) {
            spotter.decode(&mut stream).expect("decode");
            let r = spotter.result(&mut stream);
            if !r.keyword.is_empty() {
                hits.push(Hit {
                    decode: decodes,
                    keyword: r.keyword,
                    tokens: r.tokens,
                    timestamps: r.timestamps,
                    frames: r.frames,
                });
            }
            decodes += 1;
        }
    }
    (decodes, hits)
}

/// [`run`] through [`KeywordSpotter::detection`], keeping each detection's details.
fn run_detections(
    config: &KeywordSpotterConfig,
    input: Input<'_>,
    feed: usize,
) -> (usize, Vec<(usize, KeywordDetection)>) {
    let dim = config.features.num_bins;
    let mut spotter = KeywordSpotter::new(config).expect("create keyword spotter");
    let mut stream = spotter.create_stream();
    let (mut decodes, mut found) = (0usize, Vec::new());
    let total = match input {
        Input::Wave(audio) => audio.len(),
        Input::Features { samples, .. } => samples,
    };
    let (mut fed, mut injected) = (0usize, 0usize);
    while fed < total {
        let n = feed.max(1).min(total - fed);
        match input {
            Input::Wave(audio) => stream
                .accept_waveform(16_000, &audio[fed..fed + n])
                .expect("accept 16 kHz audio"),
            Input::Features { frames, .. } => {
                let ready = ready_frames(fed + n);
                stream.accept_features(&frames[injected * dim..ready * dim]);
                injected = ready;
            }
        }
        fed += n;
        while spotter.is_ready(&stream) {
            spotter.decode(&mut stream).expect("decode");
            if let Some(d) = spotter.detection(&mut stream) {
                found.push((decodes, d));
            }
            decodes += 1;
        }
    }
    (decodes, found)
}

fn hits_of(found: &[(usize, KeywordDetection)]) -> Vec<Hit> {
    found
        .iter()
        .map(|(decode, d)| Hit {
            decode: *decode,
            keyword: d.result.keyword.clone(),
            tokens: d.result.tokens.clone(),
            timestamps: d.result.timestamps.clone(),
            frames: d.result.frames.clone(),
        })
        .collect()
}

fn same_detection(want: &RefDetection, got: &Hit) -> bool {
    want.decode == got.decode
        && want.keyword == got.keyword
        && want.tokens == got.tokens
        && want.timestamps.len() == got.timestamps.len()
        && want
            .timestamps
            .iter()
            .zip(&got.timestamps)
            .all(|(w, g)| (*w as f32) == *g)
}

fn show_ref(d: &RefDetection) -> String {
    let ts: Vec<f32> = d.timestamps.iter().map(|t| *t as f32).collect();
    format!(
        "decode {} {} tokens {:?} timestamps {:?}",
        d.decode, d.keyword, d.tokens, ts
    )
}

fn show_hit(h: &Hit) -> String {
    format!(
        "decode {} {} tokens {:?} timestamps {:?} (frames {:?})",
        h.decode, h.keyword, h.tokens, h.timestamps, h.frames
    )
}

fn summary(hits: &[Hit]) -> String {
    let parts: Vec<String> = hits
        .iter()
        .map(|h| format!("d{} {}@{:?}", h.decode, h.keyword, h.timestamps.first()))
        .collect();
    format!("[{}]", parts.join(", "))
}

/// `None` when the run matches the reference exactly, else the first divergence.
fn compare(case: &Case, decodes: usize, hits: &[Hit]) -> Option<String> {
    let mut diff = String::new();
    let first = (0..case.detections.len().max(hits.len())).find(|&i| {
        match (case.detections.get(i), hits.get(i)) {
            (Some(w), Some(g)) => !same_detection(w, g),
            _ => true,
        }
    });
    if let Some(i) = first {
        let want = case.detections.get(i);
        let got = hits.get(i);
        let at = [want.map(|d| d.decode), got.map(|h| h.decode)]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(0);
        let _ = write!(
            diff,
            "first divergence at decode {at} (detection #{i}): expected {}; got {}",
            want.map_or("nothing".into(), show_ref),
            got.map_or("nothing".into(), show_hit),
        );
    }
    if decodes != case.decodes {
        if !diff.is_empty() {
            diff.push_str("; ");
        }
        let _ = write!(diff, "decodes expected {} got {decodes}", case.decodes);
    }
    (!diff.is_empty()).then_some(diff)
}

/// Sherpa's frame index for a timestamp it reported in seconds.
fn ref_frame(seconds: f64) -> i64 {
    (seconds / FRAME_SECONDS).round() as i64
}

/// Exact parity on Sherpa's own filterbank: every decode count, detection decode index,
/// keyword, token and timestamp must match.
#[test]
fn kws_detections_match_sherpa_reference() {
    let h = harness!();
    eprintln!("reference: {}", h.reference.reference);
    let mut failures = Vec::new();
    let mut without_fbank = Vec::new();
    for case in &h.reference.cases {
        let Some(frames) = h.case_fbank(case) else {
            without_fbank.push(case.id.clone());
            continue;
        };
        let samples = h.case_audio(case).len();
        let input = Input::Features {
            frames: &frames,
            samples,
        };
        let (decodes, hits) = run(&h.parity_config(case), input, case.feed_samples);
        match compare(case, decodes, &hits) {
            None => eprintln!(
                "PASS {}: decodes={decodes} detections={} {}",
                case.id,
                hits.len(),
                summary(&hits)
            ),
            Some(diff) => {
                eprintln!("FAIL {}: {diff}", case.id);
                failures.push(case.id.clone());
            }
        }
    }
    let checked = h.reference.cases.len() - without_fbank.len();
    if !without_fbank.is_empty() {
        eprintln!(
            "no Sherpa fbank, left to the waveform test: {} cases {without_fbank:?}",
            without_fbank.len()
        );
    }
    if checked == 0 {
        common::skip_missing(
            "KWS Sherpa fbank dumps",
            &h.fixtures.join("kws/fbank").display().to_string(),
        );
        return;
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} injected-fbank cases diverge from the Sherpa reference: {failures:?}",
        failures.len(),
    );
    eprintln!("{checked} cases match the Sherpa reference exactly");
}

/// Onset estimation on: every case still matches the Sherpa reference exactly (estimation
/// never changes detection), every detection carries candidates on the beam's clock, and
/// the spread of each candidate against the emission frame is reported.
#[test]
fn kws_onset_estimation_leaves_detections_unchanged() {
    let h = harness!();
    let mut failures = Vec::new();
    let (mut detections, mut checked) = (0usize, 0usize);
    // (candidate, count, sum of feature-frame deltas from emission, min, max)
    let mut spread: BTreeMap<&str, (usize, i64, i64, i64)> = BTreeMap::new();
    let mut rules: BTreeMap<&str, usize> = BTreeMap::new();
    for case in &h.reference.cases {
        let Some(frames) = h.case_fbank(case) else {
            continue;
        };
        checked += 1;
        let samples = h.case_audio(case).len();
        let mut config = h.parity_config(case);
        config.onset = Some(OnsetConfig::default());
        let input = Input::Features {
            frames: &frames,
            samples,
        };
        let (decodes, found) = run_detections(&config, input, case.feed_samples);
        if let Some(diff) = compare(case, decodes, &hits_of(&found)) {
            eprintln!("FAIL {}: {diff}", case.id);
            failures.push(case.id.clone());
        }
        for (_, d) in &found {
            detections += 1;
            let onset = d.onset.as_ref().unwrap_or_else(|| {
                panic!(
                    "case {}: {} has no onset estimates",
                    case.id, d.result.keyword
                )
            });
            let first = d.result.frames[0];
            let last = *d.result.frames.last().unwrap();
            assert_eq!(onset.emission.feature_frame, 4 * first, "case {}", case.id);
            assert!(
                d.fired_frame > last + config.num_trailing_blanks,
                "case {}",
                case.id
            );
            assert_eq!(
                d.token_probs.len(),
                d.result.tokens.len(),
                "case {}",
                case.id
            );
            assert!(
                d.score >= d.threshold,
                "case {}: score below threshold",
                case.id
            );
            if let Some(aligned) = onset.aligned {
                assert_eq!(onset.aligned_frames.len(), d.result.tokens.len());
                assert!(onset.aligned_frames.windows(2).all(|w| w[0] < w[1]));
                assert_eq!(aligned.feature_frame, 4 * onset.aligned_frames[0]);
                assert!(
                    *onset.aligned_frames.last().unwrap() <= d.fired_frame,
                    "case {}: alignment ends after the firing frame",
                    case.id
                );
            }
            let anchor = onset.aligned.unwrap_or(onset.emission).feature_frame;
            if let Some(p) = onset.posterior {
                assert!(
                    p.feature_frame <= anchor,
                    "case {}: posterior after its anchor",
                    case.id
                );
            }
            if let Some(a) = onset.acoustic {
                assert!(
                    a.feature_frame >= 0 && a.feature_frame <= anchor + 10,
                    "case {}: acoustic {} outside [0, anchor {anchor} + 10]",
                    case.id,
                    a.feature_frame
                );
            }
            for (name, estimate) in [
                ("aligned", onset.aligned),
                ("posterior", onset.posterior),
                ("acoustic", onset.acoustic),
            ] {
                let Some(e) = estimate else { continue };
                let delta = (e.feature_frame - onset.emission.feature_frame) as i64;
                let entry = spread.entry(name).or_insert((0, 0, i64::MAX, i64::MIN));
                entry.0 += 1;
                entry.1 += delta;
                entry.2 = entry.2.min(delta);
                entry.3 = entry.3.max(delta);
            }
            if let Some(rule) = onset.acoustic_rule {
                *rules.entry(rule.as_str()).or_default() += 1;
            }
        }
    }
    if checked == 0 {
        common::skip_missing(
            "KWS Sherpa fbank dumps",
            &h.fixtures.join("kws/fbank").display().to_string(),
        );
        return;
    }
    for (name, (n, sum, min, max)) in &spread {
        eprintln!(
            "onset {name}: {n}/{detections} detections, feature frames vs emission mean {:+.1} min {min:+} max {max:+}",
            *sum as f64 / (*n).max(1) as f64
        );
    }
    eprintln!("acoustic rules: {rules:?}");
    assert!(
        failures.is_empty(),
        "{} of {checked} cases changed with onset estimation on: {failures:?}",
        failures.len()
    );
    assert!(detections > 0, "no detections to estimate");
    eprintln!(
        "{checked} cases unchanged with onset estimation on; {detections} detections estimated"
    );
}

/// The accuracy levers. A one-path beam batches one hypothesis either way, so
/// `PerHypothesis` must reproduce `Beam` exactly there. Elsewhere each lever's effect is
/// reported against the same spotter without it (on the Rust filterbank): detections that
/// appear or vanish, and matched detections whose first-token frame moves. Departures are
/// expected, not failures.
#[test]
fn kws_levers_report_and_hold_their_invariants() {
    let h = harness!();
    let cases: Vec<&Case> = h
        .reference
        .cases
        .iter()
        .filter(|c| c.feed_samples == 320 || c.id.starts_with("hr_"))
        .take(12)
        .collect();
    type Lever = (&'static str, fn(&mut KeywordSpotterConfig));
    let levers: [Lever; 4] = [
        ("matched_keyword", |c| {
            c.threshold_tokens = ThresholdTokens::MatchedKeyword
        }),
        ("per_hypothesis", |c| {
            c.joiner_batching = JoinerBatching::PerHypothesis
        }),
        ("portable_math", |c| c.portable_math = true),
        ("merge_likelier", |c| {
            c.merge_timestamps = MergeTimestamps::Likelier
        }),
    ];
    // Per lever: (detections without it, with it, matched, first-token frame moved).
    let mut totals = [(0usize, 0usize, 0usize, 0usize); 4];
    for case in &cases {
        let audio = h.case_audio(case);
        let mut one_path = h.parity_config(case);
        one_path.max_active_paths = 1;
        let beam = run(&one_path, Input::Wave(&audio), case.feed_samples);
        one_path.joiner_batching = JoinerBatching::PerHypothesis;
        let per_hyp = run(&one_path, Input::Wave(&audio), case.feed_samples);
        assert_eq!(
            per_hyp, beam,
            "case {}: one-path beam differs by batching",
            case.id
        );
        let (base_decodes, base) = run(&h.parity_config(case), Input::Wave(&audio), case.feed_samples);
        for ((lever, set), total) in levers.iter().zip(totals.iter_mut()) {
            let mut config = h.parity_config(case);
            set(&mut config);
            let (decodes, hits) = run(&config, Input::Wave(&audio), case.feed_samples);
            assert_eq!(decodes, base_decodes, "case {}: {lever} changed the decode count", case.id);
            let matched: Vec<(&Hit, &Hit)> = hits
                .iter()
                .filter_map(|hit| {
                    base.iter()
                        .find(|b| (b.decode, &b.keyword) == (hit.decode, &hit.keyword))
                        .map(|b| (b, hit))
                })
                .collect();
            let moved = matched
                .iter()
                .filter(|(b, hit)| b.frames.first() != hit.frames.first())
                .count();
            *total = (
                total.0 + base.len(),
                total.1 + hits.len(),
                total.2 + matched.len(),
                total.3 + moved,
            );
            eprintln!(
                "LEVER {lever} {}: {} detections ({} without it), {} matched, first-token frame moved on {moved}",
                case.id,
                hits.len(),
                base.len(),
                matched.len()
            );
        }
    }
    for ((lever, _), (without, with, matched, moved)) in levers.iter().zip(totals) {
        eprintln!(
            "LEVER {lever}: {with} detections vs {without} without it; {matched} matched, first-token frame moved on {moved}"
        );
    }
}

/// One spotter fed step by step, so several can interleave their decodes on one shared model.
struct Stepper<'a> {
    spotter: KeywordSpotter,
    stream: KeywordStream,
    input: Input<'a>,
    feed: usize,
    dim: usize,
    total: usize,
    fed: usize,
    injected: usize,
    decodes: usize,
    hits: Vec<Hit>,
}

impl<'a> Stepper<'a> {
    fn new(spotter: KeywordSpotter, dim: usize, input: Input<'a>, feed: usize) -> Self {
        let total = match input {
            Input::Wave(audio) => audio.len(),
            Input::Features { samples, .. } => samples,
        };
        Self {
            stream: spotter.create_stream(),
            spotter,
            input,
            feed,
            dim,
            total,
            fed: 0,
            injected: 0,
            decodes: 0,
            hits: Vec::new(),
        }
    }

    /// Feed one chunk and decode while ready, as [`run`] does; false once all is fed.
    fn step(&mut self) -> bool {
        if self.fed >= self.total {
            return false;
        }
        let n = self.feed.max(1).min(self.total - self.fed);
        match self.input {
            Input::Wave(audio) => self
                .stream
                .accept_waveform(16_000, &audio[self.fed..self.fed + n])
                .expect("accept 16 kHz audio"),
            Input::Features { frames, .. } => {
                let ready = ready_frames(self.fed + n);
                self.stream
                    .accept_features(&frames[self.injected * self.dim..ready * self.dim]);
                self.injected = ready;
            }
        }
        self.fed += n;
        while self.spotter.is_ready(&self.stream) {
            self.spotter.decode(&mut self.stream).expect("decode");
            let r = self.spotter.result(&mut self.stream);
            if !r.keyword.is_empty() {
                self.hits.push(Hit {
                    decode: self.decodes,
                    keyword: r.keyword,
                    tokens: r.tokens,
                    timestamps: r.timestamps,
                    frames: r.frames,
                });
            }
            self.decodes += 1;
        }
        true
    }
}

/// The speed levers change no bit: every injected-fbank case matches the Sherpa reference
/// exactly with `static_batch` and `decoder_cache` on, first on a spotter of its own, then
/// on ONE shared [`KwsModel`] while a second spotter with another case's keyword set decodes
/// interleaved with it (one decoder cache serving both). Also: a config naming another model
/// than the shared one is refused.
#[test]
fn kws_speed_levers_and_a_shared_model_match_sherpa_reference() {
    let h = harness!();
    let with_levers = |case: &Case| {
        let mut config = h.parity_config(case);
        config.static_batch = true;
        config.decoder_cache = true;
        config
    };
    let cases: Vec<(&Case, Vec<f32>, usize)> = h
        .reference
        .cases
        .iter()
        .filter_map(|case| {
            let frames = h.case_fbank(case)?;
            Some((case, frames, h.case_audio(case).len()))
        })
        .collect();
    if cases.is_empty() {
        common::skip_missing(
            "KWS Sherpa fbank dumps",
            &h.fixtures.join("kws/fbank").display().to_string(),
        );
        return;
    }
    let mut failures = Vec::new();
    for (case, frames, samples) in &cases {
        let input = Input::Features {
            frames,
            samples: *samples,
        };
        let (decodes, hits) = run(&with_levers(case), input, case.feed_samples);
        if let Some(diff) = compare(case, decodes, &hits) {
            eprintln!("FAIL levers {}: {diff}", case.id);
            failures.push(format!("levers {}", case.id));
        }
    }

    let shared = KwsModel::load(&with_levers(cases[0].0)).expect("load shared model");
    let mut distinct_sets = 0usize;
    for pair in cases.chunks(2) {
        let mut steppers: Vec<(&Case, Stepper<'_>)> = pair
            .iter()
            .map(|(case, frames, samples)| {
                let config = with_levers(case);
                let spotter = KeywordSpotter::with_model(&config, Arc::clone(&shared))
                    .expect("spotter on the shared model");
                let input = Input::Features {
                    frames,
                    samples: *samples,
                };
                let stepper = Stepper::new(spotter, config.features.num_bins, input, case.feed_samples);
                (*case, stepper)
            })
            .collect();
        if pair.len() == 2 && pair[0].0.keyword_set != pair[1].0.keyword_set {
            distinct_sets += 1;
        }
        // Round-robin until every stream is fed.
        while steppers
            .iter_mut()
            .map(|(_, stepper)| stepper.step())
            .fold(false, |any, stepped| any | stepped)
        {}
        for (case, stepper) in &steppers {
            match compare(case, stepper.decodes, &stepper.hits) {
                None => eprintln!(
                    "PASS shared {}: decodes={} detections={}",
                    case.id,
                    stepper.decodes,
                    stepper.hits.len()
                ),
                Some(diff) => {
                    eprintln!("FAIL shared {}: {diff}", case.id);
                    failures.push(format!("shared {}", case.id));
                }
            }
        }
    }
    assert!(distinct_sets > 0, "no interleaved pair spans two keyword sets");

    let mut other = with_levers(cases[0].0);
    other.threads = 2;
    other.static_batch = false;
    let err = KeywordSpotter::with_model(&other, Arc::clone(&shared))
        .err()
        .expect("a config naming another model must be refused");
    assert!(
        err.to_string().contains("threads, static_batch"),
        "unexpected error: {err}"
    );

    assert!(
        failures.is_empty(),
        "{} runs diverge from the Sherpa reference with the speed levers on: {failures:?}",
        failures.len()
    );
    eprintln!(
        "{} cases match the Sherpa reference exactly with static_batch + decoder_cache, alone and \
         on one shared model ({distinct_sets} interleaved pairs across keyword sets)",
        cases.len()
    );
}

/// Keyword set built to fire on ordinary speech; see `kws_waveform_detections_track_sherpa`.
const STRESS_SET: &str = "dense";

/// Waveform parity: the Rust filterbank instead of Sherpa's. Decode counts, detection
/// decode indices, keywords, tokens and the last token's time must match; earlier token
/// times may drift where ulp-level filterbank noise flips a near-tied beam merge, and are
/// reported, not failed. The `dense` stress set (35 short keywords, thresholds down to
/// 0.02) fires on marginal paths that the same noise moves, so it is reported only; the
/// injected-fbank test holds it to exact equality.
#[test]
fn kws_waveform_detections_track_sherpa() {
    let h = harness!();
    let mut failures = Vec::new();
    let mut drifted = Vec::new();
    let mut first_token_deltas: BTreeMap<i64, usize> = BTreeMap::new();
    let mut token_diffs = 0usize;
    let mut detections = 0usize;
    for case in &h.reference.cases {
        let audio = h.case_audio(case);
        let (decodes, hits) = run(&h.parity_config(case), Input::Wave(&audio), case.feed_samples);
        let mut problems = Vec::new();
        let mut notes = Vec::new();
        if decodes != case.decodes {
            problems.push(format!("decodes expected {} got {decodes}", case.decodes));
        }
        // Walk the detections in order; the first missing, extra or different one ends the
        // comparison for this case (later ones would only be misaligned).
        for i in 0..case.detections.len().max(hits.len()) {
            let (want, got) = match (case.detections.get(i), hits.get(i)) {
                (Some(w), Some(g))
                    if (w.decode, &w.keyword, &w.tokens) == (g.decode, &g.keyword, &g.tokens) =>
                {
                    (w, g)
                }
                (w, g) => {
                    let at = [w.map(|d| d.decode), g.map(|h| h.decode)]
                        .into_iter()
                        .flatten()
                        .min()
                        .unwrap_or(0);
                    problems.push(format!(
                        "first divergence at decode {at} (detection #{i} of {} expected, {} got): expected {}; got {}",
                        case.detections.len(),
                        hits.len(),
                        w.map_or("nothing".into(), show_ref),
                        g.map_or("nothing".into(), show_hit),
                    ));
                    break;
                }
            };
            detections += 1;
            for (j, (w, g)) in want.timestamps.iter().zip(&got.timestamps).enumerate() {
                if (*w as f32) != *g {
                    token_diffs += 1;
                    let (wf, gf) = (ref_frame(*w), got.frames[j] as i64);
                    notes.push(format!(
                        "detection #{i} decode {} {} token {j} {}: sherpa frame {wf} ({}) rust frame {gf} ({g}), {:+} frames",
                        got.decode, got.keyword, got.tokens[j], *w as f32, gf - wf
                    ));
                }
            }
            if let (Some(w), Some(g)) = (want.timestamps.first(), got.frames.first()) {
                *first_token_deltas
                    .entry(*g as i64 - ref_frame(*w))
                    .or_default() += 1;
            }
            let (w, g) = (want.timestamps.last(), got.timestamps.last());
            if w.map(|w| *w as f32) != g.copied() {
                problems.push(format!(
                    "detection #{i} decode {} {}: last token time expected {:?} got {g:?}",
                    got.decode,
                    got.keyword,
                    w.map(|w| *w as f32)
                ));
            }
        }
        let strict = case.keyword_set != STRESS_SET;
        let status = match (problems.is_empty(), strict) {
            (true, _) => "PASS",
            (false, true) => "FAIL",
            (false, false) => "DRIFT",
        };
        eprintln!(
            "{status} {}: decodes={decodes} detections={} token-time diffs={}",
            case.id,
            hits.len(),
            notes.len()
        );
        for line in problems.iter().chain(&notes) {
            eprintln!("  {line}");
        }
        if !problems.is_empty() {
            if strict {
                failures.push(case.id.clone());
            } else {
                drifted.push(case.id.clone());
            }
        }
    }
    if !drifted.is_empty() {
        eprintln!("waveform: {} `{STRESS_SET}` cases drift (reported, not failed): {drifted:?}", drifted.len());
    }
    let histogram: Vec<String> = first_token_deltas
        .iter()
        .map(|(delta, n)| format!("{delta:+} frames x{n}"))
        .collect();
    eprintln!(
        "waveform: {detections} matched detections, {token_diffs} token times differ; \
         first-token delta (rust - sherpa): {}",
        histogram.join(", ")
    );
    assert!(
        failures.is_empty(),
        "{} of {} waveform cases miss a decode, detection, keyword, token or last-token time: {failures:?}",
        failures.len(),
        h.reference.cases.len()
    );
}

#[test]
fn kws_chunking_is_invariant() {
    let h = harness!();
    // Cases with detections exercise the most state; fall back to the first cases.
    let mut picked: Vec<&Case> = h
        .reference
        .cases
        .iter()
        .filter(|c| !c.detections.is_empty())
        .take(2)
        .collect();
    if picked.is_empty() {
        picked = h.reference.cases.iter().take(2).collect();
    }
    assert!(!picked.is_empty(), "reference has no cases");
    for case in picked {
        let audio = h.case_audio(case);
        let config = h.parity_config(case);
        let baseline = run(&config, Input::Wave(&audio), case.feed_samples);
        for feed in [160, 1_001, 4_800, 16_000, audio.len()] {
            let other = run(&config, Input::Wave(&audio), feed);
            assert_eq!(
                other, baseline,
                "case {}: feeding {feed} samples at a time changed the decodes or detections \
                 (baseline feeds {})",
                case.id, case.feed_samples
            );
        }
        eprintln!(
            "PASS {}: decodes={} detections={} identical for feeds {} / 160 / 1001 / 4800 / 16000 / all",
            case.id,
            baseline.0,
            baseline.1.len(),
            case.feed_samples
        );
    }
}

#[test]
fn kws_production_policy_smoke() {
    let h = harness!();
    // Long silences are where upstream's trailing-blank reset would restart timestamps.
    let long_silence = |c: &&Case| {
        c.segments.iter().any(|s| {
            matches!(s, Segment::Silence { silence_samples } if *silence_samples >= 24_000)
        })
    };
    let mut cases: Vec<&Case> = h.reference.cases.iter().filter(long_silence).collect();
    if cases.is_empty() {
        eprintln!("no case holds 1.5 s of silence; running every case");
        cases = h.reference.cases.iter().collect();
    }
    let fpd = FRAMES_PER_DECODE as i64;
    for case in cases {
        let audio = h.case_audio(case);
        let (decodes, hits) = run(&h.production_config(case), Input::Wave(&audio), case.feed_samples);
        eprintln!(
            "PROD {}: decodes={decodes} detections={} (reference policy found {})",
            case.id,
            hits.len(),
            case.detections.len()
        );
        let mut previous_last: Option<f32> = None;
        for hit in &hits {
            eprintln!("  {}", show_hit(hit));
            assert!(
                hit.timestamps.windows(2).all(|w| w[0] <= w[1]),
                "case {}: timestamps go backwards inside {}",
                case.id,
                show_hit(hit)
            );
            let (first, last) = (hit.timestamps[0], *hit.timestamps.last().unwrap());
            if let Some(prev) = previous_last {
                assert!(
                    first > prev,
                    "case {}: timestamps restarted: {} starts at {first} after a detection ending at {prev}",
                    case.id,
                    show_hit(hit)
                );
            }
            previous_last = Some(last);
            // Frames count from the start of the stream: the keyword ended before this
            // decode's last frame and began no more than the TTL before its first.
            let d = hit.decode as i64;
            let (f0, fl) = (hit.frames[0] as i64, *hit.frames.last().unwrap() as i64);
            assert!(
                fl < (d + 1) * fpd && f0 >= d * fpd - PRODUCTION_TTL_FRAMES as i64,
                "case {}: frames {:?} are not stream time for decode {}",
                case.id,
                hit.frames,
                hit.decode
            );
        }
    }
}

#[test]
fn kws_rejects_wrong_sample_rate_and_foreign_stream() {
    let h = harness!();
    let case = &h.reference.cases[0];
    let config = h.parity_config(case);
    let mut owner = KeywordSpotter::new(&config).expect("create spotter");
    let mut other = KeywordSpotter::new(&config).expect("create second spotter");
    let audio = h.case_audio(case);
    let mut stream = owner.create_stream();

    let err = stream
        .accept_waveform(8_000, &audio[..16_000.min(audio.len())])
        .expect_err("8 kHz audio must be rejected");
    assert!(matches!(err, Error::Audio(_)), "unexpected error: {err}");
    assert!(err.to_string().contains("8000"), "{err}");
    assert!(
        !owner.is_ready(&stream),
        "rejected audio must not be buffered"
    );

    let mut fresh = owner.create_stream();
    assert!(
        owner.decode(&mut fresh).is_err(),
        "decoding a stream that is not ready must fail"
    );

    stream
        .accept_waveform(16_000, &audio[..16_000.min(audio.len())])
        .expect("accept 16 kHz audio");
    assert!(owner.is_ready(&stream));
    let err = other
        .decode(&mut stream)
        .expect_err("a stream must not decode on another spotter");
    assert!(
        err.to_string().contains("different spotter"),
        "unexpected error: {err}"
    );
    // The refused decode left the stream intact for its owner.
    owner.decode(&mut stream).expect("owner decodes its stream");
}
