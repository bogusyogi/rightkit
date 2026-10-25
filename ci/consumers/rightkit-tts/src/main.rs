// rightkit-tts 0.2.3 consumed by exact registry pin (default features: the `system` OS-voice
// backend and the `client` QTS worker client; GenRight uses the `client` subset), outside the
// workspace.
//
// Proves, with no OS voice, no model, no ONNX Runtime and no worker process: the sample-rate
// constants and the PCM16 WAV helpers; long-text chunking and the frame budget; the worker wire
// protocol (modes, preset speakers, request and reply JSON); the per-mode model locations and the
// engine cache; the worker command line and its parser; the model manifest, data-root and model
// folder resolution, size and digest checks on synthetic files, 1.7B mode file discovery and ONNX
// Runtime discovery; the abbreviation-aware sentence splitter of the system backend and its
// option and error types; and the client's configuration, request validation and every pre-spawn
// refusal (`models_missing`, `worker_missing`, `runtime_missing`, `voice_missing`,
// `invalid_request`) through QtsClient and the free functions, none of which starts a process.
// Public API only.
// Does not prove: a rendered utterance by `say` or SAPI, the Qwen3-TTS worker, model loading, the
// `engine`, `metal`, `vulkan` and `dev-overrides` features, or the model files themselves (the
// 1.3 GB checkpoint size is never faked).
// State: a per-process directory under the OS temp directory, removed at the end; process
// environment variables of the crate are set and cleared. No network, no process spawn, no audio
// device, no user data.
use rightkit_tts::mode_location::{resolve_location, EngineCache, VOCODER_FILE};
use rightkit_tts::models::{self, DataRootError, MANIFEST, MODE_MANIFESTS};
use rightkit_tts::protocol::{self, Reply, Request, REPLY_PREFIX};
use rightkit_tts::worker_args::{self, WorkerArgs, DEFAULT_IDLE_SECS};
use rightkit_tts::{
    chunk, chunk_sentences, client, wav, Mode, ModeLocation, QtsClient, QtsError, SentenceChunk,
    SynthOptions, SynthResult, SystemTts, SystemTtsError, SystemTtsManifest, SystemTtsOptions,
    SystemTtsSegment, PRESET_SPEAKERS, QTS_SAMPLE_RATE_HZ, SAMPLE_RATE_HZ,
};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

const ENV: [&str; 11] = [
    "RIGHTKIT_DATA_ROOT",
    "GENRIGHT_DATA_ROOT",
    "RIGHTKIT_TTS_MODEL_DIR",
    "GENRIGHT_QTS_MODEL_DIR",
    "RIGHTKIT_TTS_WORKER",
    "GENRIGHT_QTS_WORKER",
    "RIGHTKIT_TTS_IDLE_SECS",
    "GENRIGHT_QTS_IDLE_SECS",
    "ORT_DYLIB_PATH",
    "RIGHTKIT_TTS_ORT",
    "GENRIGHT_QTS_ORT",
];

/// Start from a known environment: none of the variables the crate reads is set.
fn clear_environment() {
    for name in ENV {
        std::env::remove_var(name);
    }
}

fn touch(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(ToString::to_string).collect()
}

fn constants_and_wav() {
    assert_eq!((QTS_SAMPLE_RATE_HZ, SAMPLE_RATE_HZ), (24_000, 24_000));
    let bytes = wav::encode_pcm16(&[0, 1, -1, 32_767], 24_000);
    assert_eq!(bytes.len(), 44 + 8);
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + 8);
    assert_eq!(&bytes[8..16], b"WAVEfmt ");
    assert_eq!(
        u16::from_le_bytes(bytes[20..22].try_into().unwrap()),
        1,
        "PCM"
    );
    assert_eq!(
        u16::from_le_bytes(bytes[22..24].try_into().unwrap()),
        1,
        "mono"
    );
    assert_eq!(
        u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
        24_000
    );
    assert_eq!(
        u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
        48_000,
        "byte rate"
    );
    assert_eq!(&bytes[36..40], b"data");
    assert_eq!(
        wav::decode_pcm16(&bytes),
        Some((24_000, vec![0, 1, -1, 32_767]))
    );
}

fn wav_edge_cases() {
    // An empty data chunk decodes to no samples; truncation and foreign data decode to nothing.
    assert_eq!(
        wav::decode_pcm16(&wav::encode_pcm16(&[], 16_000)),
        Some((16_000, vec![]))
    );
    assert_eq!(wav::decode_pcm16(&[0u8; 10]), None);
    assert_eq!(wav::decode_pcm16(&vec![b'x'; 64]), None);
    let mut not_wave = wav::encode_pcm16(&[1, 2, 3], 24_000);
    not_wave[8..12].copy_from_slice(b"AVI ");
    assert_eq!(wav::decode_pcm16(&not_wave), None);
    // A stereo file yields its first channel.
    let mut stereo = wav::encode_pcm16(&[10, 90, 20, 91, 30, 92], 24_000);
    stereo[22..24].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(wav::decode_pcm16(&stereo), Some((24_000, vec![10, 20, 30])));
    // Conversion clamps, scales by 32767 and truncates toward zero; silence is sized by the rate.
    assert_eq!(
        wav::f32_to_i16(&[0.0, 0.5, -0.5, 1.0, -1.0, 2.0, -2.0]),
        [0, 16_383, -16_383, 32_767, -32_767, 32_767, -32_767]
    );
    assert_eq!(wav::silence(500).len(), 12_000);
    assert!(wav::silence(0).is_empty() && wav::silence(500).iter().all(|s| *s == 0));
}

