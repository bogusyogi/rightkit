// rightkit-vad 0.1.2 consumed by exact registry pin (default features, so the Silero layer is
// compiled), outside the workspace.
//
// Proves the model-free voice-activity layers through the public API: the frame constants; the
// hysteresis, padding and merge policy in scores_to_spans; detect over a FrameScorer test double
// with its input and score refusals; chunk_spans merging, splitting and dropping; the lossless
// FrameStream carry and its failed state; energy_scores; SpanOptions and RmsCalibration resolution;
// and, from the Silero layer, the option defaults, backend names, provider support rule and the
// SpeechScorer construction paths that fail before ONNX Runtime is touched (a model that does not
// exist, and a provider this OS cannot host), with and without the energy fallback.
// Public API only.
// Does not prove: a real Silero model load, any inference, ONNX Runtime binding, CoreML or DirectML
// execution (the model file and the runtime are supplied by an app and are not in the archive), or
// live audio capture.
// State: in-memory values only; one path under the running executable that cannot exist. No
// network, no files written, no process spawn, no user data.
use rightkit_vad::{
    chunk_spans, detect, energy_scores, scores_to_spans, Backend, ChunkConfig, Error,
    ExecutionProvider, FrameScorer, FrameStream, FrameVerdicts, ResolvedSpans, RmsCalibration,
    Silero, SileroOptions, Span, SpanOptions, SpeechScorer, Spinning, VadConfig, Warmup, FRAME_MS,
    FRAME_SAMPLES, SAMPLE_RATE,
};

/// A scripted per-frame scorer: one score per frame, then a model error when the script runs out.
struct Scripted {
    scores: Vec<f32>,
    next: usize,
    resets: usize,
}

impl Scripted {
    fn new(scores: Vec<f32>) -> Self {
        Self {
            scores,
            next: 0,
            resets: 0,
        }
    }
}

impl FrameScorer for Scripted {
    fn reset(&mut self) {
        self.next = 0;
        self.resets += 1;
    }

    fn score(&mut self, _frame: &[f32; FRAME_SAMPLES]) -> rightkit_vad::Result<f32> {
        let score = *self
            .scores
            .get(self.next)
            .ok_or_else(|| Error::Model("script exhausted".into()))?;
        self.next += 1;
        Ok(score)
    }
}

/// 40 frames (1280 ms): speech in frames 5..15 and 25..30, silence elsewhere.
fn two_bursts() -> Vec<f32> {
    let mut scores = vec![0.0f32; 40];
    scores[5..15].fill(0.9);
    scores[25..30].fill(0.8);
    scores
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-6
}

fn constants_and_config() {
    assert_eq!(FRAME_SAMPLES, 512);
    assert_eq!(FRAME_MS, 32);
    assert_eq!(SAMPLE_RATE, 16_000);
    assert_eq!(
        FRAME_SAMPLES as u64 * 1000 / u64::from(SAMPLE_RATE),
        FRAME_MS
    );
    let cfg = VadConfig::default();
    assert_eq!(
        (
            cfg.threshold,
            cfg.neg_threshold,
            cfg.min_speech_ms,
            cfg.min_silence_ms,
            cfg.pad_ms
        ),
        (0.5, 0.35, 100, 120, 30)
    );
    let span = Span {
        start_ms: 1000,
        end_ms: 1500,
    };
    assert_eq!(span.len_ms(), 500);
    assert_eq!(span.sample_range(100_000), 16_000..24_000);
    assert_eq!(
        span.sample_range(20_000),
        16_000..20_000,
        "the range clamps to the buffer"
    );
    assert_eq!(span.sample_range(100), 100..100);
    assert_eq!(Error::Model("x".into()).to_string(), "vad model: x");
    assert_eq!(Error::Input("y".into()).to_string(), "vad input: y");
}

