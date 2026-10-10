# CutRight contract asks: C2 (rightkit-ort DirectML), RK9 (rightkit-compose), RK3 (rightkit-record), RK4 (rightkit-align)

Source: CutRight chat, 2026-10-11, derived from CutRight's plan R1–R17 and a read of RightKit crate source; nothing built; items marked UNVERIFIED are unmeasured. RK = rightkit/crates/, CR = cutright/crates/. Owner: RightKit lead. Ledger #35–#38.

## 1. C2 — Windows DirectML runtime + model bundle (rightkit-ort, catalog)

Exists and used: `installed_runtime_candidate` (rightkit-ort/src/lib.rs:365), `ExecutionProvider::DirectMl` + device_id + directml_static_shape_workaround + cpu_fallback (session.rs:13-18,69,76,81,102), probe_providers/directml_probe (probe.rs:28,69). CR call site: cutright-reframe/src/ort_rt.rs:130-136.

Need on `SessionOptions` (session.rs:66): `pub directml_sequential: bool` (parallel execution off) and `pub directml_memory_pattern: bool` (memory pattern off); rightkit-ocr already sets both for DirectML (rightkit-ocr/src/support/session.rs:8,50-56). Plus `pub fn verify_runtime_manifest(dir: &Path) -> Result<VerifiedRuntime, RuntimeError>` hashing every file in full.

Artifact layout, staged beside the installed exe: `runtime/onnxruntime.dll` (1.24.4 DirectML build; digest row in rightkit-media/catalog/native-libraries.json:40-52, currently `planned`), `runtime/DirectML.dll`, `runtime/models/face_detection_yunet_2023mar.onnx` (232,589 bytes, pinned in CR cutright-reframe/src/models.rs:15-16), `runtime/manifest.json` = `{schema:1, platform:"windows-x64", onnxruntime:{version,variant,catalogRow}, files:[{path,sha256,bytes}]}`.

CutRight startup gate: full-hash the manifest; configure_runtime on the bundled path only; probe reports DirectML; build YuNet with cpu_fallback OFF and require `EpReport.used == DirectMl`; run one pinned 640x640 input and compare an output digest. UNVERIFIED: whether YuNet (fixed 1x3x640x640) needs the static-shape workaround.

## 2. RK9 — rightkit-compose

Sufficient today: video/still layers with keyframed transform, crop, opacity (model.rs:124,139-160; eval.rs:367-377); cursor sprite with hotspot following source transform (model.rs:167-174,283-288); procedural backgrounds (:263); rounded/circle clip (:236-242); shadow (:130,245; media layers only); temporal blur (:254); canvas badges and captions (:178,184); click ripple/highlight (:192,211); independently timed webcam (:160-165); spring/bezier/hold easing (track.rs:22-108).

Missing (C12/C13):
- a. Time domain: `evaluate` takes u64 ms (eval.rs:122-127). CutRight's clock is 705,600,000 ticks/s (CR cutright-timeline/src/time.rs:21); NTSC frames are not integer ms. Need `evaluate(c: &Composition, t: Ticks) -> Result<FrameDescription, CompositionError>` with Timing and Keyframe.at in ticks (or a rational) and the composition carrying its fps.
- b. Retime as a piecewise time map: `speed` is a constant f64 and zero is rejected (model.rs:150, validate.rs:115). Need segments `{kind: Play{source_in, source_out, speed} | Hold{source_at, duration} | Reverse{...}}`; a hold is its own kind with no source span (R8, C13).
- c. Easing: `Easing::Cosine` and a bounce-parameterised spring matching the ScreenRight camera curve (critical: 1−(1+9.23u)e^(−9.23u); underdamped: zeta = 1−clamp(bounce,0,0.3), omega = 6.9/zeta), exact to 1e-6 against cameraConformanceVectors and no overshoot for bounce in [0,0.3]. UNVERIFIED whether the existing Spring reproduces it.
- d. One transition family between adjacent layers, a mask input, shadow on canvas layers, and audio gain/fade tracks evaluated on the same clock (captions, mic, SFX, ducking read one evaluation).
- e. A declared pixel/audio tolerance per primitive in the contract (C12 acceptance records it with the primitive).