fn chunking() {
    assert_eq!(chunk::DEFAULT_MAX_CHARS, 220);
    let one = chunk::split_text("Hello there. How are you?", 220);
    assert_eq!(
        one,
        [chunk::Chunk {
            text: "Hello there. How are you?".into(),
            pause_ms: 0
        }]
    );
    // Paragraphs are separate chunks with a 280 ms join; a decimal point is not a sentence end.
    let two = chunk::split_text("Costs 3.5 dollars.\nNext paragraph here.", 220);
    assert_eq!(
        two.iter()
            .map(|c| (c.text.as_str(), c.pause_ms))
            .collect::<Vec<_>>(),
        [("Costs 3.5 dollars.", 280), ("Next paragraph here.", 0)]
    );
    // Sentences are packed up to the limit; every other join is 200 ms and the last is 0.
    let packed = chunk::split_text("This is one. This is two. This is three.", 20);
    assert_eq!(
        packed
            .iter()
            .map(|c| (c.text.as_str(), c.pause_ms))
            .collect::<Vec<_>>(),
        [
            ("This is one.", 200),
            ("This is two.", 200),
            ("This is three.", 0)
        ]
    );
    let long = "This is a sentence of moderate length. ".repeat(30);
    let many = chunk::split_text(&long, 120);
    assert!(many.len() > 5 && many.iter().all(|c| c.text.chars().count() <= 120));
    assert!(many.iter().all(|c| c.text.ends_with('.')));
    assert_eq!(many.last().unwrap().pause_ms, 0);
    assert!(many[..many.len() - 1].iter().all(|c| c.pause_ms == 200));
    // A sentence with no punctuation is cut at whitespace, never inside a word.
    let words =
        "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi";
    let cut = chunk::split_text(words, 30);
    assert!(
        cut.len() >= 3 && cut.iter().all(|c| c.text.chars().count() <= 30),
        "{cut:?}"
    );
    let rejoined: Vec<&str> = cut.iter().flat_map(|c| c.text.split_whitespace()).collect();
    assert_eq!(rejoined, words.split_whitespace().collect::<Vec<_>>());
    // The limit has a floor of 20 characters; empty and blank input produce nothing.
    assert_eq!(chunk::split_text(words, 1), chunk::split_text(words, 20));
    assert!(chunk::split_text("  \n ", 100).is_empty() && chunk::split_text("", 100).is_empty());
    // Non-ASCII sentence ends split sentences, and the text survives intact.
    let cjk = chunk::split_text("\u{4f60}\u{597d}\u{3002}\u{518d}\u{89c1}\u{ff01}", 100);
    assert_eq!(cjk.len(), 1);
    assert_eq!(
        cjk[0].text.replace(' ', ""),
        "\u{4f60}\u{597d}\u{3002}\u{518d}\u{89c1}\u{ff01}"
    );
    // The frame budget is 3 frames a character plus slack, within 64 and 1500.
    assert_eq!(
        [0, 10, 100, 100_000].map(chunk::frame_budget),
        [64, 80, 350, 1500]
    );
}

