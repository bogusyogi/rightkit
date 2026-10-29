//! In-crate parity tests against the Sherpa reference fixtures (tests/fixtures/kws).
//!
//! - Fbank: [`OnlineFbank`] over each `fbank` fixture clip against Sherpa's own frames:
//!   mean |diff| 1e-6, 2e-3 on every bin, 2e-4 on bins whose reference log-mel exceeds -10.
//!   Sherpa's KISS FFT is built with `-ffast-math`, so its rounding depends on the compiler
//!   and no port can be bit-exact with every Sherpa build; these bounds hold the measured
//!   GCC-build differences (max 1.4e-3 at the log floor, 1.6e-4 above -10, mean < 8e-7).
//!   Needs only the fixtures; a fixture tree without the fbank dumps (the published
//!   crate) skips.
//! - Encoder: [`Zipformer2Transducer::run_encoder`] chunk by chunk on the reference's own
//!   fbank frames (so the encoder is compared in isolation) against Sherpa's encoder
//!   output (gate 1e-3). Needs `RK_KWS_MODEL_DIR` and `RK_ORT_TEST_DYLIB`.
//! - Speed-lever preconditions, on this host's ONNX Runtime (run them on every OS that turns
//!   a lever on): the decoder computes every row independently of its batch (what
//!   `decoder_cache` relies on), and the `static_batch` encoder is bit-identical to the
//!   dynamic one on the reference features. Same assets as the encoder test.
//!
//! A missing asset prints a SKIP line and the test passes; `RIGHTKIT_REQUIRE_ASSETS=1`
//! makes it fail. `RK_KWS_FIXTURES` names a fixtures root to use instead of `tests/fixtures`.
//! The model-free unit tests at the end cover graph and keyword-list edge cases the
//! module tests leave out.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::Deserialize;

use super::context_graph::{ContextGraph, ROOT};
use super::fbank::{FbankConfig, FbankTables, OnlineFbank};
use super::keywords::{encode_keywords, SymbolTable};
use super::math::Math;
use super::model::Zipformer2Transducer;

const SCHEMA: &str = "rightkit.kws-reference.v1";
/// Every bin. Sherpa's GCC build runs its FFT under `-ffast-math`, so low-energy bins near
/// the log floor differ by up to ~1.4e-3.
const FBANK_GATE: f32 = 2e-3;
/// Bins whose reference log-mel exceeds [`FBANK_LOUD_LOG_MEL`].
const FBANK_LOUD_GATE: f32 = 2e-4;
/// Mean over every value of a clip.
const FBANK_MEAN_GATE: f64 = 1e-6;
const FBANK_LOUD_LOG_MEL: f32 = -10.0;
const ENCODER_GATE: f32 = 1e-3;

#[derive(Deserialize)]
struct Reference {
    schema: String,
    model: ModelRef,
    audio: std::collections::BTreeMap<String, AudioRef>,
    #[serde(default)]
    fbank: Vec<FbankRef>,
    #[serde(default)]
    encoder: Vec<EncoderRef>,
}

#[derive(Deserialize)]
struct ModelRef {
    encoder: String,
    encoder_sha256: String,
    decoder: String,
    decoder_sha256: String,
    joiner: String,
    joiner_sha256: String,
}

#[derive(Deserialize)]
struct AudioRef {
    path: String,
    samples: usize,
    sha256: String,
}

#[derive(Deserialize)]
struct FbankRef {
    audio: String,
    frames: usize,
    dim: usize,
    file: String,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Deserialize)]
struct EncoderRef {
    audio: String,
    chunks: usize,
    frames_per_chunk: usize,
    dim: usize,
    file: String,
    #[serde(default)]
    onnxruntime: Option<String>,
}

/// Prints `SKIP <test>: <what> (looked at <where>)`, or panics with it under
/// `RIGHTKIT_REQUIRE_ASSETS=1` (the policy of `tests/common`).
fn skip_missing(what: &str, looked_at: &str) {
    let test = std::thread::current().name().unwrap_or("unnamed").to_owned();
    let message = format!("SKIP {test}: {what} (looked at {looked_at})");
    if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|v| v == "1") {
        panic!("{message}");
    }
    eprintln!("{message}");
}

