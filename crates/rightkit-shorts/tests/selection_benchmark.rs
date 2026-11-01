//! Shorts selection benchmark (issue #115).
//!
//! Runs the real ranker offline on labelled synthetic transcripts in
//! `tests/fixtures/benchmark/`. Each fixture carries human-chosen best moments
//! (`labels`) and, for the LLM path, a recorded model answer (`recorded_llm`). The
//! heuristic path calls [`propose_shorts`]; the LLM path calls
//! [`propose_shorts_ranked`] with a replay provider, so no network or key is needed.
//!
//! Metric: precision@k per fixture, k = 3. A top-k proposal is a hit when its
//! temporal IoU with at least one human label is >= `iou_hit` (0.5). Missing
//! proposals count as misses. Per preset, the mean over fixtures must reach the floor
//! in `tests/fixtures/benchmark/floors.json`. Any fixture with no floor, or an LLM
//! path that silently fell back to the heuristic, fails the benchmark.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

use rightkit_shorts::{
    propose_shorts, propose_shorts_ranked, LlmMoment, MomentProvider, MomentRequest, Proposal,
    Proposals, ProposeInput, ProviderError, ShortsProfile,
};
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as SCHEMA};
use serde::Deserialize;

const HEX: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const K: usize = 3;
const IOU_HIT: f64 = 0.5;

#[derive(Deserialize)]
struct SentenceSpec {
    start_ms: u64,
    end_ms: u64,
    text: String,
}

#[derive(Deserialize, Clone, Copy)]
struct LabelSpan {
    start_ms: u64,
    end_ms: u64,
}

#[derive(Deserialize)]
struct Fixture {
    id: String,
    preset: String,
    sentences: Vec<SentenceSpec>,
    labels: Vec<LabelSpan>,
    recorded_llm: Vec<LlmMoment>,
}

#[derive(Deserialize)]
struct PresetFloor {
    heuristic: f64,
    llm_replay: f64,
}

#[derive(Deserialize)]
struct Floors {
    schema: String,
    k: usize,
    iou_hit: f64,
    floors: BTreeMap<String, PresetFloor>,
}

#[derive(Clone, Copy)]
enum Path {
    Heuristic,
    LlmReplay,
}

impl Path {
    fn name(self) -> &'static str {
        match self {
            Path::Heuristic => "heuristic",
            Path::LlmReplay => "llm_replay",
        }
    }
}

/// Replays the recorded model answer for one fixture.
struct Replay {
    moments: Vec<LlmMoment>,
}

impl MomentProvider for Replay {
    fn name(&self) -> &str {
        "recorded"
    }
    fn model(&self) -> &str {
        "recorded-fixture"
    }
    fn rank_moments(&self, _request: &MomentRequest) -> Result<Vec<LlmMoment>, ProviderError> {
        Ok(self.moments.clone())
    }
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/benchmark")
}

fn load_fixtures() -> Vec<Fixture> {
    let mut paths: Vec<PathBuf> = fs::read_dir(fixture_dir())
        .expect("benchmark fixture dir exists")
        .map(|e| e.expect("readable dir entry").path())
        .filter(|p| {
            p.extension() == Some(OsStr::new("json"))
                && p.file_name() != Some(OsStr::new("floors.json"))
        })
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no benchmark fixtures found");
    paths
        .iter()
        .map(|p| {
            let bytes = fs::read(p).expect("fixture readable");
            serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        })
        .collect()
}

fn load_floors() -> Floors {
    let bytes = fs::read(fixture_dir().join("floors.json")).expect("floors.json readable");
    let floors: Floors = serde_json::from_slice(&bytes).expect("floors.json parses");
    assert_eq!(floors.schema, "cutright.shorts.benchmark.floors.v1");
    assert_eq!(floors.k, K, "floors.json k must match the benchmark");
    assert_eq!(floors.iou_hit, IOU_HIT, "floors.json iou_hit must match");
    floors
}

/// Builds a word-level transcript: each sentence's words are spread evenly across its
/// span, with 60 ms between words. Sentences are separated by pauses above the
/// 600 ms sentence boundary, so each spec becomes exactly one sentence.
fn build_transcript(f: &Fixture) -> Transcript {
    let mut words = Vec::new();
    for s in &f.sentences {
        let ws: Vec<&str> = s.text.split_whitespace().collect();
        let per = ((s.end_ms - s.start_ms) / ws.len() as u64).max(100);
        for (k, w) in ws.iter().enumerate() {
            let start = s.start_ms + k as u64 * per;
            words.push(Word {
                text: (*w).into(),
                start_ms: start,
                end_ms: start + per - 60,
                confidence: Some(0.95),
                end_source: EndSource::Model,
            });
        }
    }
    let duration_ms = words.last().map_or(1000, |w| w.end_ms + 1500);
    let mut vad: Vec<Span> = Vec::new();
    for w in &words {
        match vad.last_mut() {
            Some(l) if w.start_ms - l.end_ms < 150 => l.end_ms = w.end_ms,
            _ => vad.push(Span {
                start_ms: w.start_ms,
                end_ms: w.end_ms,
            }),
        }
    }
    Transcript {
        schema: SCHEMA.into(),
        source_blake3: HEX.into(),
        language: "en".into(),
        duration_ms,
        words,
        vad,
        model: ModelRef {
            id: "benchmark-fixture".into(),
            revision: "1".into(),
            backend: "recorded".into(),
        },
    }
}