Stays in CutRight: typed timeline actions (CR cutright-timeline/src/action.rs:17), action→Composition mapping, renderer adapters, time_map_json.

## 3. RK3 — rightkit-record bundle reader

Exists: `BundleReader::open` (reader.rs:49), `status()` (:159), `frames()` with `SourceFrame{index,sample_pts_ns,host_ns,raw_blake3}` (:18-23,:163), `step_index()` (:168), `frame_at(step_id, postcondition)` (:177-202), verify (verify.rs:53), event contract (rightkit-contracts/src/types/record_event.rs:8-56).

Need:
- `open_complete(dir, &VerifyOptions) -> Result<BundleReader>` refusing quarantined/partial bundles (open() accepts them, reader.rs:49-153; C13 must never import a partial bundle as complete).
- `scenario(&self) -> Result<ScenarioSpec>` (steps with labels and `say`).
- `events(&self) -> impl Iterator<Item = Result<RecordEvent>>`.
- `clock_map(&self) -> ClockMap` (host ns → sample PTS → encoded PTS; offset private at reader.rs:40; encoded-PTS offset written as 0 at recorder.rs:788).
- `frame_at_target(&self, step: &str, target_id: &str) -> Result<&SourceFrame>` (postconditions keyed by description text today, reader.rs:183; C13 anchors must be ids).
- `cursor_assets(&self) -> Result<Vec<CursorAsset>>` with hotspots, plus a producer (none; recorder.rs:278 takes caller-supplied dependencies); R4: the bundle renders on the other OS with no donor directory.
- `decode_frame(&self, index) -> Result<RgbaFrame>` or a stated rule that CutRight decodes clean.mp4 through its own typed ffmpeg boundary using sample_pts_ns.

File shapes relied on: clock.json `{mapping.hostToSamplePtsOffsetNs, frames[{index,hostNs,samplePtsNs,rawBlake3}]}`, events.ndjson (one RecordEvent per line), hashes.json `{status, files{blake3,bytes}}`.

## 4. RK4 — rightkit-align

Exists: `align(script, asr, designated) -> Alignment` (lib.rs:158), Anchor with lexical/timing/asr confidence, `Outcome::{Accepted{span},Rejected{reason}}`, Coverage + meets_floor (lib.rs:45-118), 50 ms tolerance (timing.rs:14-19).

Need:
- a. Identity: token_id is a whitespace index into one string (lib.rs:40-41). Need `ScriptSpec{tokens:[{word_id, step_id, token_index, text}]}` and designated anchors as `{word_id, target_id}`; every output row echoes them (R8: anchors are step_id/word_id/target_id, never label text).
- b. Units: start/end in samples plus sample_rate alongside ms.
- c. Honest timing confidence: ASR words arrive as EndSource::Model with no confidence, so timing_confidence is 1.0 for every local-route word (lib.rs:139, timing.rs:26-40). Parakeet's 80 ms lattice cannot meet 50 ms without acoustic boundary refinement; the confidence must reflect that.
- d. Clause output: `clauses[{clause_id, step_id, first_word_id, last_word_id, start, end}]`.
- e. A versioned contract in rightkit/contracts/: `{schema:"rightkit.alignment.v1", route, tokens:[{word_id, step_id, text, outcome:{status:"accepted",start_ms,end_ms,start_sample,end_sample,sample_rate}|{status:"rejected",reason}, lexical_confidence, timing_confidence, asr_confidence, asr_text}], clauses:[…], coverage:{designated,accepted,failures}, timing_tolerance_ms}`.
- f. Fixtures on real annotated audio (tests/alignment.rs uses synthetic lattice audio).