/// `require!(option, what, looked_at)`: the value, or a SKIP and a return from the test.
macro_rules! require {
    ($opt:expr, $what:expr, $looked_at:expr $(,)?) => {
        match $opt {
            Some(v) => v,
            None => {
                skip_missing($what, &$looked_at);
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

fn fixtures_root() -> PathBuf {
    env_path("RK_KWS_FIXTURES")
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"))
}

fn load_reference(root: &Path) -> Option<Reference> {
    let path = root.join("kws/reference.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let reference: Reference = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    assert_eq!(reference.schema, SCHEMA, "{}", path.display());
    Some(reference)
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

fn read_f32le(path: &Path, expected_len: usize) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_eq!(
        bytes.len(),
        expected_len * 4,
        "{} holds {} bytes, expected {} f32 values",
        path.display(),
        bytes.len(),
        expected_len
    );
    let (values, _) = bytes.as_chunks::<4>();
    values.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

fn wav(root: &Path, reference: &Reference, name: &str) -> Vec<f32> {
    let entry = reference
        .audio
        .get(name)
        .unwrap_or_else(|| panic!("audio `{name}` is not in the reference audio map"));
    let path = root.join(&entry.path);
    check_sha256(&path, &entry.sha256);
    let mut reader =
        hound::WavReader::open(&path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let spec = reader.spec();
    assert_eq!(
        (spec.sample_rate, spec.channels, spec.bits_per_sample),
        (16_000, 1, 16),
        "{}",
        path.display()
    );
    let pcm: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.expect("wav sample") as f32 / 32768.0)
        .collect();
    assert_eq!(pcm.len(), entry.samples, "{} sample count", path.display());
    pcm
}

/// The audio an fbank dump was computed from: `+`-joined parts, each an audio-map key, or
/// `sil<n>` / `tail<n>` for `n` samples of digital silence (`en_0.wav+sil64000+en_1.wav+tail3200`).
fn clip_audio(root: &Path, reference: &Reference, clip: &str) -> Vec<f32> {
    let mut pcm = Vec::new();
    for part in clip.split('+') {
        if reference.audio.contains_key(part) {
            pcm.extend(wav(root, reference, part));
            continue;
        }
        let zeros = ["sil", "tail"]
            .iter()
            .find_map(|prefix| part.strip_prefix(prefix)?.parse::<usize>().ok())
            .unwrap_or_else(|| panic!("fbank clip `{clip}`: unknown part `{part}`"));
        pcm.resize(pcm.len() + zeros, 0.0);
    }
    pcm
}

/// Max and mean absolute difference, and the flat index of the max.
fn diff_stats(ours: &[f32], theirs: &[f32]) -> (f32, f64, usize) {
    assert_eq!(ours.len(), theirs.len());
    let (mut max, mut at, mut sum) = (0.0f32, 0usize, 0.0f64);
    for (i, (a, b)) in ours.iter().zip(theirs).enumerate() {
        let d = (a - b).abs();
        sum += d as f64;
        if d > max || d.is_nan() {
            max = d;
            at = i;
        }
    }
    (max, sum / ours.len().max(1) as f64, at)
}

/// Bind and initialise the ONNX Runtime named by `RK_ORT_TEST_DYLIB` once per process.
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
    match outcome {
        Ok(info) => eprintln!("onnxruntime: {info}"),
        Err(e) => panic!("bind the ONNX Runtime test library: {e}"),
    }
}

#[test]
fn kws_fbank_matches_kaldi_native_fbank() {
    let root = fixtures_root();
    let reference = require!(
        load_reference(&root),
        "KWS Sherpa reference fixture",
        root.join("kws/reference.json").display().to_string()
    );
    assert!(!reference.fbank.is_empty(), "reference.json lists no fbank clips");
    let tables = Arc::new(FbankTables::new(&FbankConfig::default(), Math::PLATFORM).unwrap());
    let mut failures = Vec::new();
    for entry in &reference.fbank {
        assert_eq!(entry.dim, tables.dim(), "{}: fbank dim", entry.file);
        let path = root.join("kws").join(&entry.file);
        if !path.is_file() {
            skip_missing("KWS Sherpa fbank dump", &path.display().to_string());
            return;
        }
        if let Some(sha) = &entry.sha256 {
            check_sha256(&path, sha);
        }
        let theirs = read_f32le(&path, entry.frames * entry.dim);
        let pcm = clip_audio(&root, &reference, &entry.audio);
        let mut fbank = OnlineFbank::new(Arc::clone(&tables));
        fbank.accept_waveform(&pcm).unwrap();
        // The reference holds the frames ready without a flush, as Sherpa's stream does.
        assert_eq!(fbank.num_frames_ready(), entry.frames, "{}: frame count", entry.file);
        let ours = fbank.frames(0, entry.frames).unwrap();

        let (max, mean, at) = diff_stats(&ours, &theirs);
        let exact = ours
            .iter()
            .zip(&theirs)
            .filter(|(a, b)| a.to_bits() == b.to_bits())
            .count();
        // Bins clear of the log(FLT_EPSILON) floor (-15.9), where fast-math FFT noise is
        // relatively large.
        let (mut loud_max, mut loud_at) = (0.0f32, None);
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            let d = (a - b).abs();
            if *b > FBANK_LOUD_LOG_MEL && (d > loud_max || d.is_nan()) {
                loud_max = d;
                loud_at = Some(i);
            }
        }
        let place = |i: usize| format!("frame {} bin {}", i / entry.dim, i % entry.dim);
        eprintln!(
            "fbank {}: {} frames; max |diff| {max:.3e} at {} (sherpa log-mel {}, ours {}), mean {mean:.3e}; \
             above log-mel {FBANK_LOUD_LOG_MEL}: max |diff| {loud_max:.3e}{}; bit-exact {:.4}% ({exact}/{})",
            entry.audio,
            entry.frames,
            place(at),
            theirs[at],
            ours[at],
            loud_at.map_or(String::new(), |i| format!(" at {} (sherpa log-mel {})", place(i), theirs[i])),
            100.0 * exact as f64 / ours.len().max(1) as f64,
            ours.len()
        );
        if mean > FBANK_MEAN_GATE || mean.is_nan() {
            failures.push(format!("{}: mean |diff| {mean:e} exceeds {FBANK_MEAN_GATE:e}", entry.audio));
        }
        if max > FBANK_GATE || max.is_nan() {
            failures.push(format!(
                "{}: max |diff| {max:e} at {} (sherpa {}, ours {}) exceeds {FBANK_GATE:e}",
                entry.audio,
                place(at),
                theirs[at],
                ours[at]
            ));
        }
        if loud_max > FBANK_LOUD_GATE || loud_max.is_nan() {
            let i = loud_at.unwrap_or(0);
            failures.push(format!(
                "{}: max |diff| {loud_max:e} above log-mel {FBANK_LOUD_LOG_MEL} at {} (sherpa {}, ours {}) exceeds {FBANK_LOUD_GATE:e}",
                entry.audio,
                place(i),
                theirs[i],
                ours[i]
            ));
        }
    }
    assert!(failures.is_empty(), "fbank parity: {failures:#?}");
}

#[test]
fn kws_encoder_matches_sherpa_on_reference_features() {
    let root = fixtures_root();
    let reference = require!(
        load_reference(&root),
        "KWS Sherpa reference fixture",
        root.join("kws/reference.json").display().to_string()
    );
    let model_dir = require!(
        env_path("RK_KWS_MODEL_DIR"),
        "KWS zipformer model directory",
        "RK_KWS_MODEL_DIR (encoder, decoder, joiner named in reference.json)"
    );
    let runtime = require!(
        env_path("RK_ORT_TEST_DYLIB"),
        "ONNX Runtime dylib",
        "RK_ORT_TEST_DYLIB (absolute path to the native ONNX Runtime library)"
    );
    assert!(!reference.encoder.is_empty(), "reference.json lists no encoder dumps");
    bind_runtime(&runtime);
    let m = &reference.model;
    let (encoder, decoder, joiner) = (
        model_dir.join(&m.encoder),
        model_dir.join(&m.decoder),
        model_dir.join(&m.joiner),
    );
    check_sha256(&encoder, &m.encoder_sha256);
    check_sha256(&decoder, &m.decoder_sha256);
    check_sha256(&joiner, &m.joiner_sha256);
    let dim = FbankTables::new(&FbankConfig::default(), Math::PLATFORM)
        .unwrap()
        .dim();
    let mut model =
        Zipformer2Transducer::load(&encoder, &decoder, &joiner, dim, 1, false).unwrap();
    let (size, shift) = (model.chunk_size(), model.chunk_shift());

    let mut worst = 0.0f32;
    for entry in &reference.encoder {
        let fb = reference
            .fbank
            .iter()
            .find(|f| f.audio == entry.audio)
            .unwrap_or_else(|| panic!("encoder dump {} has no fbank dump for its audio", entry.file));
        assert_eq!(fb.dim, dim);
        let features = read_f32le(&root.join("kws").join(&fb.file), fb.frames * fb.dim);
        let expected = read_f32le(
            &root.join("kws").join(&entry.file),
            entry.chunks * entry.frames_per_chunk * entry.dim,
        );
        assert!(
            (entry.chunks - 1) * shift + size <= fb.frames,
            "{}: {} chunks need {} fbank frames; the dump has {}",
            entry.file,
            entry.chunks,
            (entry.chunks - 1) * shift + size,
            fb.frames
        );
        eprintln!(
            "encoder {}: {} chunks of {} frames, reference onnxruntime {}",
            entry.audio,
            entry.chunks,
            entry.frames_per_chunk,
            entry.onnxruntime.as_deref().unwrap_or("?")
        );
        let mut states = model.init_states();
        let per_chunk = entry.frames_per_chunk * entry.dim;
        let mut entry_max = 0.0f32;
        let mut failures = Vec::new();
        for c in 0..entry.chunks {
            let start = c * shift;
            let x = &features[start * dim..(start + size) * dim];
            let out = model.run_encoder(x, &mut states).unwrap();
            assert_eq!(
                (out.frames, out.dim),
                (entry.frames_per_chunk, entry.dim),
                "{}: chunk {c} shape",
                entry.file
            );
            let want = &expected[c * per_chunk..(c + 1) * per_chunk];
            let (max, mean, at) = diff_stats(&out.data, want);
            eprintln!(
                "  chunk {c:3}: max |diff| {max:.3e} at frame {} dim {}, mean {mean:.3e}",
                at / entry.dim,
                at % entry.dim
            );
            entry_max = entry_max.max(max);
            if max > ENCODER_GATE {
                failures.push((c, max));
            }
        }
        eprintln!("encoder {}: max |diff| {entry_max:.3e}", entry.audio);
        worst = worst.max(entry_max);
        assert!(
            failures.is_empty(),
            "encoder {}: chunks over the {ENCODER_GATE:e} gate (chunk, max): {failures:?}",
            entry.audio
        );
    }
    eprintln!("encoder worst max |diff| {worst:.3e}");
}

/// The reference model's files, checked against `reference.json`, with ONNX Runtime bound;
/// `None` after a SKIP line.
fn model_files() -> Option<(PathBuf, Reference, [PathBuf; 3])> {
    let root = fixtures_root();
    let Some(reference) = load_reference(&root) else {
        skip_missing(
            "KWS Sherpa reference fixture",
            &root.join("kws/reference.json").display().to_string(),
        );
        return None;
    };
    let Some(model_dir) = env_path("RK_KWS_MODEL_DIR") else {
        skip_missing("KWS zipformer model directory", "RK_KWS_MODEL_DIR");
        return None;
    };
    let Some(runtime) = env_path("RK_ORT_TEST_DYLIB") else {
        skip_missing("ONNX Runtime dylib", "RK_ORT_TEST_DYLIB");
        return None;
    };
    bind_runtime(&runtime);
    let m = &reference.model;
    let files = [
        model_dir.join(&m.encoder),
        model_dir.join(&m.decoder),
        model_dir.join(&m.joiner),
    ];
    for (file, sha) in files
        .iter()
        .zip([&m.encoder_sha256, &m.decoder_sha256, &m.joiner_sha256])
    {
        check_sha256(file, sha);
    }
    Some((root, reference, files))
}

fn feature_dim() -> usize {
    FbankTables::new(&FbankConfig::default(), Math::PLATFORM)
        .unwrap()
        .dim()
}

/// `decoder_cache` precondition: every decoder row is bit-identical whatever batch computes
/// it. Every context the beam can form (each token after the blank-context `-1` and after
/// every token) is computed alone, then again in batches of 2 to 16 in shuffled orders.
#[test]
fn kws_decoder_is_batch_invariant() {
    let Some((_, _, [encoder, decoder, joiner])) = model_files() else {
        return;
    };
    let mut model =
        Zipformer2Transducer::load(&encoder, &decoder, &joiner, feature_dim(), 1, false).unwrap();
    let (cs, vocab) = (model.context_size(), model.vocab_size() as i64);
    assert_eq!(cs, 2, "this test enumerates two-token contexts");
    let mut contexts: Vec<[i64; 2]> = Vec::new();
    for prev in -1..vocab {
        for token in 0..vocab {
            contexts.push([prev, token]);
        }
    }
    let alone: Vec<Vec<f32>> = contexts
        .iter()
        .map(|c| model.run_decoder(c.to_vec()).unwrap().1)
        .collect();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let (mut rows, mut differ) = (0usize, 0usize);
    let mut first_difference = None;
    for batch in [2usize, 3, 4, 5, 8, 16] {
        // Fisher-Yates on a fixed LCG: each batch size sees other neighbours.
        let mut order: Vec<usize> = (0..contexts.len()).collect();
        for i in (1..order.len()).rev() {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            order.swap(i, (seed >> 33) as usize % (i + 1));
        }
        for group in order.chunks(batch) {
            let ids: Vec<i64> = group.iter().flat_map(|&i| contexts[i]).collect();
            let (dim, out) = model.run_decoder(ids).unwrap();
            for (&i, row) in group.iter().zip(out.chunks_exact(dim)) {
                rows += 1;
                let same = row
                    .iter()
                    .zip(&alone[i])
                    .all(|(a, b)| a.to_bits() == b.to_bits());
                if !same {
                    differ += 1;
                    first_difference.get_or_insert((batch, contexts[i]));
                }
            }
        }
    }
    eprintln!(
        "decoder batch invariance: {} contexts, {rows} batched rows, {differ} differ from the row computed alone",
        contexts.len()
    );
    assert_eq!(
        differ, 0,
        "decoder rows depend on their batch (first: batch, context = {first_difference:?}); \
         decoder_cache is not exact on this host"
    );
}

/// `static_batch` precondition: the encoder built with `N` pinned to 1 gives the same bits as
/// the dynamic one, chunk by chunk with carried states, on every reference clip's features.
#[test]
fn kws_static_batch_encoder_is_bit_identical() {
    let Some((root, reference, [encoder, decoder, joiner])) = model_files() else {
        return;
    };
    let dim = feature_dim();
    let mut dynamic = Zipformer2Transducer::load(&encoder, &decoder, &joiner, dim, 1, false).unwrap();
    let mut pinned = Zipformer2Transducer::load(&encoder, &decoder, &joiner, dim, 1, true).unwrap();
    let (size, shift) = (dynamic.chunk_size(), dynamic.chunk_shift());
    let (mut chunks, mut differ) = (0usize, 0usize);
    for fb in &reference.fbank {
        let path = root.join("kws").join(&fb.file);
        if !path.is_file() {
            skip_missing("KWS Sherpa fbank dump", &path.display().to_string());
            return;
        }
        let features = read_f32le(&path, fb.frames * fb.dim);
        let (mut a, mut b) = (dynamic.init_states(), pinned.init_states());
        let mut start = 0;
        while start + size <= fb.frames {
            let x = &features[start * dim..(start + size) * dim];
            let (da, db) = (
                dynamic.run_encoder(x, &mut a).unwrap(),
                pinned.run_encoder(x, &mut b).unwrap(),
            );
            chunks += 1;
            let same = da.data.len() == db.data.len()
                && da.data.iter().zip(&db.data).all(|(p, q)| p.to_bits() == q.to_bits())
                && a == b;
            if !same {
                differ += 1;
                let (max, _, _) = diff_stats(&da.data, &db.data);
                eprintln!("  {} chunk at frame {start}: max |diff| {max:.3e}", fb.audio);
            }
            start += shift;
        }
    }
    eprintln!("static batch encoder: {chunks} chunks, {differ} differ from the dynamic encoder");
    assert!(chunks > 0, "no reference features to encode");
    assert_eq!(differ, 0, "static_batch changes encoder output on this host");
}

// --- Model-free edge cases -------------------------------------------------------------

#[test]
fn repeated_keyword_takes_the_later_phrase_and_threshold_and_the_larger_boost() {
    let ids = vec![vec![5, 6], vec![5, 6]];
    let phrases = vec!["A".to_string(), "B".to_string()];
    let g = ContextGraph::new(&ids, 1.0, 0.25, &[1.0, 2.0], &phrases, &[0.1, 0.2]);
    let (s1, a) = g.forward_one_step(ROOT, 5);
    let (s2, b) = g.forward_one_step(a, 6);
    assert_eq!(s1, 2.0);
    // Token score 2 plus the end node's output score (node score 4).
    assert_eq!(s2, 6.0);
    let end = g.state(g.matched(b).unwrap());
    assert_eq!((end.phrase.as_str(), end.ac_threshold, end.level), ("B", 0.2, 2));
}

#[test]
fn keyword_ending_inside_a_longer_one_matches_at_its_own_end() {
    let ids = vec![vec![5, 6], vec![5, 6, 7]];
    let phrases = vec!["SHORT".to_string(), "LONG".to_string()];
    let g = ContextGraph::new(&ids, 1.0, 0.25, &[], &phrases, &[]);
    let (s1, a) = g.forward_one_step(ROOT, 5);
    let (s2, b) = g.forward_one_step(a, 6);
    let (s3, c) = g.forward_one_step(b, 7);
    assert_eq!((s1, s2, s3), (1.0, 3.0, 4.0));
    assert_eq!(g.matched(a), None);
    assert_eq!(g.state(g.matched(b).unwrap()).phrase, "SHORT");
    assert_eq!(g.state(g.matched(c).unwrap()).phrase, "LONG");
    assert_eq!(g.state(c).level, 3);
}

#[test]
fn unknown_token_at_the_root_stays_at_the_root_without_score() {
    let g = ContextGraph::new(&[vec![5, 6]], 1.0, 0.25, &[], &[], &[]);
    assert_eq!(g.forward_one_step(ROOT, 9), (0.0, ROOT));
    assert_eq!(g.matched(ROOT), None);
}

#[test]
fn blank_keyword_lines_encode_as_empty_keywords() {
    // EncodeBase pushes one id list per line, empty lines included.
    let symbols = SymbolTable::parse("<blk> 0\nZ 3\nEH1 4\nF 5\nER0 6\n").unwrap();
    let k = encode_keywords("Z EH1\n\nF ER0 @FER\n", &symbols).unwrap();
    assert_eq!(k.ids, vec![vec![3, 4], vec![], vec![5, 6]]);
    assert_eq!(k.phrases, vec![String::new(), String::new(), "FER".into()]);
    // An empty keyword adds no node: the first token of the next one starts at the root.
    let g = ContextGraph::new(&k.ids, 1.0, 0.25, &k.scores, &k.phrases, &k.thresholds);
    let (_, a) = g.forward_one_step(ROOT, 5);
    let (_, b) = g.forward_one_step(a, 6);
    assert_eq!(g.state(g.matched(b).unwrap()).phrase, "FER");
}

#[test]
fn tokens_table_reads_a_bare_id_as_the_space_token_and_keeps_the_first_duplicate() {
    let t = SymbolTable::parse("<blk> 0\n7\nA 1\nA 2\nB 1\n").unwrap();
    assert_eq!(t.id(" "), Some(7));
    assert_eq!(t.symbol(7), " ");
    assert_eq!(t.id("A"), Some(1));
    assert_eq!(t.symbol(1), "A");
    assert_eq!(t.symbol(2), "A");
    assert_eq!(t.symbol(99), "");
}

#[test]
fn fbank_refuses_audio_after_input_finished() {
    let mut f = OnlineFbank::new(Arc::new(
        FbankTables::new(&FbankConfig::default(), Math::PLATFORM).unwrap(),
    ));
    f.accept_waveform(&[0.1; 1_000]).unwrap();
    let before = f.num_frames_ready();
    f.input_finished();
    assert!(f.num_frames_ready() > before, "flush releases the tail frames");
    assert!(f.accept_waveform(&[0.1; 160]).is_err());
}