fn span_policy() {
    let cfg = VadConfig::default();
    // Speech starts at the frame that reaches the threshold and ends after min_silence of quiet;
    // padding extends both edges and the end clamps to the signal duration.
    assert_eq!(
        scores_to_spans(&two_bursts(), &cfg, 1280),
        [
            Span {
                start_ms: 130,
                end_ms: 510
            },
            Span {
                start_ms: 770,
                end_ms: 990
            }
        ]
    );
    assert_eq!(
        scores_to_spans(&two_bursts(), &cfg, 900),
        [
            Span {
                start_ms: 130,
                end_ms: 510
            },
            Span {
                start_ms: 770,
                end_ms: 900
            }
        ],
        "the last span clamps to the duration"
    );
    // Wide padding merges neighbouring spans into one.
    let wide = VadConfig { pad_ms: 200, ..cfg };
    assert_eq!(
        scores_to_spans(&two_bursts(), &wide, 1280),
        [Span {
            start_ms: 0,
            end_ms: 1160
        }]
    );
    // A dip between the two thresholds keeps the segment open; a dip shorter than min_silence too.
    let dip = [
        0.0, 0.9, 0.4, 0.4, 0.4, 0.4, 0.4, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0,
    ];
    assert_eq!(
        scores_to_spans(&dip, &cfg, 416),
        [Span {
            start_ms: 2,
            end_ms: 286
        }]
    );
    let short_gap = [
        0.9, 0.9, 0.9, 0.9, 0.0, 0.0, 0.9, 0.9, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0,
    ];
    assert_eq!(scores_to_spans(&short_gap, &cfg, 480).len(), 1);
    // Bursts shorter than min_speech are dropped; empty input has no spans.
    assert!(scores_to_spans(&[0.0, 0.0, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0], &cfg, 288).is_empty());
    assert!(scores_to_spans(&[], &cfg, 0).is_empty());
    // Speech that runs to the end of the signal still closes a span.
    assert_eq!(
        scores_to_spans(&[0.0, 0.9, 0.9, 0.9, 0.9], &cfg, 160),
        [Span {
            start_ms: 2,
            end_ms: 160
        }]
    );

    // detect resets the scorer, scores whole frames and applies the same policy.
    let mut scorer = Scripted::new(two_bursts());
    let samples = vec![0.0f32; 40 * FRAME_SAMPLES];
    assert_eq!(
        detect(&mut scorer, &samples, &cfg).unwrap(),
        scores_to_spans(&two_bursts(), &cfg, 1280)
    );
    assert_eq!(scorer.resets, 1);
    // A trailing partial frame is zero-padded and scored as one more frame.
    let mut partial = Scripted::new(vec![0.9, 0.9]);
    detect(&mut partial, &vec![0.0f32; FRAME_SAMPLES + 1], &cfg).unwrap();
    assert_eq!(partial.next, 2);
    // Refusals: inverted thresholds and non-finite samples are Input, a non-finite score is Model.
    let inverted = VadConfig {
        threshold: 0.3,
        neg_threshold: 0.5,
        ..cfg
    };
    assert!(matches!(
        detect(&mut Scripted::new(vec![]), &samples, &inverted),
        Err(Error::Input(_))
    ));
    let mut untouched = Scripted::new(vec![0.9]);
    assert!(matches!(
        detect(&mut untouched, &[f32::NAN; 4], &cfg),
        Err(Error::Input(_))
    ));
    assert_eq!(
        untouched.resets, 0,
        "bad input is refused before the scorer is touched"
    );
    for bad in [f32::NAN, f32::INFINITY] {
        let mut broken = Scripted::new(vec![bad]);
        assert!(matches!(
            detect(&mut broken, &[0.0; FRAME_SAMPLES], &cfg),
            Err(Error::Model(_))
        ));
    }
    assert!(matches!(
        detect(&mut Scripted::new(vec![]), &[0.0; FRAME_SAMPLES], &cfg),
        Err(Error::Model(_))
    ));
}

