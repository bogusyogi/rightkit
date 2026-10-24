// rightkit-audio 0.3.1 consumed by exact registry pin (default features off, which is the filter
// graph, measurement-maths, WAV and room-tone surface that CutRight's engine, ScrapeRight's speech
// crate and GenRight use), outside the workspace.
//
// Proves, through the public API and with no FFmpeg and no audio device: the typed filter text and
// number formatting; the dialogue chain (validation of every stage, filter text, JSON defaults);
// ducking (sidechain and span forms); the dialogue-bus graph fragment; the three bundled presets
// and the preset JSON contract; the loudness gate; the level helpers; WAV reading and writing
// (PCM16, IEEE float, multichannel averaging, the 16 kHz mono contract); the mic-to-camera lag
// search; room-tone discovery, gap expansion and the graph fill fragment; the pure looped room-tone
// fill and bed; and the Display text of the error type.
// Public API only.
// Does not prove: anything that starts FFmpeg (`AudioTools` measurement and renders), the
// `capture` feature (cpal capture, the CoreAudio probe, the Windows endpoint and power lifecycle)
// or the `playback` feature (OutputSession, play_once), because they need an FFmpeg binary or a
// live audio device.
// State: a per-process directory under the OS temp directory, removed at the end. No network, no
// process spawn, no audio device, no user data.
use rightkit_audio::bus::GraphFragment;
use rightkit_audio::chain::{Compressor, DeEsser, Denoise, Expander, HighPass, Limiter};
use rightkit_audio::measure::{self, AStats, LevelMeasurement, RoomToneStep};
use rightkit_audio::mic_align::{AlignParams, MicAlignment};
use rightkit_audio::preset::PRESET_SCHEMA_VERSION;
use rightkit_audio::profile::LoudnessGateResult;
use rightkit_audio::room_tone::{expand_gaps, GapSpan, RoomToneSample};
use rightkit_audio::wav::{self, MonoAudio};
use rightkit_audio::{
    align_mic, dialogue_bus_fragment, encode_mono_f32_wav, filter, find_room_tone, rms,
    room_tone_bed, room_tone_fill, room_tone_fill_fragment, sidechain_split_fragment, AudioError,
    AudioGate, DialogueChain, Ducking, FillParams, Filter, Preset, RoomToneParams, RoomTonePolicy,
    ToneFill,
};
use std::path::{Path, PathBuf};

/// Deterministic white noise in [-amp, amp).
fn noise(n: usize, amp: f32, seed: &mut u32) -> Vec<f32> {
    (0..n)
        .map(|_| {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((*seed >> 8) as f32 / 8_388_608.0 - 1.0) * amp
        })
        .collect()
}

fn near(a: f64, b: f64, tolerance: f64) -> bool {
    (a - b).abs() <= tolerance
}

fn invalid_text(result: Result<(), AudioError>) -> String {
    match result {
        Err(AudioError::Invalid(message)) => message,
        other => panic!("expected AudioError::Invalid, got {other:?}"),
    }
}

fn filter_text() {
    assert_eq!(
        Filter::new("highpass")
            .num("f", 80.0)
            .opt("poles", 2)
            .render(),
        "highpass=f=80:poles=2"
    );
    let anull = Filter::new("anull");
    assert_eq!((anull.name(), anull.render().as_str()), ("anull", "anull"));
    // Positional values render bare and in call order, before the named ones.
    assert_eq!(
        Filter::new("atrim").pos(0).pos(12.5).render(),
        "atrim=0:12.5"
    );
    assert_eq!(
        Filter::new("apad").pos(1).opt("whole_dur", 3).render(),
        "apad=1:whole_dur=3"
    );
    assert_eq!(
        Filter::new("amix")
            .opt("inputs", 2)
            .opt("duration", "first")
            .render(),
        "amix=inputs=2:duration=first"
    );
    // Quoted options protect expression characters, including an inner quote.
    assert_eq!(filter::quote("a:b,c"), "'a:b,c'");
    assert_eq!(filter::quote("it's"), "'it'\\''s'");
    assert_eq!(
        Filter::new("volume").quoted("volume", "1-t").render(),
        "volume=volume='1-t'"
    );
    assert_eq!(filter::render_chain(&[]), "");
    assert_eq!(
        filter::render_chain(&[Filter::new("a").opt("x", 1), Filter::new("b")]),
        "a=x=1,b"
    );
    assert_eq!(Filter::new("a"), Filter::new("a"));
    assert_ne!(Filter::new("a"), Filter::new("b"));

    // Numbers lose trailing zeros and never print a negative zero.
    for (value, text) in [
        (80.0, "80"),
        (100.0, "100"),
        (0.25, "0.25"),
        (-18.0, "-18"),
        (0.0, "0"),
        (-0.0, "0"),
        (1e-9, "0"),
        (-1e-9, "0"),
        (0.123_456_7, "0.123457"),
        (-12.5, "-12.5"),
    ] {
        assert_eq!(filter::num(value), text, "{value}");
    }
    assert_eq!(filter::db_to_linear(0.0), 1.0);
    assert!(near(filter::db_to_linear(20.0), 10.0, 1e-9));
    assert!(near(filter::db_to_linear(-20.0), 0.1, 1e-12));
    assert!(near(filter::db_to_linear(-6.0), 0.501_187_233_6, 1e-9));
}

fn podcast_chain() -> DialogueChain {
    Preset::builtin("podcast").expect("podcast preset").chain
}