/// Temporal intersection over union of two half-open millisecond ranges.
fn iou(a: (u64, u64), b: (u64, u64)) -> f64 {
    let inter = a.1.min(b.1).saturating_sub(a.0.max(b.0));
    let union = a.1.max(b.1).saturating_sub(a.0.min(b.0));
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// Fraction of the top `k` proposals (by rank) that hit a human label.
fn precision_at_k(out: &Proposals, labels: &[LabelSpan], k: usize) -> f64 {
    let mut ranked: Vec<&Proposal> = out.proposals.iter().collect();
    ranked.sort_by_key(|p| p.rank);
    let hits = ranked
        .iter()
        .take(k)
        .filter(|p| {
            labels
                .iter()
                .any(|l| iou((p.start_ms, p.end_ms), (l.start_ms, l.end_ms)) >= IOU_HIT)
        })
        .count();
    hits as f64 / k as f64
}

fn rank_fixture(f: &Fixture, profile: &ShortsProfile, path: Path) -> Proposals {
    let tr = build_transcript(f);
    let input = ProposeInput::new(&tr);
    match path {
        Path::Heuristic => propose_shorts(&input, profile, K).expect("heuristic ranks"),
        Path::LlmReplay => {
            let replay = Replay {
                moments: f.recorded_llm.clone(),
            };
            let out = propose_shorts_ranked(&input, profile, K, Ok(&replay)).expect("replay ranks");
            assert_eq!(
                out.ranking.method, "llm",
                "{}: the recorded LLM path fell back to the heuristic ({:?})",
                f.id, out.ranking.fallback_reason
            );
            out
        }
    }
}

/// Runs one path over all fixtures and asserts each preset mean against its floor.
fn run_benchmark(path: Path) {
    let profile = ShortsProfile::default();
    let floors = load_floors();
    let mut by_preset: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for f in load_fixtures() {
        let out = rank_fixture(&f, &profile, path);
        let p = precision_at_k(&out, &f.labels, K);
        eprintln!(
            "BENCH path={} fixture={} preset={} precision_at_{K}={p:.3}",
            path.name(),
            f.id,
            f.preset
        );
        by_preset.entry(f.preset.clone()).or_default().push(p);
    }
    for (preset, scores) in &by_preset {
        let floor = floors
            .floors
            .get(preset)
            .unwrap_or_else(|| panic!("no floor recorded for preset {preset}"));
        let want = match path {
            Path::Heuristic => floor.heuristic,
            Path::LlmReplay => floor.llm_replay,
        };
        let mean = scores.iter().sum::<f64>() / scores.len() as f64;
        eprintln!(
            "BENCH path={} preset={preset} mean_precision_at_{K}={mean:.3} floor={want:.3} fixtures={}",
            path.name(),
            scores.len()
        );
        assert!(
            mean + 1e-9 >= want,
            "selection benchmark below floor: path={} preset={preset} mean={mean:.3} floor={want:.3}",
            path.name()
        );
    }
}

#[test]
fn heuristic_ranker_meets_recorded_floor() {
    run_benchmark(Path::Heuristic);
}

#[test]
fn llm_replay_ranker_meets_recorded_floor() {
    run_benchmark(Path::LlmReplay);
}

#[test]
fn benchmark_output_is_deterministic() {
    let profile = ShortsProfile::default();
    for f in load_fixtures() {
        for path in [Path::Heuristic, Path::LlmReplay] {
            let a = serde_json::to_string(&rank_fixture(&f, &profile, path)).unwrap();
            let b = serde_json::to_string(&rank_fixture(&f, &profile, path)).unwrap();
            assert_eq!(
                a,
                b,
                "{}: {} ranking differs between runs",
                f.id,
                path.name()
            );
        }
    }
}

/// Pure arithmetic of the hit rule; no end-to-end run reaches it, so it is a unit test.
#[test]
fn iou_hit_rule_is_exact() {
    assert!((iou((0, 10_000), (5_000, 15_000)) - 5.0 / 15.0).abs() < 1e-9);
    assert_eq!(iou((0, 1_000), (2_000, 3_000)), 0.0);
    assert!(iou((0, 10_000), (0, 10_000)) >= IOU_HIT);
    assert!(iou((0, 10_000), (6_000, 16_000)) < IOU_HIT);
}