fn chunking() {
    let cfg = ChunkConfig::default();
    assert_eq!(
        (cfg.merge_gap_ms, cfg.max_chunk_ms, cfg.min_chunk_ms),
        (300, 30_000, 200)
    );
    let span = |start_ms, end_ms| Span { start_ms, end_ms };
    // Near spans merge, short chunks drop, far spans stay apart.
    assert_eq!(
        chunk_spans(
            &[
                span(0, 1000),
                span(1200, 2000),
                span(5000, 5100),
                span(9000, 9500)
            ],
            &cfg,
            None
        ),
        [span(0, 2000), span(9000, 9500)]
    );
    assert_eq!(
        chunk_spans(&[span(0, 1000), span(1300, 2000)], &cfg, None).len(),
        2,
        "a gap of exactly merge_gap_ms stays split"
    );
    // Over-long spans split at the limit.
    assert_eq!(
        chunk_spans(&[span(20_000, 80_000)], &cfg, None),
        [span(20_000, 50_000), span(50_000, 80_000)]
    );
    // With scores the split lands on the quietest frame of the last quarter of the window.
    let mut scores = vec![0.9f32; 2000];
    scores[1500] = 0.0;
    assert_eq!(
        chunk_spans(&[span(20_000, 80_000)], &cfg, Some(&scores)),
        [
            span(20_000, 48_000),
            span(48_000, 78_000),
            span(78_000, 80_000)
        ]
    );
    assert!(chunk_spans(&[], &cfg, None).is_empty());
}

fn streaming() {
    let mut stream = FrameStream::new(Scripted::new(vec![0.9, 0.1, 0.9, 0.9]), 0.5);
    assert_eq!(stream.threshold(), 0.5);
    assert_eq!(stream.aggregate(), None);
    // Less than a frame: nothing scored, nothing lost.
    assert_eq!(
        stream.push(&[0.0; 300]),
        Some(FrameVerdicts {
            scores: vec![],
            speech: vec![]
        })
    );
    assert_eq!(stream.aggregate(), None);
    // The remainder carries over: 300 + 724 samples are exactly two frames.
    assert_eq!(
        stream.push(&[0.0; 724]),
        Some(FrameVerdicts {
            scores: vec![0.9, 0.1],
            speech: vec![true, false]
        })
    );
    assert_eq!(stream.aggregate(), Some((2, 1)));
    // The threshold can change between pushes and applies to the next frame.
    stream.set_threshold(0.95);
    assert_eq!(stream.threshold(), 0.95);
    assert_eq!(
        stream.push(&[0.0; FRAME_SAMPLES]),
        Some(FrameVerdicts {
            scores: vec![0.9],
            speech: vec![false]
        })
    );
    assert_eq!(stream.aggregate(), Some((3, 1)));
    // A scorer failure is unknown, never silence, and the stream stays failed until reset.
    assert!(!stream.is_failed());
    assert_eq!(
        stream.push(&[0.0; 2 * FRAME_SAMPLES]),
        None,
        "the script holds one more score than two frames need"
    );
    assert!(stream.is_failed());
    assert_eq!(stream.push(&[0.0; FRAME_SAMPLES]), None);
    stream.reset();
    assert!(!stream.is_failed());
    assert_eq!(stream.aggregate(), None);
    assert_eq!(
        stream.push(&[0.0; FRAME_SAMPLES]).unwrap().scores,
        vec![0.9],
        "reset rewinds the scorer"
    );
    // A non-finite score fails the stream the same way.
    let mut nan = FrameStream::new(Scripted::new(vec![f32::NAN]), 0.5);
    assert_eq!(nan.push(&[0.0; FRAME_SAMPLES]), None);
    assert!(nan.is_failed());
}