fn dialogue_chain() {
    // The podcast preset, stage by stage and as one -af string.
    let chain = podcast_chain();
    chain.validate().expect("the bundled chain is valid");
    let texts: Vec<String> = chain.filters().iter().map(Filter::render).collect();
    assert_eq!(
        texts,
        [
            "highpass=f=80:poles=2",
            "afftdn=nr=10:nf=-50:rf=-60:ad=0.5:tn=1",
            "acompressor=threshold=-20dB:ratio=3:attack=15:release=120:makeup=1.412538",
            "deesser=i=0.3:m=0.5:f=0.5",
            "alimiter=limit=0.891251:attack=5:release=50:level=0:latency=1",
        ]
    );
    assert_eq!(chain.af(), texts.join(","));
    assert_eq!(chain.conditioning_filters().len(), 2);
    assert_eq!(chain.dynamics_filters().len(), 2);
    assert_eq!(
        chain.limiter_filter().render(),
        "alimiter=limit=0.891251:attack=5:release=50:level=0:latency=1"
    );

    // The expander leads the dynamics (talking-head), as a gate with a linear range.
    let head = Preset::builtin("talking-head").unwrap().chain;
    assert_eq!(
        head.dynamics_filters()[0].render(),
        "agate=threshold=-40dB:ratio=8:attack=5:release=120:range=0.1"
    );
    assert_eq!(head.dynamics_filters().len(), 3);

    // Every stage can be absent; only the limiter is required.
    let bare = DialogueChain {
        high_pass: None,
        denoise: Denoise::None,
        expander: None,
        compressor: None,
        deesser: None,
        limiter: Limiter {
            ceiling_db: 0.0,
            attack_ms: 5.0,
            release_ms: 50.0,
        },
    };
    bare.validate().unwrap();
    assert!(bare.conditioning_filters().is_empty() && bare.dynamics_filters().is_empty());
    assert_eq!(
        bare.af(),
        "alimiter=limit=1:attack=5:release=50:level=0:latency=1"
    );
    // A very low ceiling is clamped to the limiter's floor of 0.0625 (-24 dB).
    let floor = DialogueChain {
        limiter: Limiter {
            ceiling_db: -24.0,
            ..bare.limiter
        },
        ..bare.clone()
    };
    floor.validate().unwrap();
    assert_eq!(
        floor.limiter_filter().render(),
        "alimiter=limit=0.063096:attack=5:release=50:level=0:latency=1"
    );

    // The other denoisers render their own options.
    let anlmdn = DialogueChain {
        denoise: Denoise::Anlmdn { strength: 0.0001 },
        ..bare.clone()
    };
    anlmdn.validate().unwrap();
    assert_eq!(anlmdn.conditioning_filters()[0].render(), "anlmdn=s=0.0001");

    // Validation names the stage and the range in the error text.
    let mutate = |change: &dyn Fn(&mut DialogueChain)| {
        let mut candidate = podcast_chain();
        change(&mut candidate);
        invalid_text(candidate.validate())
    };
    assert_eq!(
        mutate(&|c| c.high_pass = Some(HighPass { hz: 10.0, poles: 2 })),
        "high_pass.hz = 10 outside 20..=500"
    );
    assert_eq!(
        mutate(&|c| c.high_pass = Some(HighPass { hz: 80.0, poles: 3 })),
        "high_pass.poles must be 1 or 2"
    );
    assert!(mutate(&|c| c.limiter.ceiling_db = 1.0).contains("limiter.ceiling_db = 1 outside"));
    assert!(mutate(&|c| c.limiter.ceiling_db = f64::NAN).contains("limiter.ceiling_db = NaN"));
    assert!(mutate(&|c| c.limiter.attack_ms = 0.0).contains("limiter.attack_ms"));
    assert!(mutate(&|c| c.limiter.release_ms = 9000.0).contains("limiter.release_ms"));
    assert!(mutate(&|c| {
        c.compressor = Some(Compressor {
            threshold_db: -20.0,
            ratio: 0.5,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 0.0,
        })
    })
    .contains("compressor.ratio"));
    assert!(mutate(&|c| {
        c.deesser = Some(DeEsser {
            intensity: 1.5,
            max_reduction: 0.5,
            frequency: 0.5,
        })
    })
    .contains("deesser.intensity"));
    assert!(mutate(&|c| {
        c.expander = Some(Expander {
            threshold_db: 5.0,
            ratio: 2.0,
            attack_ms: 5.0,
            release_ms: 100.0,
            range: 0.1,
        })
    })
    .contains("expander.threshold_db"));
    assert!(mutate(&|c| {
        c.denoise = Denoise::Afftdn {
            reduction_db: 10.0,
            noise_floor_db: -10.0,
            residual_floor_db: -60.0,
            adaptivity: 0.5,
            track: true,
        }
    })
    .contains("denoise.noise_floor_db"));
    assert!(mutate(&|c| c.denoise = Denoise::Anlmdn { strength: 0.0 }).contains("denoise.strength"));
}

fn dialogue_chain_json_and_model_files(scratch: &Path) {
    // Omitted fields take the documented defaults.
    let json = r#"{
        "high_pass": {"hz": 90},
        "denoise": {"kind": "afftdn", "reduction_db": 12, "noise_floor_db": -50, "track": false},
        "compressor": null,
        "deesser": null,
        "limiter": {"ceiling_db": -1}
    }"#;
    let chain: DialogueChain = serde_json::from_str(json).unwrap();
    assert_eq!(chain.high_pass, Some(HighPass { hz: 90.0, poles: 2 }));
    assert_eq!(
        chain.denoise,
        Denoise::Afftdn {
            reduction_db: 12.0,
            noise_floor_db: -50.0,
            residual_floor_db: -38.0,
            adaptivity: 0.5,
            track: false,
        }
    );
    assert_eq!(chain.expander, None);
    assert_eq!(
        (chain.limiter.attack_ms, chain.limiter.release_ms),
        (5.0, 50.0)
    );
    chain.validate().unwrap();
    assert_eq!(
        chain.conditioning_filters()[1].render(),
        "afftdn=nr=12:nf=-50:rf=-38:ad=0.5:tn=0"
    );
    let again: DialogueChain =
        serde_json::from_str(&serde_json::to_string(&chain).unwrap()).unwrap();
    assert_eq!(again, chain);
    assert_eq!(
        serde_json::to_value(Denoise::None).unwrap(),
        serde_json::json!({"kind": "none"})
    );
    assert_eq!(
        serde_json::to_value(Denoise::Anlmdn { strength: 2.0 }).unwrap(),
        serde_json::json!({"kind": "anlmdn", "strength": 2.0})
    );

    // RNNoise needs a model file the caller supplies: a missing file is refused, a real one
    // renders with forward slashes and a quoted path.
    let model = scratch.join("voice.rnnn");
    let rnn = |mix: f64| DialogueChain {
        denoise: Denoise::Arnndn {
            model: model.to_string_lossy().into_owned(),
            mix,
        },
        ..chain.clone()
    };
    match rnn(0.5).validate() {
        Err(AudioError::MissingInput(path)) => assert_eq!(path, model),
        other => panic!("expected MissingInput, got {other:?}"),
    }
    std::fs::write(&model, b"model bytes are not read").unwrap();
    rnn(0.5).validate().unwrap();
    assert!(invalid_text(rnn(2.0).validate()).contains("denoise.mix"));
    let expected = format!(
        "arnndn=m='{}':mix=0.5",
        model.to_string_lossy().replace('\\', "/")
    );
    assert_eq!(rnn(0.5).conditioning_filters()[1].render(), expected);
}