fn modes_and_protocol() {
    for (mode, name) in [
        (Mode::Base, "base"),
        (Mode::CustomVoice, "custom_voice"),
        (Mode::VoiceDesign, "voice_design"),
    ] {
        assert_eq!(mode.as_str(), name);
        assert_eq!(Mode::parse(name), Some(mode));
        assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{name}\""));
    }
    assert_eq!(Mode::parse(" Voice_Design "), Some(Mode::VoiceDesign));
    assert_eq!(Mode::parse("customvoice"), None);
    assert_eq!(Mode::default(), Mode::Base);

    assert_eq!(
        PRESET_SPEAKERS,
        ["vivian", "serena", "uncle_fu", "dylan", "eric", "ryan", "aiden", "ono_anna", "sohee"]
    );
    assert!(protocol::is_preset_speaker(" Vivian "));
    assert!(!protocol::is_preset_speaker("uncle-fu") && !protocol::is_preset_speaker(""));
    assert_eq!(REPLY_PREFIX, "@@GQTS@@ ");

    // Requests and replies are one JSON object per line, tagged by `op` and `type`.
    assert_eq!(
        serde_json::to_string(&Request::Quit).unwrap(),
        r#"{"op":"quit"}"#
    );
    assert_eq!(
        serde_json::to_string(&Request::Clone {
            id: 4,
            wav: "in.wav".into(),
            out: "out.pb".into()
        })
        .unwrap(),
        r#"{"op":"clone","id":4,"wav":"in.wav","out":"out.pb"}"#
    );
    let synth = Request::Synth {
        id: 7,
        text: "hi".into(),
        voice_prompt: None,
        mode: Some(Mode::CustomVoice),
        instruct: Some("warm".into()),
        speaker: Some("vivian".into()),
        language_id: 2050,
        max_frames: 100,
        temperature: 0.5,
        top_p: 1.0,
        top_k: 50,
        out_pcm: "x.pcm".into(),
    };
    let line = serde_json::to_string(&synth).unwrap();
    assert_eq!(
        line,
        r#"{"op":"synth","id":7,"text":"hi","voice_prompt":null,"mode":"custom_voice","instruct":"warm","speaker":"vivian","language_id":2050,"max_frames":100,"temperature":0.5,"top_p":1.0,"top_k":50,"out_pcm":"x.pcm"}"#
    );
    assert_eq!(
        serde_json::to_string(&serde_json::from_str::<Request>(&line).unwrap()).unwrap(),
        line
    );
    // A request from an older client has no mode fields (Base); unknown keys are ignored.
    let old = r#"{"op":"synth","id":1,"text":"t","language_id":1,"max_frames":2,"temperature":0.9,"top_p":1.0,"top_k":50,"out_pcm":"o","future":{"a":1}}"#;
    match serde_json::from_str::<Request>(old).unwrap() {
        Request::Synth {
            mode,
            speaker,
            instruct,
            voice_prompt,
            ..
        } => {
            assert_eq!(
                (mode, speaker, instruct, voice_prompt),
                (None, None, None, None)
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(serde_json::from_str::<Request>(r#"{"op":"unknown"}"#).is_err());
    for (reply, json) in [
        (
            Reply::Ready {
                backend: "cpu".into(),
                vocoder: "v.onnx".into(),
                load_ms: 12,
                version: "1".into(),
            },
            r#"{"type":"ready","backend":"cpu","vocoder":"v.onnx","load_ms":12,"version":"1"}"#,
        ),
        (
            Reply::Done {
                id: 2,
                samples: 480,
                sample_rate: 24_000,
                gen_ms: 9,
            },
            r#"{"type":"done","id":2,"samples":480,"sample_rate":24000,"gen_ms":9}"#,
        ),
        (
            Reply::Error {
                id: 3,
                message: "model_missing: base".into(),
            },
            r#"{"type":"error","id":3,"message":"model_missing: base"}"#,
        ),
    ] {
        assert_eq!(serde_json::to_string(&reply).unwrap(), json);
        assert_eq!(
            serde_json::to_string(&serde_json::from_str::<Reply>(json).unwrap()).unwrap(),
            json
        );
    }
}

fn mode_locations(scratch: &Path) {
    assert_eq!(VOCODER_FILE, "qwen3-tts-vocoder.onnx");
    let base = ModeLocation::new(Mode::Base, "/models/base");
    assert_eq!((base.mode, base.vocoder.clone()), (Mode::Base, None));
    assert_eq!(
        base.with_vocoder("/v.onnx").vocoder,
        Some(PathBuf::from("/v.onnx"))
    );

    // A mode without an entry uses the defaults; the last entry for a mode wins.
    let (dir, vocoder) = resolve_location(Path::new("/m"), Path::new("/v.onnx"), &[], Mode::Base);
    assert_eq!(
        (dir, vocoder),
        (PathBuf::from("/m"), PathBuf::from("/v.onnx"))
    );
    let entries = [
        ModeLocation::new(Mode::VoiceDesign, "/old").with_vocoder("/old/v.onnx"),
        ModeLocation::new(Mode::VoiceDesign, "/new").with_vocoder("/new/v.onnx"),
    ];
    assert_eq!(
        resolve_location(
            Path::new("/m"),
            Path::new("/v"),
            &entries,
            Mode::VoiceDesign
        ),
        (PathBuf::from("/new"), PathBuf::from("/new/v.onnx"))
    );
    assert_eq!(
        resolve_location(Path::new("/m"), Path::new("/v"), &entries, Mode::Base).0,
        PathBuf::from("/m")
    );
    // Without its own vocoder an entry prefers the vocoder in its folder, then the default.
    let with = scratch.join("with-vocoder");
    let without = scratch.join("without-vocoder");
    touch(&with.join(VOCODER_FILE), b"v");
    std::fs::create_dir_all(&without).unwrap();
    let entries = [
        ModeLocation::new(Mode::CustomVoice, &with),
        ModeLocation::new(Mode::VoiceDesign, &without),
    ];
    let default = Path::new("/base/v.onnx");
    assert_eq!(
        resolve_location(Path::new("/b"), default, &entries, Mode::CustomVoice).1,
        with.join(VOCODER_FILE)
    );
    assert_eq!(
        resolve_location(Path::new("/b"), default, &entries, Mode::VoiceDesign).1,
        default
    );

    // The engine cache loads once per mode, keeps load order, and retries a failed load.
    let mut cache: EngineCache<u32> = EngineCache::default();
    let mut loads = 0;
    for mode in [
        Mode::Base,
        Mode::VoiceDesign,
        Mode::Base,
        Mode::CustomVoice,
        Mode::VoiceDesign,
    ] {
        cache
            .get_or_load(mode, || {
                loads += 1;
                Ok(loads)
            })
            .unwrap();
    }
    assert_eq!(loads, 3);
    assert_eq!(
        cache.loaded(),
        [Mode::Base, Mode::VoiceDesign, Mode::CustomVoice]
    );
    let mut fresh: EngineCache<u32> = EngineCache::default();
    let failed = fresh
        .get_or_load(Mode::Base, || Err("model_missing: base: /x".into()))
        .unwrap_err();
    assert!(failed.starts_with("model_missing:") && fresh.loaded().is_empty());
    assert_eq!(*fresh.get_or_load(Mode::Base, || Ok(7)).unwrap(), 7);
}

fn worker_command_line() {
    assert_eq!(DEFAULT_IDLE_SECS, 600);
    let defaults = WorkerArgs::default();
    assert_eq!(defaults.mode, Mode::Base);
    assert_eq!(
        defaults.to_command_line(),
        [OsString::from("--mode"), "base".into()]
    );
    let args = worker_args::parse(strings(&[
        "--model-dir",
        "/models",
        "--backend",
        "cpu",
        "--mode",
        "custom_voice",
        "--vocoder",
        "/v.onnx",
        "--data-root",
        "/data",
        "--ort",
        "/ort.so",
        "--mode-model-dir",
        "voice_design",
        "/design",
        "--mode-vocoder",
        "voice_design",
        "/design/v.onnx",
        "--idle-secs",
        "30",
    ]))
    .unwrap();
    assert_eq!(args.model_dir, Some(PathBuf::from("/models")));
    assert_eq!(
        (args.backend.as_deref(), args.mode, args.idle_secs),
        (Some("cpu"), Mode::CustomVoice, Some(30))
    );
    assert_eq!(
        args.mode_locations,
        [ModeLocation::new(Mode::VoiceDesign, "/design").with_vocoder("/design/v.onnx")]
    );
    assert_eq!(args.idle_timeout(), Duration::from_secs(30));
    // The command line parses back to the same arguments.
    let line: Vec<String> = args
        .to_command_line()
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
    assert_eq!(worker_args::parse(line).unwrap(), args);
    for (bad, message) in [
        (vec!["--model-dir"], "--model-dir needs a value"),
        (
            vec!["--mode", "loud"],
            "unknown mode loud; expected base, custom_voice or voice_design",
        ),
        (
            vec!["--idle-secs", "soon"],
            "--idle-secs needs a whole number, got soon",
        ),
        (
            vec!["--mode-vocoder", "base", "/v"],
            "--mode-vocoder base has no --mode-model-dir",
        ),
        (vec!["--surprise"], "unknown argument --surprise"),
    ] {
        assert_eq!(worker_args::parse(strings(&bad)).unwrap_err(), message);
    }
    assert_eq!(
        worker_args::parse(Vec::<String>::new()).unwrap(),
        WorkerArgs::default()
    );

    // The idle window comes from the arguments, then RIGHTKIT_TTS_IDLE_SECS, then the legacy
    // name, then the default.
    assert_eq!(worker_args::env_idle_timeout(), Duration::from_secs(600));
    std::env::set_var("GENRIGHT_QTS_IDLE_SECS", "11");
    assert_eq!(worker_args::env_idle_timeout(), Duration::from_secs(11));
    std::env::set_var("RIGHTKIT_TTS_IDLE_SECS", "7");
    assert_eq!(worker_args::env_idle_timeout(), Duration::from_secs(7));
    assert_eq!(WorkerArgs::default().idle_timeout(), Duration::from_secs(7));
    std::env::set_var("RIGHTKIT_TTS_IDLE_SECS", "not a number");
    assert_eq!(worker_args::env_idle_timeout(), Duration::from_secs(600));
    clear_environment();
}

fn manifest_and_roots(scratch: &Path) {
    assert_eq!(models::MODEL_ID, "qwen3-tts-0.6b-q8_0");
    assert_eq!(models::HF_REPO, "dsh0416/Qwen3-TTS-12Hz-0.6B-Base-QTS");
    assert_eq!(
        (
            MANIFEST[0].name,
            MANIFEST[0].bytes,
            MANIFEST[1].name,
            MANIFEST[1].bytes
        ),
        (
            "qwen3-tts-0.6b-q8_0.gguf",
            1_325_190_016,
            VOCODER_FILE,
            459_514_304
        )
    );
    assert_eq!(
        models::DOWNLOAD_BYTES,
        MANIFEST[0].bytes + MANIFEST[1].bytes
    );
    for file in &MANIFEST {
        assert!(
            file.sha256.len() == 64
                && file
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }
    assert!(models::mode_manifest(Mode::Base).is_none());
    assert_eq!(MODE_MANIFESTS.len(), 2);
    for mode in [Mode::CustomVoice, Mode::VoiceDesign] {
        let manifest = models::mode_manifest(mode).unwrap();
        assert_eq!(manifest.mode, mode);
        let stem = format!(
            "qwen3-tts-1.7b-{}",
            if mode == Mode::CustomVoice {
                "customvoice"
            } else {
                "voicedesign"
            }
        );
        assert_eq!(manifest.files.len(), 3);
        assert!(manifest
            .files
            .iter()
            .all(|f| f.name.starts_with(&stem) && f.name.ends_with(".gguf")));
        assert!(
            manifest
                .files
                .iter()
                .all(|f| f.bytes.is_none() && f.sha256.is_none()),
            "no 1.7B export is pinned yet"
        );
    }

    // The data root is the caller's absolute path, else RIGHTKIT_DATA_ROOT (then the legacy name),
    // else the per-user base; a relative path is refused.
    let root = scratch.join("data");
    assert_eq!(models::data_root_in(Some(&root)).unwrap(), root);
    let relative = models::data_root_in(Some(Path::new("relative/root"))).unwrap_err();
    assert_eq!(
        relative,
        DataRootError("relative/root is not absolute".into())
    );
    assert_eq!(
        relative.to_string(),
        "cannot resolve the RightKit data root: relative/root is not absolute"
    );
    assert_eq!(
        models::try_voices_dir_in(Some(&root)).unwrap(),
        root.join("voices").join("local")
    );
    assert_eq!(
        models::model_dir_in(Some(&root)).unwrap(),
        root.join("models").join("qts")
    );
    std::env::set_var("GENRIGHT_DATA_ROOT", scratch.join("legacy"));
    assert_eq!(models::try_data_root().unwrap(), scratch.join("legacy"));
    std::env::set_var("RIGHTKIT_DATA_ROOT", &root);
    assert_eq!(models::try_data_root().unwrap(), root);
    assert_eq!(
        models::try_model_dir().unwrap(),
        root.join("models").join("qts")
    );
    assert_eq!(
        models::try_voices_dir().unwrap(),
        root.join("voices").join("local")
    );
    assert_eq!(
        (
            models::data_root(),
            models::model_dir(),
            models::voices_dir()
        ),
        (
            root.clone(),
            root.join("models").join("qts"),
            root.join("voices").join("local")
        )
    );
    // A relative override fails closed in the try_ forms.
    std::env::set_var("RIGHTKIT_DATA_ROOT", "relative");
    assert!(models::try_data_root()
        .unwrap_err()
        .0
        .contains("not absolute"));
    assert!(models::try_model_dir().is_err());
    // The model folder override beats the data root, new name first.
    std::env::set_var("RIGHTKIT_DATA_ROOT", &root);
    std::env::set_var("GENRIGHT_QTS_MODEL_DIR", scratch.join("old-models"));
    assert_eq!(
        models::model_dir_in(Some(&root)).unwrap(),
        scratch.join("old-models")
    );
    std::env::set_var("RIGHTKIT_TTS_MODEL_DIR", scratch.join("new-models"));
    assert_eq!(
        models::model_dir_in(Some(&root)).unwrap(),
        scratch.join("new-models")
    );
    assert_eq!(models::model_dir(), scratch.join("new-models"));
    clear_environment();
}

fn model_files(scratch: &Path) {
    let dir = scratch.join("model-files");
    std::fs::create_dir_all(&dir).unwrap();
    let vocoder = dir.join(VOCODER_FILE);
    // An empty folder has nothing; both Base files are missing and are listed.
    assert!(!models::files_present(&dir) && !models::base_files_present(&dir, &vocoder));
    assert_eq!(
        models::missing_for_mode(&dir, &vocoder, Mode::Base),
        [dir.join(MANIFEST[0].name), vocoder.clone()]
    );
    assert_eq!(
        models::verify(&dir, false).unwrap_err(),
        format!(
            "missing model file {}",
            dir.join(MANIFEST[0].name).display()
        )
    );
    // Files of the wrong size are not the model: the exact byte count is part of the manifest.
    touch(&dir.join(MANIFEST[0].name), b"short");
    touch(&vocoder, b"short");
    assert!(!models::files_present(&dir));
    assert_eq!(
        models::missing_for_mode(&dir, &vocoder, Mode::Base).len(),
        2
    );
    assert_eq!(
        models::verify(&dir, false).unwrap_err(),
        "qwen3-tts-0.6b-q8_0.gguf has 5 bytes, expected 1325190016"
    );
    assert!(models::verify_base(&dir, &dir.join("elsewhere.onnx"), true)
        .unwrap_err()
        .contains("expected 1325190016"));
    // Only the vocoder at its default place is size-pinned; one elsewhere only has to be a file.
    let elsewhere = scratch.join("shared").join("v.onnx");
    touch(&elsewhere, b"any bytes");
    assert_eq!(
        models::missing_for_mode(&dir, &elsewhere, Mode::Base),
        [dir.join(MANIFEST[0].name)]
    );
    // The mode with no pinned export passes verification without reading anything.
    assert_eq!(models::verify_mode(&dir, Mode::Base, true), Ok(()));
    assert_eq!(models::verify_mode(&dir, Mode::CustomVoice, false), Ok(()));
    // 1.7B modes: absent components are listed under their f16 names; a bundled file wins.
    let modes = scratch.join("mode-files");
    std::fs::create_dir_all(&modes).unwrap();
    assert!(models::mode_model_files(&modes, Mode::Base).is_empty());
    let absent = models::mode_model_files(&modes, Mode::CustomVoice);
    assert_eq!(
        absent
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "qwen3-tts-1.7b-customvoice-talker-f16.gguf",
            "qwen3-tts-1.7b-customvoice-code-predictor-f16.gguf",
            "qwen3-tts-1.7b-customvoice-tokenizer.gguf"
        ]
    );
    assert_eq!(
        models::missing_for_mode(&modes, &modes.join(VOCODER_FILE), Mode::CustomVoice).len(),
        4
    );
    touch(
        &modes.join("qwen3-tts-1.7b-customvoice-talker-q8_0.gguf"),
        b"t",
    );
    assert_eq!(
        models::mode_model_files(&modes, Mode::CustomVoice)[0],
        modes.join("qwen3-tts-1.7b-customvoice-talker-q8_0.gguf")
    );
    touch(
        &modes.join("qwen3-tts-1.7b-voicedesign-q4_k.gguf"),
        b"bundled",
    );
    assert_eq!(
        models::mode_model_files(&modes, Mode::VoiceDesign),
        [modes.join("qwen3-tts-1.7b-voicedesign-q4_k.gguf")]
    );
    touch(&modes.join(VOCODER_FILE), b"v");
    assert!(
        models::missing_for_mode(&modes, &modes.join(VOCODER_FILE), Mode::VoiceDesign).is_empty()
    );

    // SHA-256 of known bytes, through the shared digest helper; a missing file is an I/O error.
    let abc = scratch.join("abc.bin");
    touch(&abc, b"abc");
    assert_eq!(
        models::sha256_file(&abc).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(models::sha256_file(&scratch.join("nothing.bin")).is_err());

    // ONNX Runtime discovery looks beside the model folder's parent and under <data>/runtime/qts.
    let data = scratch.join("runtime-root");
    let runtime_name = models::runtime_lib_name();
    assert_eq!(
        runtime_name,
        if cfg!(windows) {
            "onnxruntime.dll"
        } else if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        }
    );
    assert_eq!(models::find_onnxruntime_in(Some(&data), None), None);
    touch(&data.join("runtime").join("qts").join(runtime_name), b"x");
    assert_eq!(
        models::find_onnxruntime_in(Some(&data), None),
        Some(data.join("runtime").join("qts").join(runtime_name))
    );
    let beside = scratch.join("app").join("runtime").join(runtime_name);
    touch(&beside, b"x");
    assert_eq!(
        models::find_onnxruntime_in(Some(&data), Some(&scratch.join("app").join("models"))),
        Some(beside)
    );
    assert_eq!(
        models::default_backend(),
        if cfg!(target_os = "macos") {
            "metal"
        } else {
            "cpu"
        }
    );

    // Without `dev-overrides` binding the runtime drops an inherited ORT_DYLIB_PATH, and an
    // explicit runtime is exported with its folder first on PATH.
    std::env::set_var("ORT_DYLIB_PATH", "inherited");
    models::bind_ort_runtime(None);
    assert!(std::env::var_os("ORT_DYLIB_PATH").is_none());
    let path_before = std::env::var_os("PATH");
    let runtime = data.join("runtime").join("qts").join(runtime_name);
    models::bind_ort_runtime(Some(&runtime));
    assert_eq!(
        std::env::var_os("ORT_DYLIB_PATH"),
        Some(runtime.clone().into_os_string())
    );
    let first = std::env::split_paths(&std::env::var_os("PATH").unwrap()).next();
    assert_eq!(first.as_deref(), runtime.parent());
    match path_before {
        Some(path) => std::env::set_var("PATH", path),
        None => std::env::remove_var("PATH"),
    }
    clear_environment();
}

fn system_voice_types() {
    let chunk = |text: &str, char_start, char_end| SentenceChunk {
        text: text.into(),
        char_start,
        char_end,
    };
    assert_eq!(
        chunk_sentences("Hello there. How are you? Fine!"),
        [
            chunk("Hello there.", 0, 12),
            chunk("How are you?", 13, 25),
            chunk("Fine!", 26, 31)
        ]
    );
    // Abbreviations, initials, decimals and unspaced dots do not end a sentence.
    assert_eq!(
        chunk_sentences("Dr. Smith paid 3.5 dollars. Then left.").len(),
        2
    );
    assert_eq!(
        chunk_sentences("Dr. Smith paid 3.5 dollars. Then left.")[0].text,
        "Dr. Smith paid 3.5 dollars."
    );
    assert_eq!(
        chunk_sentences("See e.g. the map, i.e. this one. Done.").len(),
        2
    );
    assert_eq!(chunk_sentences("J. K. Rowling wrote it. Yes.").len(), 2);
    assert_eq!(chunk_sentences("visit example.com today. ok").len(), 2);
    // Offsets are character offsets into the original text, not bytes.
    let accented = chunk_sentences("Caf\u{e9} ouvert. Fin.");
    assert_eq!((accented[1].char_start, accented[1].char_end), (13, 17));
    assert!(chunk_sentences("   ").is_empty() && chunk_sentences("").is_empty());
    assert_eq!(chunk_sentences("no end").len(), 1);
    assert_eq!(chunk_sentences("Really?! Yes.")[0].text, "Really?!");

    let options = SystemTtsOptions::default();
    assert_eq!(
        (
            options.voice.clone(),
            options.rate,
            options.language.as_str()
        ),
        (None, None, "en")
    );
    for (error, text) in [
        (SystemTtsError::EmptyText, "text is empty"),
        (
            SystemTtsError::Unavailable("none".into()),
            "system TTS is unavailable: none",
        ),
        (
            SystemTtsError::InvalidOutput("bad".into()),
            "system TTS output is invalid: bad",
        ),
        (
            SystemTtsError::Command("exit 1".into()),
            "system TTS command failed: exit 1",
        ),
    ] {
        assert_eq!(error.to_string(), text);
    }
    let segment = SystemTtsSegment {
        sentence_index: 0,
        text: "Hi.".into(),
        source_char_start: 0,
        source_char_end: 3,
        start_ms: 0,
        end_ms: 400,
    };
    let manifest = SystemTtsManifest {
        sample_rate: 24_000,
        channels: 1,
        bits_per_sample: 16,
        total_duration_ms: 400,
        segments: vec![segment.clone()],
    };
    assert_eq!(manifest.segments[0], segment);
    // Blank text is refused before any voice is asked for. Rendering itself needs `say` or SAPI.
    for blank in ["", "   \n\t "] {
        assert!(
            matches!(
                SystemTts.synthesize(blank, &options),
                Err(SystemTtsError::EmptyText)
            ),
            "{blank:?}"
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    assert!(matches!(
        SystemTts.synthesize("Hello.", &options),
        Err(SystemTtsError::Unavailable(message)) if message == "no OS speech backend"
    ));
}

fn synth_options_and_results() {
    let defaults = SynthOptions::default();
    assert_eq!(
        (
            defaults.language_id,
            defaults.temperature,
            defaults.top_p,
            defaults.top_k,
            defaults.max_chunk_chars
        ),
        (2050, 0.9, 1.0, 50, chunk::DEFAULT_MAX_CHARS)
    );
    assert_eq!(
        (
            defaults.mode,
            defaults.backend.clone(),
            defaults.max_interior_silence_ms
        ),
        (Mode::Base, None, None)
    );
    defaults.validate().unwrap();
    let invalid = |options: SynthOptions, message: &str| {
        let error = options.validate().unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            ("invalid_request", message)
        );
    };
    invalid(
        SynthOptions {
            speaker: Some("vivian".into()),
            ..defaults.clone()
        },
        "base mode does not accept a speaker or instruct",
    );
    invalid(
        SynthOptions {
            instruct: Some("warm".into()),
            ..defaults.clone()
        },
        "base mode does not accept a speaker or instruct",
    );
    let custom = SynthOptions {
        mode: Mode::CustomVoice,
        speaker: Some("vivian".into()),
        ..defaults.clone()
    };
    custom.validate().unwrap();
    SynthOptions {
        speaker: Some(" Eric ".into()),
        instruct: Some("slowly".into()),
        ..custom.clone()
    }
    .validate()
    .unwrap();
    invalid(
        SynthOptions {
            speaker: None,
            ..custom.clone()
        },
        "custom_voice needs a preset speaker",
    );
    invalid(
        SynthOptions {
            speaker: Some("  ".into()),
            ..custom.clone()
        },
        "speaker name is empty",
    );
    invalid(
        SynthOptions {
            speaker: Some("nobody".into()),
            ..custom.clone()
        },
        "unknown preset speaker nobody",
    );
    invalid(
        SynthOptions {
            voice_prompt: Some("v.pb".into()),
            ..custom.clone()
        },
        "custom_voice does not accept a voice prompt",
    );
    let design = SynthOptions {
        mode: Mode::VoiceDesign,
        instruct: Some("a deep calm voice".into()),
        ..defaults.clone()
    };
    design.validate().unwrap();
    invalid(
        SynthOptions {
            instruct: None,
            ..design.clone()
        },
        "voice_design needs a non-empty instruct",
    );
    invalid(
        SynthOptions {
            instruct: Some("   ".into()),
            ..design.clone()
        },
        "voice_design needs a non-empty instruct",
    );
    invalid(
        SynthOptions {
            speaker: Some("vivian".into()),
            ..design.clone()
        },
        "voice_design does not accept a speaker",
    );
    invalid(
        SynthOptions {
            voice_prompt: Some("v.pb".into()),
            ..design
        },
        "voice_design does not accept a voice prompt",
    );

    for (code, id) in [
        ("en", 2050),
        ("EN", 2050),
        (" english ", 2050),
        ("de", 2053),
        ("es", 2054),
        ("zh", 2055),
        ("ja", 2058),
        ("fr", 2061),
        ("ko", 2064),
        ("ru", 2069),
        ("it", 2070),
        ("portuguese", 2071),
    ] {
        assert_eq!(client::language_id(code), Some(id), "{code}");
    }
    assert_eq!(client::language_id("xx"), None);
    let result = SynthResult {
        wav: vec![],
        audio_seconds: 4.0,
        wall_seconds: 2.0,
        gen_seconds: 1.0,
        chunks: 2,
        backend: "cpu".into(),
        vocoder: "v".into(),
    };
    assert_eq!(result.rtf(), 0.25);
    assert!(
        SynthResult {
            audio_seconds: 0.0,
            ..result
        }
        .rtf()
            > 1e8,
        "empty audio does not divide by zero"
    );
    let error = QtsError {
        code: "models_missing",
        message: "nope".into(),
    };
    assert_eq!(error.to_string(), "models_missing: nope");
}

fn code_of<T: std::fmt::Debug>(result: Result<T, QtsError>) -> (&'static str, String) {
    let error = result.expect_err("a refusal was expected");
    (error.code, error.message)
}

fn client_refusals(scratch: &Path) {
    // Configuration is plain data: the idle window has a floor of one second unless set explicitly.
    assert_eq!(QtsClient::new(0).idle_secs(), 1);
    assert_eq!(QtsClient::new(90).idle_secs(), 90);
    assert_eq!(QtsClient::default().idle_secs(), 600);
    assert_eq!(QtsClient::new(90).with_idle_secs(0).idle_secs(), 0);
    clear_environment();

    // Worker resolution: an explicit executable is exact; the environment and discovery follow.
    let fake_worker = scratch.join("fake-worker");
    touch(&fake_worker, b"never executed");
    let absent = scratch.join("absent-worker");
    let exact = QtsClient::new(5).with_worker_exe(fake_worker.clone());
    assert_eq!(exact.worker_exe().unwrap(), fake_worker);
    let (code, message) = code_of(
        QtsClient::new(5)
            .with_worker_exe(absent.clone())
            .worker_exe(),
    );
    assert_eq!(
        (code, message),
        (
            "worker_missing",
            format!("worker {} not found", absent.display())
        )
    );
    std::env::set_var("RIGHTKIT_TTS_WORKER", &fake_worker);
    assert_eq!(client::worker_exe().unwrap(), fake_worker);
    assert_eq!(
        code_of(
            QtsClient::new(5)
                .with_worker_exe(absent.clone())
                .worker_exe()
        )
        .0,
        "worker_missing",
        "an explicit path is never replaced by the environment"
    );
    std::env::set_var("RIGHTKIT_TTS_WORKER", &absent);
    assert_eq!(
        code_of(client::worker_exe()),
        (
            "worker_missing",
            format!("worker override {} not found", absent.display())
        )
    );
    clear_environment();
    let root = scratch.join("worker-root");
    assert_eq!(code_of(client::worker_exe()).0, "worker_missing");
    let installed = root.join("runtime").join("qts").join(if cfg!(windows) {
        "rightkit-tts-worker.exe"
    } else {
        "rightkit-tts-worker"
    });
    touch(&installed, b"never executed");
    assert_eq!(
        QtsClient::new(5)
            .with_data_root(root.clone())
            .worker_exe()
            .unwrap(),
        installed
    );
    assert!(client::worker_pid().is_none() && QtsClient::new(5).worker_pid().is_none());

    // Requests are refused before any worker is considered.
    let models = scratch.join("client-models");
    std::fs::create_dir_all(&models).unwrap();
    let client = QtsClient::new(5)
        .with_model_dir(models.clone())
        .with_worker_exe(absent.clone());
    let say = SynthOptions::default();
    assert_eq!(
        code_of(client.synthesize("   ", &say)),
        ("invalid_request", "text is empty".into())
    );
    assert_eq!(
        code_of(client.synthesize(
            "Hi.",
            &SynthOptions {
                mode: Mode::CustomVoice,
                ..say.clone()
            }
        ))
        .0,
        "invalid_request"
    );
    let missing_prompt = SynthOptions {
        voice_prompt: Some(scratch.join("no-voice.pb")),
        ..say.clone()
    };
    assert_eq!(
        code_of(client.synthesize("Hi.", &missing_prompt)).0,
        "voice_missing"
    );
    // The model files are checked before a process could start: Base needs its checkpoint.
    let (code, message) = code_of(client.synthesize("Hello there.", &say));
    assert_eq!(code, "models_missing");
    assert!(
        message.starts_with(&format!(
            "QTS base model files not found in {}: ",
            models.display()
        )),
        "{message}"
    );
    assert!(
        message.contains(MANIFEST[0].name) && message.contains(VOCODER_FILE),
        "{message}"
    );
    assert_eq!(code_of(client.engine_info(None)).0, "models_missing");
    // A 1.7B export that is present passes the file check, and the missing worker is then the answer.
    touch(
        &models.join("qwen3-tts-1.7b-voicedesign-q8_0.gguf"),
        b"bundled export",
    );
    touch(&models.join(VOCODER_FILE), b"vocoder");
    let design = SynthOptions {
        mode: Mode::VoiceDesign,
        instruct: Some("a calm voice".into()),
        ..say.clone()
    };
    assert_eq!(
        code_of(client.synthesize("Hello there.", &design)),
        (
            "worker_missing",
            format!("worker {} not found", absent.display())
        )
    );
    // Then the runtime: a worker that exists but a runtime that does not stops the launch too.
    let with_runtime = QtsClient::new(5)
        .with_model_dir(models.clone())
        .with_worker_exe(fake_worker.clone())
        .with_ort_runtime(scratch.join("absent-runtime"));
    let (code, message) = code_of(with_runtime.synthesize("Hello there.", &design));
    assert_eq!(code, "runtime_missing");
    assert!(
        message.starts_with("ONNX Runtime ") && message.ends_with(" not found"),
        "{message}"
    );
    // Locations: per-mode folders and vocoders, and the explicit vocoder, are plain configuration.
    let located = QtsClient::new(5)
        .with_model_dir(models.clone())
        .with_vocoder(models.join(VOCODER_FILE))
        .with_mode_location(ModeLocation::new(
            Mode::CustomVoice,
            scratch.join("elsewhere"),
        ))
        .with_mode_model_dir(Mode::VoiceDesign, models.clone())
        .with_worker_exe(absent.clone());
    assert_eq!(
        code_of(located.synthesize("Hello there.", &design)).0,
        "worker_missing"
    );
    assert_eq!(
        code_of(located.synthesize(
            "Hello there.",
            &SynthOptions {
                mode: Mode::CustomVoice,
                speaker: Some("ryan".into()),
                ..say.clone()
            }
        ))
        .0,
        "models_missing"
    );

    // Cloning: the reference audio must exist, and a library id is a plain name.
    let reference = scratch.join("reference.wav");
    touch(&reference, &wav::encode_pcm16(&[0; 100], 24_000));
    let out = scratch.join("voices-out").join("voice.pb");
    assert_eq!(
        code_of(client.clone_from_wav(&scratch.join("no-reference.wav"), &out)).0,
        "invalid_request"
    );
    assert_eq!(
        code_of(client.clone_from_wav(&reference, &out)).0,
        "models_missing"
    );
    assert!(
        out.parent().unwrap().is_dir(),
        "the output folder is created before the worker is needed"
    );
    let library = QtsClient::new(5)
        .with_data_root(scratch.join("library"))
        .with_worker_exe(absent);
    for bad in ["", "a b", "../x", "a.b"] {
        assert_eq!(
            code_of(library.clone_into_library(&reference, bad)),
            ("invalid_request", "voice id must be [A-Za-z0-9_-]+".into()),
            "{bad:?}"
        );
    }
    assert_eq!(
        code_of(library.clone_into_library(&reference, "my-voice_1")).0,
        "models_missing"
    );
    assert!(scratch
        .join("library")
        .join("voices")
        .join("local")
        .is_dir());

    // A voice record is only accepted inside the voices folder.
    let voices = scratch.join("library").join("voices").join("local");
    touch(&voices.join("a.xvector.pb"), b"x");
    let canonical = voices.canonicalize().unwrap().join("a.xvector.pb");
    assert_eq!(
        library.resolve_voice_record("a.xvector.pb").unwrap(),
        canonical
    );
    assert_eq!(
        client::resolve_voice_record_in(&voices, "a.xvector.pb").unwrap(),
        canonical
    );
    touch(&scratch.join("library").join("outside.pb"), b"x");
    assert_eq!(
        code_of(client::resolve_voice_record_in(&voices, "../../outside.pb")).0,
        "invalid_request"
    );
    assert_eq!(
        code_of(client::resolve_voice_record_in(&voices, "missing.pb")).0,
        "voice_missing"
    );
    assert_eq!(
        code_of(client::resolve_voice_record_in(
            &scratch.join("no-voices"),
            "x.pb"
        ))
        .0,
        "voice_missing"
    );

    // The free functions use the per-user data root, here RIGHTKIT_DATA_ROOT, and refuse alike.
    let user = scratch.join("user-root");
    std::env::set_var("RIGHTKIT_DATA_ROOT", &user);
    std::env::set_var("RIGHTKIT_TTS_WORKER", scratch.join("absent-worker"));
    assert_eq!(
        code_of(client::synthesize("Hello there.", &say)).0,
        "models_missing"
    );
    assert_eq!(
        code_of(client::engine_info(Some("cpu"))).0,
        "models_missing"
    );
    assert_eq!(
        code_of(client::clone_into_library(&reference, "mine")).0,
        "models_missing"
    );
    assert_eq!(
        code_of(client::resolve_voice_record("x.pb")).0,
        "voice_missing"
    );
    client::cancel_active();
    client::shutdown();
    assert!(client::worker_pid().is_none());
    clear_environment();
}

fn main() {
    clear_environment();
    let scratch = std::env::temp_dir().join(format!("rk-tts-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("create consumer temp dir");
    constants_and_wav();
    wav_edge_cases();
    chunking();
    modes_and_protocol();
    mode_locations(&scratch);
    worker_command_line();
    manifest_and_roots(&scratch);
    model_files(&scratch);
    system_voice_types();
    synth_options_and_results();
    client_refusals(&scratch);
    std::fs::remove_dir_all(&scratch).expect("remove consumer temp dir");
    println!("OK rightkit-tts 0.2.3 (registry consumer)");
}