fn energy_and_calibration() {
    // Ten silent frames then ten frames of a quiet tone: the energy scorer separates them with no model.
    let mut samples = vec![0.0f32; 10 * FRAME_SAMPLES];
    samples.extend((0..10 * FRAME_SAMPLES).map(|i| (i as f32 * 0.05).sin() * 0.3));
    let scores = energy_scores(&samples);
    assert_eq!(scores.len(), 20);
    assert!(scores[..10].iter().all(|s| *s == 0.0));
    assert!(scores[10..].iter().all(|s| *s == 1.0));
    assert_eq!(
        scores_to_spans(&scores, &VadConfig::default(), 640),
        [Span {
            start_ms: 290,
            end_ms: 640
        }]
    );
    assert!(energy_scores(&[]).is_empty());

    // Defaults resolve to the default config; calibration derives the threshold from ambient RMS.
    let resolved: ResolvedSpans = SpanOptions::default()
        .resolve(VadConfig::default(), None)
        .unwrap();
    assert_eq!(resolved.config, VadConfig::default());
    assert_eq!(resolved.noise_floor_rms, None);
    assert!(SpanOptions::default().cap_silence_at_calibrated);
    let calibration = RmsCalibration::default();
    assert_eq!(
        (
            calibration.multiplier,
            calibration.noise_floor,
            calibration.minimum_threshold
        ),
        (8.0, 0.005, 0.20)
    );
    let calibrated = SpanOptions {
        rms_calibration: Some(calibration),
        ..SpanOptions::default()
    };
    let ambient = vec![0.05f32; 16_000];
    let resolved = calibrated
        .resolve(VadConfig::default(), Some(&ambient))
        .unwrap();
    assert!(near(resolved.config.threshold, 0.40), "8x an RMS of 0.05");
    assert!(near(resolved.noise_floor_rms.unwrap(), 0.05));
    assert!(near(resolved.config.neg_threshold, 0.35));
    // A silent room cannot drive the threshold below the minimum; a loud one clamps to 1.
    let quiet = calibrated
        .resolve(VadConfig::default(), Some(&vec![0.001f32; 16_000]))
        .unwrap();
    assert!(near(quiet.config.threshold, 0.20) && near(quiet.noise_floor_rms.unwrap(), 0.005));
    assert!(
        near(quiet.config.neg_threshold, 0.20),
        "the silence threshold is capped at the speech threshold"
    );
    let loud = calibrated
        .resolve(VadConfig::default(), Some(&vec![0.5f32; 16_000]))
        .unwrap();
    assert_eq!(loud.config.threshold, 1.0);
    // Uncapped, a silence threshold above the calibrated speech threshold is refused, not clamped.
    let uncapped = SpanOptions {
        silence_threshold: 0.35,
        cap_silence_at_calibrated: false,
        ..calibrated
    };
    assert!(matches!(
        uncapped.resolve(VadConfig::default(), Some(&vec![0.001f32; 16_000])),
        Err(Error::Input(_))
    ));
    let low_silence = SpanOptions {
        silence_threshold: 0.15,
        ..uncapped
    };
    assert!(near(
        low_silence
            .resolve(VadConfig::default(), Some(&vec![0.001f32; 16_000]))
            .unwrap()
            .config
            .neg_threshold,
        0.15
    ));
    // Calibration needs real ambient audio; inverted thresholds are refused.
    assert!(matches!(
        calibrated.resolve(VadConfig::default(), None),
        Err(Error::Input(_))
    ));
    assert!(matches!(
        calibrated.resolve(VadConfig::default(), Some(&[])),
        Err(Error::Input(_))
    ));
    assert!(matches!(
        calibrated.resolve(VadConfig::default(), Some(&[0.1, f32::NAN])),
        Err(Error::Input(_))
    ));
    let inverted = SpanOptions {
        speech_threshold: 0.3,
        silence_threshold: 0.5,
        ..SpanOptions::default()
    };
    assert!(matches!(
        inverted.resolve(VadConfig::default(), None),
        Err(Error::Input(_))
    ));
}