fn dialogue_bus() {
    let chain = podcast_chain();
    // Without a room-tone layer the fragment is the whole chain between two labels.
    let plain: GraphFragment = dialogue_bus_fragment(&chain, "dlg", None, "out", "p").unwrap();
    assert_eq!(plain, format!("[dlg]{}[out]", chain.af()));
    // With a layer, the tone is mixed before the limiter so the ceiling holds for the sum.
    let mut pre = chain.conditioning_filters();
    pre.extend(chain.dynamics_filters());
    let mixed = dialogue_bus_fragment(&chain, "dlg", Some("tone"), "out", "p").unwrap();
    assert_eq!(
        mixed,
        format!(
            "[dlg]{}[p_d];[p_d][tone]amix=inputs=2:normalize=0:duration=first[p_m];[p_m]{}[out]",
            filter::render_chain(&pre),
            chain.limiter_filter().render()
        )
    );
    // A chain with nothing before the limiter still needs a pass-through for the mix.
    let bare = DialogueChain {
        high_pass: None,
        denoise: Denoise::None,
        expander: None,
        compressor: None,
        deesser: None,
        limiter: chain.limiter,
    };
    let passthrough = dialogue_bus_fragment(&bare, "dlg", Some("tone"), "out", "q").unwrap();
    assert!(
        passthrough.starts_with("[dlg]anull[q_d];[q_d][tone]amix="),
        "{passthrough}"
    );
    // An invalid chain is refused before any text is produced.
    let mut broken = chain;
    broken.limiter.ceiling_db = 3.0;
    assert!(matches!(
        dialogue_bus_fragment(&broken, "dlg", None, "out", "p"),
        Err(AudioError::Invalid(_))
    ));
}

fn ducking_forms() {
    let sidechain = Ducking::Sidechain {
        threshold_db: -38.0,
        ratio: 6.0,
        attack_ms: 20.0,
        release_ms: 350.0,
        makeup_db: 0.0,
    };
    sidechain.validate().unwrap();
    assert_eq!(
        sidechain.fragment("music", Some("key"), &[], "ducked").unwrap(),
        "[music][key]sidechaincompress=threshold=0.012589:ratio=6:attack=20:release=350:makeup=1[ducked]"
    );
    assert_eq!(
        invalid_text(sidechain.fragment("music", None, &[], "ducked").map(|_| ())),
        "sidechain ducking needs a dialogue key label"
    );
    assert_eq!(
        sidechain_split_fragment("dlg", "main", "key"),
        "[dlg]asplit=2[main][key]"
    );

    // Span ducking follows speech spans with smooth ramps; ramps that would overlap are merged.
    let spans = Ducking::Spans {
        duck_db: -14.0,
        fade_ms: 250,
    };
    spans.validate().unwrap();
    let one = "volume=volume='1-0.800474*(clip(min((t-0.75)/0.25,(2.25-t)/0.25),0,1))':eval=frame";
    assert_eq!(
        spans
            .fragment("music", Some("ignored"), &[(1000, 2000)], "out")
            .unwrap(),
        format!("[music]{one}[out]")
    );
    assert_eq!(
        rightkit_audio::ducking::spans_volume_filter(&[(1000, 2000)], -14.0, 250).render(),
        one
    );
    let render = |spans: &[(i64, i64)]| {
        rightkit_audio::ducking::spans_volume_filter(spans, -14.0, 250).render()
    };
    assert_eq!(
        render(&[(0, 1000), (1200, 2000)]),
        render(&[(0, 2000)]),
        "spans closer than two ramps merge"
    );
    assert_eq!(
        render(&[(1200, 2000), (0, 1000)]),
        render(&[(0, 2000)]),
        "input order does not matter"
    );
    assert_eq!(
        render(&[(0, 1000), (3000, 4000)]).matches("clip(").count(),
        2
    );
    assert!(render(&[(0, 1000), (3000, 4000)]).contains(")+clip("));
    assert_eq!(render(&[]), "volume=volume=1");
    assert_eq!(
        render(&[(500, 500), (700, 600)]),
        "volume=volume=1",
        "empty and inverted spans drop"
    );

    // Ranges are enforced for both forms.
    assert!(invalid_text(
        Ducking::Spans {
            duck_db: 3.0,
            fade_ms: 250
        }
        .validate()
    )
    .contains("ducking.duck_db"));
    assert!(invalid_text(
        Ducking::Spans {
            duck_db: -14.0,
            fade_ms: 0
        }
        .validate()
    )
    .contains("ducking.fade_ms"));
    assert!(invalid_text(
        Ducking::Sidechain {
            threshold_db: -38.0,
            ratio: 0.5,
            attack_ms: 20.0,
            release_ms: 350.0,
            makeup_db: 0.0
        }
        .validate()
    )
    .contains("ducking.ratio"));
    assert!(spans.fragment("m", None, &[], "o").is_ok());
    assert_eq!(
        serde_json::to_value(&spans).unwrap(),
        serde_json::json!({"mode": "spans", "duck_db": -14.0, "fade_ms": 250})
    );
}