fn silero_layer_without_a_model() {
    let defaults = SileroOptions::default();
    assert_eq!(
        (
            defaults.intra_threads,
            defaults.inter_threads,
            defaults.execution_provider,
            defaults.device_id,
            defaults.energy_fallback
        ),
        (1, 1, ExecutionProvider::Cpu, None, false)
    );
    assert_eq!(
        (
            defaults.warmup,
            defaults.intra_op_spinning,
            defaults.inter_op_spinning,
            defaults.own_thread_pool
        ),
        (Warmup::ResetAfter, Spinning::Off, Spinning::Off, None)
    );
    assert_eq!(ExecutionProvider::default(), ExecutionProvider::Cpu);
    assert_eq!(
        (Warmup::default(), Spinning::default()),
        (Warmup::ResetAfter, Spinning::Off)
    );
    assert!(ExecutionProvider::Cpu.supported_on_this_os());
    assert_eq!(
        ExecutionProvider::CoreMl.supported_on_this_os(),
        cfg!(target_vendor = "apple")
    );
    assert_eq!(
        ExecutionProvider::DirectMl.supported_on_this_os(),
        cfg!(target_os = "windows")
    );
    for (backend, name) in [
        (Backend::SileroCpu, "silero-cpu"),
        (Backend::SileroCoreMl, "silero-coreml"),
        (Backend::SileroDirectMl, "silero-directml"),
        (Backend::Energy, "energy-zcr-fallback"),
    ] {
        assert_eq!(backend.name(), name);
    }

    // A path that cannot exist: a child of the running executable, which is a file.
    let missing = std::env::current_exe().unwrap().join("silero.onnx");
    match Silero::load(&missing) {
        Ok(_) => panic!("a missing model must not load"),
        Err(Error::Model(message)) => assert!(message.contains("model not found"), "{message}"),
        Err(other) => panic!("model error expected, got {other}"),
    }
    match SpeechScorer::from_path(&missing, SileroOptions::default()) {
        Ok(_) => panic!("a missing model must not load without the fallback"),
        Err(Error::Model(message)) => assert!(message.contains("model not found"), "{message}"),
        Err(other) => panic!("model error expected, got {other}"),
    }

    // With the energy fallback the failed load is reported and the energy scorer takes over.
    let lenient = SileroOptions {
        energy_fallback: true,
        ..SileroOptions::default()
    };
    let (mut scorer, report) = SpeechScorer::from_path(&missing, lenient).unwrap();
    assert!(matches!(scorer, SpeechScorer::Energy));
    assert_eq!(report.backend, Backend::Energy);
    assert_eq!(scorer.backend(), Backend::Energy);
    assert!(report
        .fallback_reason
        .as_deref()
        .is_some_and(|r| r.starts_with("silero unavailable:") && r.contains("model not found")));
    let tone: Vec<f32> = (0..16_000).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
    assert_eq!(scorer.scores(&tone).unwrap(), energy_scores(&tone));
    let spans = scorer.detect(&tone, &VadConfig::default()).unwrap();
    assert_eq!(
        spans,
        scores_to_spans(&energy_scores(&tone), &VadConfig::default(), 1000)
    );
    assert!(matches!(scorer.scores(&[f32::NAN]), Err(Error::Input(_))));
    assert!(matches!(
        scorer.detect(
            &tone,
            &VadConfig {
                threshold: 0.2,
                neg_threshold: 0.9,
                ..VadConfig::default()
            }
        ),
        Err(Error::Input(_))
    ));

    // A provider this OS cannot host is refused before any load, and also falls back when allowed.
    let other = if cfg!(target_os = "windows") {
        ExecutionProvider::CoreMl
    } else {
        ExecutionProvider::DirectMl
    };
    if !other.supported_on_this_os() {
        let strict = SileroOptions {
            execution_provider: other,
            ..SileroOptions::default()
        };
        match SpeechScorer::from_path(&missing, strict) {
            Ok(_) => panic!("an unavailable provider must be refused"),
            Err(error) => assert!(error.to_string().contains("not available"), "{error}"),
        }
        let (_, report) = SpeechScorer::from_path(
            &missing,
            SileroOptions {
                energy_fallback: true,
                ..strict
            },
        )
        .unwrap();
        assert_eq!(report.backend, Backend::Energy);
        assert!(report.fallback_reason.unwrap().contains("not available"));
    }
}

fn main() {
    constants_and_config();
    span_policy();
    chunking();
    streaming();
    energy_and_calibration();
    silero_layer_without_a_model();
    println!("OK rightkit-vad 0.1.2 (registry consumer)");
}