fn presets() {
    assert_eq!(PRESET_SCHEMA_VERSION, 1);
    assert_eq!(
        Preset::builtin_names(),
        ["podcast", "talking-head", "noisy-room"]
    );
    for name in Preset::builtin_names() {
        let preset = Preset::builtin(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(preset.name, name);
        assert_eq!(preset.schema_version, PRESET_SCHEMA_VERSION);
        assert!(!preset.description.is_empty());
        preset.validate().unwrap();
        // The JSON a project would save is read back to an equal value.
        let saved = preset.to_json().unwrap();
        assert_eq!(Preset::from_json(&saved).unwrap(), preset, "{name}");
        // Each preset builds a complete chain and a ducking fragment.
        assert!(preset.chain.af().starts_with("highpass="), "{name}");
        assert!(preset.chain.af().ends_with("latency=1"), "{name}");
        preset
            .ducking
            .fragment("music", Some("key"), &[(0, 1000)], "out")
            .unwrap();
    }
    let podcast = Preset::builtin("podcast").unwrap();
    assert_eq!(podcast.gate.target_integrated_lufs, -16.0);
    assert!(matches!(podcast.ducking, Ducking::Sidechain { .. }));
    assert!(podcast.room_tone.enabled && podcast.room_tone.overlap_ms == 80);
    assert_eq!(podcast.room_tone.params, RoomToneParams::default());
    let head = Preset::builtin("talking-head").unwrap();
    assert!(head.chain.expander.is_some() && head.gate.target_true_peak_dbtp == -1.5);
    let noisy = Preset::builtin("noisy-room").unwrap();
    assert!(matches!(noisy.ducking, Ducking::Spans { fade_ms: 250, .. }));
    assert_eq!(noisy.room_tone.params.max_level_db, -25.0);
    assert_eq!(noisy.room_tone.overlap_ms, 100);

    // Refusals: unknown name, wrong schema version, malformed JSON, a stage out of range, and
    // inconsistent room-tone parameters.
    assert_eq!(
        invalid_text(Preset::builtin("nope").map(|_| ())),
        "unknown audio preset \"nope\""
    );
    let saved: serde_json::Value = serde_json::from_str(&podcast.to_json().unwrap()).unwrap();
    let edited = |change: &dyn Fn(&mut serde_json::Value)| {
        let mut value = saved.clone();
        change(&mut value);
        Preset::from_json(&value.to_string()).map(|_| ())
    };
    assert_eq!(
        invalid_text(edited(&|v| v["schema_version"] = serde_json::json!(2))),
        "unsupported preset schema_version 2"
    );
    match Preset::from_json("{not json") {
        Err(error @ AudioError::Preset(_)) => {
            assert!(error.to_string().starts_with("preset JSON: "), "{error}")
        }
        other => panic!("expected AudioError::Preset, got {other:?}"),
    }
    assert!(matches!(
        edited(&|v| v["name"] = serde_json::json!(7)),
        Err(AudioError::Preset(_))
    ));
    assert!(invalid_text(edited(
        &|v| v["chain"]["limiter"]["ceiling_db"] = serde_json::json!(5.0)
    ))
    .contains("limiter.ceiling_db"));
    assert_eq!(
        invalid_text(edited(
            &|v| v["room_tone"]["params"]["frame_ms"] = serde_json::json!(0)
        )),
        "room tone params inconsistent"
    );
    // Room-tone policy defaults apply when fields are omitted.
    let policy: RoomTonePolicy = serde_json::from_str(r#"{"enabled": true}"#).unwrap();
    assert_eq!(
        (policy.gain_db, policy.overlap_ms, policy.params),
        (0.0, 80, RoomToneParams::default())
    );
}

fn loudness_gate_and_levels() {
    let gate = AudioGate::default();
    assert_eq!(
        (
            gate.target_integrated_lufs,
            gate.loudness_tolerance_lu,
            gate.target_true_peak_dbtp,
            gate.true_peak_tolerance_db
        ),
        (-14.0, 1.0, -1.0, 0.5)
    );
    let level = |lufs, peak, clipped| LevelMeasurement {
        integrated_lufs: lufs,
        true_peak_dbtp: peak,
        clipped_samples: clipped,
    };
    let pass: LoudnessGateResult = gate.evaluate(&level(Some(-14.4), Some(-1.2), 0));
    assert!(pass.passed && pass.failures.is_empty());
    assert_eq!(
        (
            pass.integrated_lufs,
            pass.true_peak_dbtp,
            pass.clipped_samples
        ),
        (Some(-14.4), Some(-1.2), 0)
    );
    // The tolerances are inclusive.
    assert!(gate.evaluate(&level(Some(-15.0), Some(-0.5), 0)).passed);
    assert!(gate.evaluate(&level(Some(-13.0), Some(-0.5), 0)).passed);
    assert!(!gate.evaluate(&level(Some(-15.01), Some(-0.5), 0)).passed);
    let loud = gate.evaluate(&level(Some(-12.0), Some(0.0), 3));
    assert!(!loud.passed);
    assert_eq!(
        loud.failures,
        [
            "integrated loudness -12.00 LUFS is outside -14.00 +/- 1.00 LU",
            "true peak 0.00 dBTP exceeds the -1.00 dBTP ceiling (+0.50 dB tolerance)",
            "3 clipped samples detected; none are permitted",
        ]
    );
    let unmeasured = gate.evaluate(&level(None, None, 0));
    assert_eq!(
        unmeasured.failures,
        [
            "integrated loudness could not be measured",
            "true peak could not be measured"
        ]
    );
    assert_eq!(
        serde_json::to_value(&unmeasured).unwrap()["passed"],
        serde_json::json!(false)
    );

    // Level helpers over samples.
    assert_eq!(measure::rms_db(&[]), f64::NEG_INFINITY);
    assert!(near(measure::rms_db(&[0.5; 100]), -6.020_599_913, 1e-6));
    assert!(near(
        measure::peak_db(&[0.0, -0.5, 0.25]),
        -6.020_599_913,
        1e-6
    ));
    assert!(near(measure::peak_db(&[1.0]), 0.0, 1e-9));
    assert!(near(measure::peak_db(&[]), -240.0, 1e-6));
    assert_eq!(measure::CLIP_THRESHOLD, 0.999);
    // The measurement records are plain values a host fills from its own FFmpeg run.
    let stats = AStats {
        rms_db: -20.0,
        peak_db: -3.0,
        noise_floor_db: Some(-60.0),
        rms_trough_db: None,
    };
    assert_eq!(stats, stats);
    let step = RoomToneStep {
        before_mean_db: Some(-60.0),
        after_mean_db: Some(-58.0),
        step_db: Some(2.0),
    };
    assert_eq!(step.step_db, Some(2.0));
}

fn pcm16_wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data = samples.len() as u32 * 2;
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

fn wav_files(scratch: &Path) {
    // PCM16 write: clamped, scaled by 32767, rounded half away from zero; read back as f32.
    let path = scratch.join("mono.wav");
    wav::write_wav_mono_i16(&path, 16_000, &[0.0, 0.5, -0.5, 1.0, -1.0, 2.0, -2.0]).unwrap();
    let audio: MonoAudio = wav::read_wav_mono_f32(&path).unwrap();
    assert_eq!(audio.sample_rate, 16_000);
    let scale = 32_767.0f32;
    assert_eq!(
        audio.samples,
        [
            0.0,
            16_384.0 / scale,
            -16_384.0 / scale,
            1.0,
            -1.0,
            1.0,
            -1.0
        ]
    );
    assert_eq!(wav::read_wav_16k_mono(&path).unwrap(), audio.samples);

    // An empty file is valid and empty.
    let empty = scratch.join("empty.wav");
    wav::write_wav_mono_i16(&empty, 16_000, &[]).unwrap();
    assert!(wav::read_wav_16k_mono(&empty).unwrap().is_empty());

    // IEEE float input is read exactly; the encoder's header is the 44-byte canonical one.
    let samples = [0.25f32, -0.125, 0.5, 0.0, -1.0];
    let bytes = encode_mono_f32_wav(&samples, 48_000);
    assert_eq!(bytes.len(), 44 + 4 * samples.len());
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize,
        36 + 4 * samples.len()
    );
    assert_eq!(&bytes[8..16], b"WAVEfmt ");
    assert_eq!(
        u16::from_le_bytes(bytes[20..22].try_into().unwrap()),
        3,
        "IEEE float"
    );
    assert_eq!(
        u16::from_le_bytes(bytes[22..24].try_into().unwrap()),
        1,
        "mono"
    );
    assert_eq!(
        u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
        48_000
    );
    assert_eq!(&bytes[36..40], b"data");
    let float = scratch.join("float.wav");
    std::fs::write(&float, &bytes).unwrap();
    assert_eq!(
        wav::read_wav_mono_f32(&float).unwrap(),
        MonoAudio {
            sample_rate: 48_000,
            samples: samples.to_vec()
        }
    );

    // Multichannel input is averaged; the 16 kHz mono contract refuses anything else.
    let stereo = scratch.join("stereo.wav");
    std::fs::write(
        &stereo,
        pcm16_wav(16_000, 2, &[32_767, 0, -32_767, 32_767, 16_384, 16_384]),
    )
    .unwrap();
    let mixed = wav::read_wav_mono_f32(&stereo).unwrap();
    assert_eq!(mixed.sample_rate, 16_000);
    assert_eq!(mixed.samples.len(), 3);
    assert_eq!(mixed.samples[0], 0.5);
    assert_eq!(mixed.samples[1], 0.0);
    assert!(near(f64::from(mixed.samples[2]), 16_384.0 / 32_767.0, 1e-6));
    match wav::read_wav_16k_mono(&stereo) {
        Err(AudioError::Wav(message)) => {
            assert_eq!(message, "expected 16 kHz mono, found 16000 Hz x2")
        }
        other => panic!("expected a Wav error, got {other:?}"),
    }
    let cd = scratch.join("cd.wav");
    wav::write_wav_mono_i16(&cd, 44_100, &[0.0, 0.1]).unwrap();
    match wav::read_wav_16k_mono(&cd) {
        Err(AudioError::Wav(message)) => {
            assert_eq!(message, "expected 16 kHz mono, found 44100 Hz x1")
        }
        other => panic!("expected a Wav error, got {other:?}"),
    }
    assert_eq!(wav::read_wav_mono_f32(&cd).unwrap().sample_rate, 44_100);

    // Missing and malformed files are Wav errors, not panics.
    assert!(matches!(
        wav::read_wav_mono_f32(&scratch.join("absent.wav")),
        Err(AudioError::Wav(_))
    ));
    let junk = scratch.join("junk.wav");
    std::fs::write(&junk, b"this is not a wave file at all").unwrap();
    assert!(matches!(
        wav::read_wav_mono_f32(&junk),
        Err(AudioError::Wav(_))
    ));
    assert!(matches!(
        wav::read_wav_16k_mono(&junk),
        Err(AudioError::Wav(_))
    ));
    assert!(matches!(
        wav::write_wav_mono_i16(&scratch.join("no-such-dir").join("x.wav"), 16_000, &[0.0]),
        Err(AudioError::Wav(_))
    ));
}

/// The camera signal, and a mic recording of the same content `lag` samples later with
/// independent noise at 10% of the content level.
fn camera_and_mic(rate: u32, seconds: f64, lag: i64, seed: u32) -> (Vec<f32>, Vec<f32>) {
    let n = (f64::from(rate) * seconds) as usize;
    let mut state = seed;
    let camera = noise(n, 0.3, &mut state);
    let mut mic: Vec<f32> = (0..n)
        .map(|j| {
            let i = j as i64 - lag;
            if i >= 0 && (i as usize) < n {
                camera[i as usize]
            } else {
                0.0
            }
        })
        .collect();
    for (m, e) in mic.iter_mut().zip(noise(n, 0.03, &mut state)) {
        *m += e;
    }
    (camera, mic)
}

fn mic_alignment() {
    let defaults = AlignParams::default();
    assert_eq!(
        (defaults.max_lag_ms, defaults.window_ms, defaults.min_ncc),
        (1000.0, 10_000.0, 0.5)
    );
    let params = |max_lag_ms, window_ms| AlignParams {
        max_lag_ms,
        window_ms,
        min_ncc: 0.5,
    };

    // The mic holds the camera's content 37 samples (2.3125 ms) later.
    let (camera, mic) = camera_and_mic(16_000, 3.0, 37, 11);
    let found: MicAlignment = align_mic(&camera, &mic, 16_000, &params(100.0, 2000.0)).unwrap();
    assert_eq!(found.lag_samples, 37);
    assert!(near(found.offset_ms, 2.3125, 1e-9));
    assert!(found.confidence > 0.9 && found.margin > 0.3, "{found:?}");
    // A negative lag means the mic started first.
    let (camera, mic) = camera_and_mic(48_000, 2.0, -72, 23);
    let found = align_mic(&camera, &mic, 48_000, &params(50.0, 1000.0)).unwrap();
    assert_eq!(found.lag_samples, -72);
    assert!(near(found.offset_ms, -1.5, 1e-9));
    // No offset is found as zero, and a zero search range can only ever report zero.
    let (camera, mic) = camera_and_mic(16_000, 2.0, 0, 5);
    assert_eq!(
        align_mic(&camera, &mic, 16_000, &params(50.0, 1500.0))
            .unwrap()
            .lag_samples,
        0
    );
    let (camera, mic) = camera_and_mic(16_000, 2.0, 2, 17);
    let pinned = AlignParams {
        max_lag_ms: 0.0,
        window_ms: 1500.0,
        min_ncc: 0.0,
    };
    match align_mic(&camera, &mic, 16_000, &pinned) {
        Ok(found) => assert_eq!(found.lag_samples, 0),
        Err(error) => assert!(matches!(error, AudioError::Invalid(_))),
    }

    // Refusals are all Invalid: unrelated audio, a silent camera, short input, a zero rate and
    // parameters out of range.
    let (mut one, mut two) = (101, 202);
    let unrelated = (noise(32_000, 0.3, &mut one), noise(32_000, 0.3, &mut two));
    assert!(matches!(
        align_mic(&unrelated.0, &unrelated.1, 16_000, &params(50.0, 1500.0)),
        Err(AudioError::Invalid(_))
    ));
    let silent = vec![0.0f32; 16_000];
    assert!(matches!(
        align_mic(&silent, &mic, 16_000, &defaults),
        Err(AudioError::Invalid(_))
    ));
    assert!(matches!(
        align_mic(&camera[..32], &mic, 16_000, &defaults),
        Err(AudioError::Invalid(_))
    ));
    assert!(matches!(
        align_mic(&camera, &mic[..32], 16_000, &defaults),
        Err(AudioError::Invalid(_))
    ));
    assert!(matches!(
        align_mic(&camera, &mic, 0, &defaults),
        Err(AudioError::Invalid(_))
    ));
    for bad in [
        AlignParams {
            min_ncc: 1.5,
            ..defaults
        },
        AlignParams {
            window_ms: 0.0,
            ..defaults
        },
        AlignParams {
            max_lag_ms: f64::NAN,
            ..defaults
        },
        AlignParams {
            max_lag_ms: -1.0,
            ..defaults
        },
    ] {
        assert!(
            matches!(
                align_mic(&camera, &mic, 16_000, &bad),
                Err(AudioError::Invalid(_))
            ),
            "{bad:?}"
        );
    }
}

/// 3 s at 16 kHz: speech-level noise, one second of room tone, speech-level noise.
fn tone_in_the_middle(quiet_ms: usize) -> Vec<f32> {
    let mut seed = 3;
    let mut samples = noise(16_000, 0.3, &mut seed);
    samples.extend(noise(16 * quiet_ms, 0.002, &mut seed));
    samples.extend(noise(16_000, 0.3, &mut seed));
    samples
}

fn room_tone_discovery() {
    let defaults = RoomToneParams::default();
    assert_eq!(
        (
            defaults.frame_ms,
            defaults.min_len_ms,
            defaults.fallback_min_len_ms,
            defaults.max_len_ms,
            defaults.max_spread_db,
            defaults.max_level_db,
            defaults.speech_margin_ms,
            defaults.envelope_speech_db
        ),
        (20, 400, 250, 2000, 6.0, -35.0, 80, 12.0)
    );
    defaults.validate().unwrap();
    for broken in [
        RoomToneParams {
            frame_ms: 0,
            ..defaults.clone()
        },
        RoomToneParams {
            fallback_min_len_ms: 30,
            ..defaults.clone()
        },
        RoomToneParams {
            min_len_ms: 200,
            ..defaults.clone()
        },
        RoomToneParams {
            max_len_ms: 300,
            ..defaults.clone()
        },
        RoomToneParams {
            max_level_db: f64::NAN,
            ..defaults.clone()
        },
        RoomToneParams {
            speech_margin_ms: -1,
            ..defaults.clone()
        },
    ] {
        assert_eq!(
            invalid_text(broken.validate()),
            "room tone params inconsistent"
        );
        assert!(find_room_tone(&[0.0; 16_000], 16_000, &[], &broken).is_err());
    }

    // With VAD spans: the quiet second, minus 80 ms either side, is the window.
    let samples = tone_in_the_middle(1000);
    let spans = [(0, 1000), (2000, 3000)];
    let sample: RoomToneSample = find_room_tone(&samples, 16_000, &spans, &defaults).unwrap();
    assert_eq!((sample.start_ms, sample.duration_ms), (1080, 840));
    assert!(near(sample.rms_db, -58.8, 1.0), "{sample:?}");
    // Without spans, the envelope test finds the same window.
    assert_eq!(
        find_room_tone(&samples, 16_000, &[], &defaults).unwrap(),
        sample
    );
    // A longer window is capped at max_len_ms.
    let long = tone_in_the_middle(3000);
    let long_spans = [(0, 1000), (4000, 5000)];
    let capped = find_room_tone(&long, 16_000, &long_spans, &defaults).unwrap();
    assert_eq!(capped.duration_ms, 2000);
    // Under min_len_ms but over the fallback length is still accepted.
    let short = tone_in_the_middle(500);
    let short_spans = [(0, 1000), (1500, 2500)];
    assert_eq!(
        find_room_tone(&short, 16_000, &short_spans, &defaults)
            .unwrap()
            .duration_ms,
        340
    );

    // No quiet window, audio shorter than one frame, and a quiet window that is too loud.
    let mut seed = 9;
    let loud = noise(48_000, 0.3, &mut seed);
    assert_eq!(
        match find_room_tone(&loud, 16_000, &[(0, 3000)], &defaults) {
            Err(AudioError::NoRoomTone(message)) => message,
            other => panic!("expected NoRoomTone, got {other:?}"),
        },
        "no stationary window <= -35 dBFS of at least 250 ms outside speech"
    );
    assert_eq!(
        match find_room_tone(&[0.1; 100], 16_000, &[], &defaults) {
            Err(AudioError::NoRoomTone(message)) => message,
            other => panic!("expected NoRoomTone, got {other:?}"),
        },
        "audio is too short"
    );
    let strict = RoomToneParams {
        max_level_db: -80.0,
        ..defaults.clone()
    };
    assert!(matches!(
        find_room_tone(&samples, 16_000, &spans, &strict),
        Err(AudioError::NoRoomTone(_))
    ));
}

fn room_tone_gaps_and_fragment() {
    let gap = |start_ms, end_ms| GapSpan { start_ms, end_ms };
    // Gaps grow by the overlap, merge when they touch, drop when empty and clamp to the timeline.
    assert_eq!(
        expand_gaps(&[gap(2100, 3000), gap(1000, 2000), gap(500, 500)], 80, None),
        [gap(920, 3080)]
    );
    assert_eq!(
        expand_gaps(&[gap(1000, 2000), gap(2100, 3000)], 80, Some(3000)),
        [gap(920, 3000)]
    );
    assert_eq!(expand_gaps(&[gap(30, 100)], 80, None), [gap(0, 180)]);
    assert_eq!(
        expand_gaps(&[gap(1000, 2000), gap(3000, 4000)], 80, None).len(),
        2
    );
    assert!(expand_gaps(&[], 80, None).is_empty());

    let fragment = room_tone_fill_fragment(
        "tone",
        500,
        48_000,
        &[gap(1000, 2000)],
        -3.0,
        100,
        5000,
        "rt",
        "bed",
    )
    .unwrap();
    assert_eq!(
        fragment.split(';').collect::<Vec<_>>(),
        [
            "[tone]aformat=sample_rates=48000:channel_layouts=mono,asplit=2[rt_a][rt_b]",
            "[rt_b]areverse[rt_r]",
            "[rt_a][rt_r]concat=n=2:v=0:a=1[rt_unit]",
            "[rt_unit]aloop=loop=-1:size=48000:start=0,asplit=1[rt_l0]",
            "[rt_l0]atrim=start=0:end=1.2,asetpts=PTS-STARTPTS,afade=t=in:d=0.1,afade=t=out:st=1.1:d=0.1,volume=volume=-3dB,adelay=delays=900:all=1[rt_g0]",
            "[rt_g0]amix=inputs=1:normalize=0:duration=longest,apad=whole_dur=5,atrim=end=5[bed]",
        ]
    );
    // Several gaps each read a different offset of the loop and are mixed back together.
    let two = room_tone_fill_fragment(
        "tone",
        500,
        48_000,
        &[gap(1000, 2000), gap(3000, 4000)],
        0.0,
        100,
        5000,
        "rt",
        "bed",
    )
    .unwrap();
    assert!(two.contains("asplit=2[rt_l0][rt_l1]"), "{two}");
    assert!(two.contains("[rt_l1]atrim=start=0.733:end=1.933,"), "{two}");
    assert!(two.contains("[rt_g0][rt_g1]amix=inputs=2:"), "{two}");
    for (tone_ms, overlap, gaps, text) in [
        (50, 100, vec![gap(0, 1000)], "room tone sample under 100 ms"),
        (
            500,
            -1,
            vec![gap(0, 1000)],
            "room tone overlap must be >= 0",
        ),
        (500, 100, vec![], "no gaps to fill with room tone"),
        (
            500,
            100,
            vec![gap(10, 10)],
            "no gaps to fill with room tone",
        ),
    ] {
        match room_tone_fill_fragment("t", tone_ms, 48_000, &gaps, 0.0, overlap, 5000, "p", "o") {
            Err(AudioError::Invalid(message)) => assert_eq!(message, text),
            other => panic!("expected {text:?}, got {other:?}"),
        }
    }
}

fn room_tone_fill_and_bed() {
    let mut seed = 42;
    let rate = 16_000;
    let tone = noise(rate as usize, 0.002, &mut seed);
    let level = |x: &[f32]| rms(&x.iter().map(|v| f64::from(*v)).collect::<Vec<_>>());
    let reference = level(&tone);
    assert_eq!(rms(&[]), 0.0);
    assert!(near(rms(&[3.0, 4.0]), (12.5f64).sqrt(), 1e-12));

    // The fill has the tone's RMS plus the gain, for gaps shorter than, equal to and longer than the tone.
    let defaults = FillParams::default();
    assert_eq!((defaults.crossfade_ms, defaults.gain_db), (20.0, 0.0));
    for gap_ms in [80u64, 333, 1000, 2500, 7321] {
        let len = (u64::from(rate) * gap_ms / 1000) as usize;
        let fill = room_tone_fill(&tone, rate, len, &defaults).unwrap();
        assert_eq!(fill.len(), len);
        assert!(near(level(&fill) / reference, 1.0, 1e-4), "gap {gap_ms} ms");
    }
    let quieter = FillParams {
        crossfade_ms: 20.0,
        gain_db: -6.0,
    };
    let fill = room_tone_fill(&tone, rate, 3 * rate as usize, &quieter).unwrap();
    assert!(near(
        level(&fill) / reference,
        10f64.powf(-6.0 / 20.0),
        1e-4
    ));

    // Loop seams carry no step larger than the tone's own steps.
    let step = |x: &[f32]| {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    };
    let crossfade =
        ((defaults.crossfade_ms * f64::from(rate) / 1000.0).round() as usize).min(tone.len() / 4);
    let period = tone.len() - crossfade;
    let fill = room_tone_fill(&tone, rate, 3 * rate as usize, &defaults).unwrap();
    for k in 1..3 {
        let seam = (fill[k * period] - fill[k * period - 1]).abs();
        assert!(seam <= 1.5 * step(&tone), "seam {k}: {seam}");
    }

    // Silence in gives silence out; empty output is allowed; bad input is refused by kind.
    assert!(room_tone_fill(&vec![0.0; 1000], rate, 500, &defaults)
        .unwrap()
        .iter()
        .all(|v| *v == 0.0));
    assert!(room_tone_fill(&tone, rate, 0, &defaults)
        .unwrap()
        .is_empty());
    assert!(matches!(
        room_tone_fill(&tone[..4], rate, 100, &defaults),
        Err(AudioError::NoRoomTone(_))
    ));
    assert!(matches!(
        room_tone_fill(&tone, 0, 100, &defaults),
        Err(AudioError::Invalid(_))
    ));
    assert!(matches!(
        room_tone_fill(
            &tone,
            rate,
            100,
            &FillParams {
                crossfade_ms: -1.0,
                gain_db: 0.0
            }
        ),
        Err(AudioError::Invalid(_))
    ));
    assert!(matches!(
        room_tone_fill(
            &tone,
            rate,
            100,
            &FillParams {
                crossfade_ms: 20.0,
                gain_db: f64::NAN
            }
        ),
        Err(AudioError::Invalid(_))
    ));

    // A bed is silent except under its fills; a fill past the end is clipped and one past it is skipped.
    let bed = room_tone_bed(
        10_000,
        rate,
        &[
            ToneFill {
                start: 2000,
                len: 3000,
                tone: &tone,
            },
            ToneFill {
                start: 9000,
                len: 5000,
                tone: &tone,
            },
            ToneFill {
                start: 20_000,
                len: 100,
                tone: &tone,
            },
        ],
        &defaults,
    )
    .unwrap();
    assert_eq!(bed.len(), 10_000);
    assert!(bed[..2000].iter().all(|v| *v == 0.0) && bed[5000..9000].iter().all(|v| *v == 0.0));
    assert!(bed[2000..5000].iter().any(|v| *v != 0.0) && bed[9000..].iter().any(|v| *v != 0.0));
    assert_eq!(
        &bed[2000..5000],
        &room_tone_fill(&tone, rate, 3000, &defaults).unwrap()[..]
    );
    assert!(room_tone_bed(10, rate, &[], &defaults)
        .unwrap()
        .iter()
        .all(|v| *v == 0.0));
    assert!(matches!(
        room_tone_bed(
            10,
            rate,
            &[ToneFill {
                start: 0,
                len: 5,
                tone: &tone[..2]
            }],
            &defaults
        ),
        Err(AudioError::NoRoomTone(_))
    ));
}

fn error_text() {
    for (error, text) in [
        (
            AudioError::Invalid("x".into()),
            "invalid audio parameter: x",
        ),
        (
            AudioError::MissingInput(PathBuf::from("in.wav")),
            "input does not exist: in.wav",
        ),
        (
            AudioError::OutputIsInput,
            "output must not overwrite the input",
        ),
        (
            AudioError::NoRoomTone("quiet".into()),
            "no quiet non-speech region found for room tone: quiet",
        ),
        (
            AudioError::Measurement("m".into()),
            "could not read ffmpeg measurement: m",
        ),
        (AudioError::Wav("w".into()), "wav: w"),
        (AudioError::Playback("p".into()), "playback: p"),
    ] {
        assert_eq!(error.to_string(), text);
    }
}

fn main() {
    let scratch = std::env::temp_dir().join(format!("rk-audio-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("create consumer temp dir");
    filter_text();
    dialogue_chain();
    dialogue_chain_json_and_model_files(&scratch);
    dialogue_bus();
    ducking_forms();
    presets();
    loudness_gate_and_levels();
    wav_files(&scratch);
    mic_alignment();
    room_tone_discovery();
    room_tone_gaps_and_fragment();
    room_tone_fill_and_bed();
    error_text();
    std::fs::remove_dir_all(&scratch).expect("remove consumer temp dir");
    println!("OK rightkit-audio 0.3.1 (registry consumer)");
}
