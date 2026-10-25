// rightkit-tts-qwen 0.2.7 consumed by exact registry pin (default features, which is the plain CPU
// build; GenRight selects backends through its own `metal` and `vulkan` features), outside the
// workspace. The library crate is named `qts`.
//
// Proves, with no Qwen3-TTS model, no ONNX Runtime and no audio device: the synthesis request and
// result defaults; the talker KV mode names; the mode, preset speaker and request-combination rules;
// the voice-clone prompt protobuf (round trip, accessors and every validation refusal); model path
// resolution for the 0.6B and 1.7B layouts; the GGUF container reader against GGUF files that this
// program writes byte by byte (metadata of every kind, tensors, tensor reads, refusals); the text
// tokenizer built from such a file (byte-level BPE merges, the TTS prompt layouts, decode, and its
// load refusals); the pure-Rust reference speaker encoder over generated WAV audio; the stage
// timing record; the config types; and the engine and vocoder load refusals that happen before any
// transformer, backend or ONNX Runtime session exists. The sealed archive also builds its vendored
// ggml through rightkit-tts-qwen-sys on the host, so a green run proves that link too.
// Public API only.
// Does not prove: a loaded transformer, a synthesized waveform, the vocoder session, the Metal,
// Vulkan, BLAS and execution-provider features, or any real model file.
// State: a per-process directory under the OS temp directory, removed at the end. No network, no
// process spawn, no GPU, no user data.
use qts::{
    load_and_validate, BackendKind, CodePredictorConfig, CodecRolloutSubTimings, GgufFile,
    ModeModelPaths, ModeSynthesizeRequest, ModelPaths, PresetSpeaker, QtsMode, Qwen3TtsEngine,
    Qwen3TtsError, SpeakerEncoder, SpeakerEncoderConfig, StreamingSynthesis,
    StreamingSynthesizeResult, SynthesisStageTimings, SynthesizeRequest, SynthesizeResult,
    TalkerKvMode, TensorF32, TensorI32, TextTokenizer, TokenizerConfig, TtsTransformerConfig,
    Vocoder, VocoderConfig, VocoderExecutionProvider, VoiceClonePromptV2, SAMPLE_RATE_HZ,
    VOICE_CLONE_PROMPT_V2_SCHEMA_VERSION,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn message(error: &Qwen3TtsError) -> String {
    error.to_string()
}

fn invalid_input(result: Result<(), Qwen3TtsError>) -> String {
    match result {
        Err(Qwen3TtsError::InvalidInput(text)) => text,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

// ---- GGUF fixtures --------------------------------------------------------------------------

enum Value<'a> {
    U32(u32),
    F32(f32),
    Str(&'a str),
    Strs(&'a [&'a str]),
}

fn put_string(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u64).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

fn pad_to_32(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(32), 0);
}

/// A GGUF version 3 file: metadata, F32 tensors (dims fastest first), 32-byte alignment.
fn gguf(values: &[(&str, Value)], tensors: &[(&str, &[u64], &[f32])]) -> Vec<u8> {
    let mut out = b"GGUF".to_vec();
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(tensors.len() as u64).to_le_bytes());
    out.extend_from_slice(&(values.len() as u64).to_le_bytes());
    for (key, value) in values {
        put_string(&mut out, key);
        match value {
            Value::U32(v) => {
                out.extend_from_slice(&4u32.to_le_bytes());
                out.extend_from_slice(&v.to_le_bytes());
            }
            Value::F32(v) => {
                out.extend_from_slice(&6u32.to_le_bytes());
                out.extend_from_slice(&v.to_le_bytes());
            }
            Value::Str(text) => {
                out.extend_from_slice(&8u32.to_le_bytes());
                put_string(&mut out, text);
            }
            Value::Strs(items) => {
                out.extend_from_slice(&9u32.to_le_bytes());
                out.extend_from_slice(&8u32.to_le_bytes());
                out.extend_from_slice(&(items.len() as u64).to_le_bytes());
                for item in *items {
                    put_string(&mut out, item);
                }
            }
        }
    }
    let mut offset = 0u64;
    for (name, dims, data) in tensors {
        put_string(&mut out, name);
        out.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for dim in *dims {
            out.extend_from_slice(&dim.to_le_bytes());
        }
        out.extend_from_slice(&0u32.to_le_bytes()); // GGML_TYPE_F32
        out.extend_from_slice(&offset.to_le_bytes());
        offset += (data.len() as u64 * 4).next_multiple_of(32);
    }
    pad_to_32(&mut out);
    for (_, dims, data) in tensors {
        assert_eq!(dims.iter().product::<u64>() as usize, data.len());
        for value in *data {
            out.extend_from_slice(&value.to_le_bytes());
        }
        pad_to_32(&mut out);
    }
    out
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

const TOKENS: [&str; 9] = [
    "<pad>",
    "<|im_start|>",
    "<|im_end|>",
    "assistant",
    "\u{10a}",
    "h",
    "i",
    "user",
    "hi",
];

fn tokenizer_file(path: &Path, tokens: &[&str], merges: &[&str]) {
    write(
        path,
        &gguf(
            &[
                ("tokenizer.ggml.tokens", Value::Strs(tokens)),
                ("tokenizer.ggml.merges", Value::Strs(merges)),
                ("tokenizer.ggml.bos_token_id", Value::U32(1)),
                ("tokenizer.ggml.eos_token_id", Value::U32(2)),
                ("tokenizer.ggml.padding_token_id", Value::U32(0)),
            ],
            &[],
        ),
    );
}

// ---- checks ---------------------------------------------------------------------------------

fn requests_and_modes() {
    assert_eq!(SAMPLE_RATE_HZ, 24_000);
    let request = SynthesizeRequest::default();
    assert_eq!(request.text, "");
    assert_eq!(
        (
            request.temperature,
            request.top_p,
            request.top_k,
            request.max_audio_frames
        ),
        (0.9, 1.0, 50, 4096)
    );
    assert_eq!(
        (
            request.thread_count,
            request.repetition_penalty,
            request.language_id
        ),
        (4, 1.05, 2050)
    );
    assert_eq!(
        (
            request.vocoder_thread_count,
            request.vocoder_chunk_size,
            request.talker_kv_mode
        ),
        (4, 0, TalkerKvMode::F16)
    );
    let result = SynthesizeResult {
        pcm_f32: vec![0.0; 3],
        sample_rate_hz: SAMPLE_RATE_HZ,
        generated_frames: 0,
    };
    assert_eq!(result.clone().pcm_f32.len(), 3);
    let streamed = StreamingSynthesizeResult {
        sample_rate_hz: SAMPLE_RATE_HZ,
        generated_frames: 2,
        generated_samples: 3840,
    };
    assert_eq!(streamed, streamed);

    // The talker KV mode has two names and some aliases; anything else is refused with the text.
    assert_eq!(
        (
            TalkerKvMode::default(),
            TalkerKvMode::F16.as_str(),
            TalkerKvMode::TurboQuant.as_str()
        ),
        (TalkerKvMode::F16, "f16", "turboquant")
    );
    for (name, mode) in [
        ("f16", TalkerKvMode::F16),
        (" F16 ", TalkerKvMode::F16),
        ("turboquant", TalkerKvMode::TurboQuant),
        ("turbo", TalkerKvMode::TurboQuant),
        ("q8_0", TalkerKvMode::TurboQuant),
        ("Q8", TalkerKvMode::TurboQuant),
    ] {
        assert_eq!(TalkerKvMode::parse(name).unwrap(), mode, "{name}");
    }
    assert_eq!(
        message(&TalkerKvMode::parse(" Loud ").unwrap_err()),
        "invalid input: unknown talker KV mode 'loud' (expected f16 or turboquant)"
    );

    // Modes and preset speakers.
    assert_eq!(QtsMode::default(), QtsMode::Base);
    assert_eq!(
        [QtsMode::Base, QtsMode::CustomVoice, QtsMode::VoiceDesign].map(QtsMode::as_str),
        ["base", "customvoice", "voicedesign"]
    );
    assert_eq!(PresetSpeaker::ALL.len(), 9);
    assert_eq!(
        PresetSpeaker::ALL.map(PresetSpeaker::as_str),
        ["vivian", "serena", "uncle_fu", "dylan", "eric", "ryan", "aiden", "ono_anna", "sohee"]
    );
    for speaker in PresetSpeaker::ALL {
        for spelled in [
            speaker.as_str().to_string(),
            speaker.as_str().to_uppercase(),
            format!("  {}  ", speaker.as_str()),
        ] {
            assert_eq!(PresetSpeaker::parse(&spelled), Some(speaker), "{spelled:?}");
        }
    }
    for refused in ["uncle-fu", "uncle fu", "", "nobody"] {
        assert_eq!(PresetSpeaker::parse(refused), None, "{refused:?}");
    }
}

fn mode_request_rules() {
    let prompt = VoiceClonePromptV2 {
        schema_version: VOICE_CLONE_PROMPT_V2_SCHEMA_VERSION,
        x_vector_only_mode: true,
        ref_spk_embedding: Some(TensorF32 {
            shape: vec![4],
            values: vec![0.0; 4],
        }),
        ..Default::default()
    };
    // The valid combinations of each mode.
    ModeSynthesizeRequest {
        voice_clone_prompt: Some(&prompt),
        speaker_embedding: Some(&[1.0; 4]),
        ..Default::default()
    }
    .validate()
    .unwrap();
    ModeSynthesizeRequest {
        mode: QtsMode::CustomVoice,
        speaker: Some(PresetSpeaker::Vivian),
        instruct: Some("hi".into()),
        ..Default::default()
    }
    .validate()
    .unwrap();
    ModeSynthesizeRequest {
        mode: QtsMode::VoiceDesign,
        instruct: Some("hi".into()),
        ..Default::default()
    }
    .validate()
    .unwrap();
    // What each mode requires.
    for (mode, expected) in [
        (QtsMode::Base, "Base needs a voice-clone prompt"),
        (QtsMode::CustomVoice, "CustomVoice needs a preset speaker"),
        (QtsMode::VoiceDesign, "VoiceDesign needs nonempty instruct"),
    ] {
        assert_eq!(
            invalid_input(
                ModeSynthesizeRequest {
                    mode,
                    ..Default::default()
                }
                .validate()
            ),
            expected
        );
    }
    assert_eq!(
        invalid_input(
            ModeSynthesizeRequest {
                mode: QtsMode::VoiceDesign,
                instruct: Some("  ".into()),
                ..Default::default()
            }
            .validate()
        ),
        "VoiceDesign needs nonempty instruct"
    );
    // What each mode rejects.
    let base_with = |speaker, instruct| ModeSynthesizeRequest {
        voice_clone_prompt: Some(&prompt),
        speaker,
        instruct,
        ..Default::default()
    };
    assert_eq!(
        invalid_input(base_with(Some(PresetSpeaker::Ryan), None).validate()),
        "Base does not accept preset speaker or instruct"
    );
    assert_eq!(
        invalid_input(base_with(None, Some("x".into())).validate()),
        "Base does not accept preset speaker or instruct"
    );
    let custom = ModeSynthesizeRequest {
        mode: QtsMode::CustomVoice,
        speaker: Some(PresetSpeaker::Ryan),
        ..Default::default()
    };
    assert!(invalid_input(
        ModeSynthesizeRequest {
            voice_clone_prompt: Some(&prompt),
            ..custom.clone()
        }
        .validate()
    )
    .starts_with("CustomVoice does not accept"));
    assert!(invalid_input(
        ModeSynthesizeRequest {
            speaker_embedding: Some(&[0.0; 4]),
            ..custom
        }
        .validate()
    )
    .starts_with("CustomVoice does not accept"));
    let design = ModeSynthesizeRequest {
        mode: QtsMode::VoiceDesign,
        instruct: Some("hi".into()),
        ..Default::default()
    };
    for bad in [
        ModeSynthesizeRequest {
            speaker: Some(PresetSpeaker::Ryan),
            ..design.clone()
        },
        ModeSynthesizeRequest {
            speaker_embedding: Some(&[0.0; 4]),
            ..design.clone()
        },
        ModeSynthesizeRequest {
            voice_clone_prompt: Some(&prompt),
            ..design.clone()
        },
    ] {
        assert!(invalid_input(bad.validate()).starts_with("VoiceDesign does not accept"));
    }
    // A prompt that is itself invalid is refused through the request.
    let broken = VoiceClonePromptV2 {
        schema_version: 1,
        ..prompt.clone()
    };
    assert!(invalid_input(
        ModeSynthesizeRequest {
            voice_clone_prompt: Some(&broken),
            ..Default::default()
        }
        .validate()
    )
    .contains("schema_version: 1"));
}

fn voice_clone_prompts() {
    assert_eq!(VOICE_CLONE_PROMPT_V2_SCHEMA_VERSION, 2);
    let xvector = VoiceClonePromptV2 {
        schema_version: 2,
        source: "fixture".into(),
        model_id: "model".into(),
        speaker_encoder_sample_rate_hz: 16_000,
        x_vector_only_mode: true,
        ref_spk_embedding: Some(TensorF32 {
            shape: vec![4],
            values: vec![0.5, -0.5, 0.25, 1.0],
        }),
        ..Default::default()
    };
    xvector.validate().unwrap();
    assert_eq!(xvector.speaker_embedding(), [0.5, -0.5, 0.25, 1.0]);
    assert_eq!(xvector.speaker_embedding_dim(), Some(4));
    assert_eq!(
        (xvector.ref_code_shape(), xvector.ref_code_values()),
        (None, None)
    );
    let bytes = xvector.to_protobuf_vec().unwrap();
    assert_eq!(
        VoiceClonePromptV2::from_protobuf_bytes(&bytes).unwrap(),
        xvector
    );

    // In-context prompts carry the reference text and a [frames, codebooks] code tensor.
    let icl = VoiceClonePromptV2 {
        schema_version: 2,
        icl_mode: true,
        ref_text: "hello there".into(),
        ref_code: Some(TensorI32 {
            shape: vec![2, 3],
            values: vec![1, -2, 3, 4, 5, 6],
        }),
        ref_spk_embedding: Some(TensorF32 {
            shape: vec![2],
            values: vec![0.1, 0.2],
        }),
        ..Default::default()
    };
    icl.validate().unwrap();
    assert_eq!(
        (icl.ref_code_shape(), icl.ref_code_values()),
        (Some((2, 3)), Some(&[1, -2, 3, 4, 5, 6][..]))
    );
    assert_eq!(
        VoiceClonePromptV2::from_protobuf_bytes(&icl.to_protobuf_vec().unwrap()).unwrap(),
        icl
    );
    assert_eq!(
        VoiceClonePromptV2::default().speaker_embedding(),
        [] as [f32; 0]
    );
    assert_eq!(VoiceClonePromptV2::default().speaker_embedding_dim(), None);

    let with = |change: &dyn Fn(&mut VoiceClonePromptV2)| {
        let mut prompt = xvector.clone();
        change(&mut prompt);
        invalid_input(prompt.validate())
    };
    assert_eq!(
        with(&|p| p.schema_version = 1),
        "unsupported voice clone prompt schema_version: 1"
    );
    assert_eq!(
        with(&|p| p.ref_spk_embedding = None),
        "voice clone prompt ref_spk_embedding is required"
    );
    assert_eq!(
        with(&|p| p.ref_spk_embedding = Some(TensorF32 {
            shape: vec![2, 2],
            values: vec![0.0; 4]
        })),
        "voice clone prompt ref_spk_embedding must be a 1D tensor"
    );
    assert_eq!(
        with(&|p| p.ref_spk_embedding = Some(TensorF32 {
            shape: vec![4],
            values: vec![0.0; 3]
        })),
        "voice clone prompt ref_spk_embedding.values length 3 does not match shape product 4"
    );
    assert_eq!(
        with(&|p| p.ref_spk_embedding = Some(TensorF32 {
            shape: vec![0],
            values: vec![]
        })),
        "voice clone prompt ref_spk_embedding.shape must not contain zero"
    );
    assert_eq!(
        with(&|p| p.ref_spk_embedding = Some(TensorF32 {
            shape: vec![],
            values: vec![]
        })),
        "voice clone prompt ref_spk_embedding.shape must not be empty"
    );
    assert_eq!(
        with(&|p| p.icl_mode = true),
        "voice clone prompt must use exactly one of x_vector_only_mode or icl_mode"
    );
    assert_eq!(
        with(&|p| p.x_vector_only_mode = false),
        "voice clone prompt must use exactly one of x_vector_only_mode or icl_mode"
    );
    assert_eq!(
        with(&|p| p.ref_code = Some(TensorI32 {
            shape: vec![1, 1],
            values: vec![7]
        })),
        "voice clone prompt ref_code must be empty in x-vector-only mode"
    );
    let with_icl = |change: &dyn Fn(&mut VoiceClonePromptV2)| {
        let mut prompt = icl.clone();
        change(&mut prompt);
        invalid_input(prompt.validate())
    };
    assert_eq!(
        with_icl(&|p| p.ref_text = "  ".into()),
        "voice clone prompt ref_text is required in icl_mode"
    );
    assert_eq!(
        with_icl(&|p| p.ref_code = None),
        "voice clone prompt ref_code is required in icl_mode"
    );
    assert_eq!(
        with_icl(&|p| p.ref_code = Some(TensorI32 {
            shape: vec![6],
            values: vec![0; 6]
        })),
        "voice clone prompt ref_code must be a 2D tensor [frames, codebooks]"
    );
    // Serialising validates, and garbage bytes are refused with a protobuf error.
    assert!(VoiceClonePromptV2 {
        schema_version: 9,
        ..xvector
    }
    .to_protobuf_vec()
    .is_err());
    assert!(invalid_input(
        VoiceClonePromptV2::from_protobuf_bytes(&[0xff, 0xff, 0xff]).map(|_| ())
    )
    .starts_with("invalid voice clone prompt protobuf: "));
    assert!(
        VoiceClonePromptV2::from_protobuf_bytes(&[]).is_err(),
        "an empty message is a schema 0 prompt"
    );
}

fn model_paths(scratch: &Path) {
    // The 0.6B layout: the first quantisation present wins, f16 is the name when none exists.
    let none = ModelPaths::from_model_dir("/no/such/models");
    assert!(
        none.main_gguf.ends_with("qwen3-tts-0.6b-f16.gguf")
            && none.vocoder_onnx.ends_with("qwen3-tts-vocoder.onnx")
    );
    assert!(!none.main_exists() && !none.vocoder_exists());
    let dir = scratch.join("paths");
    write(&dir.join("qwen3-tts-0.6b-q4_k.gguf"), b"q4");
    assert!(ModelPaths::from_model_dir(&dir)
        .main_gguf
        .ends_with("qwen3-tts-0.6b-q4_k.gguf"));
    write(&dir.join("qwen3-tts-0.6b-q8_0.gguf"), b"q8");
    write(&dir.join("qwen3-tts-vocoder.onnx"), b"onnx");
    let paths = ModelPaths::from_model_dir(&dir);
    assert!(
        paths.main_gguf.ends_with("qwen3-tts-0.6b-q8_0.gguf"),
        "q8_0 is tried before q4_k"
    );
    assert!(paths.main_exists() && paths.vocoder_exists());
    let explicit = ModelPaths::new(PathBuf::from("a.gguf"), PathBuf::from("v.onnx"));
    assert_eq!(
        (explicit.main_gguf, explicit.vocoder_onnx),
        (PathBuf::from("a.gguf"), PathBuf::from("v.onnx"))
    );

    // The 1.7B layouts: per-component files by default, a bundled export when one exists.
    for mode in [QtsMode::Base, QtsMode::CustomVoice, QtsMode::VoiceDesign] {
        let stem = format!("qwen3-tts-1.7b-{}", mode.as_str());
        let paths = ModeModelPaths::from_model_dir("/no/such/models", mode);
        assert!(paths
            .talker_gguf
            .ends_with(format!("{stem}-talker-f16.gguf")));
        assert!(paths
            .code_predictor_gguf
            .ends_with(format!("{stem}-code-predictor-f16.gguf")));
        assert!(paths
            .tokenizer_gguf
            .ends_with(format!("{stem}-tokenizer.gguf")));
        assert!(paths.vocoder_onnx.ends_with("qwen3-tts-vocoder.onnx"));
    }
    let moved = ModeModelPaths::from_model_dir("/m", QtsMode::VoiceDesign)
        .with_vocoder("/elsewhere/v.onnx");
    assert_eq!(moved.vocoder_onnx, PathBuf::from("/elsewhere/v.onnx"));
    let split = scratch.join("split");
    write(
        &split.join("qwen3-tts-1.7b-customvoice-talker-q4_k.gguf"),
        b"",
    );
    write(
        &split.join("qwen3-tts-1.7b-customvoice-code-predictor-q8_0.gguf"),
        b"",
    );
    let resolved = ModeModelPaths::from_model_dir(&split, QtsMode::CustomVoice);
    assert!(resolved
        .talker_gguf
        .ends_with("qwen3-tts-1.7b-customvoice-talker-q4_k.gguf"));
    assert!(resolved
        .code_predictor_gguf
        .ends_with("qwen3-tts-1.7b-customvoice-code-predictor-q8_0.gguf"));
    write(&split.join("qwen3-tts-1.7b-customvoice-q8_0.gguf"), b"");
    let bundled = ModeModelPaths::from_model_dir(&split, QtsMode::CustomVoice);
    assert!(bundled
        .talker_gguf
        .ends_with("qwen3-tts-1.7b-customvoice-q8_0.gguf"));
    assert_eq!(
        (&bundled.talker_gguf, &bundled.talker_gguf),
        (&bundled.code_predictor_gguf, &bundled.tokenizer_gguf)
    );
}

fn gguf_container(scratch: &Path) {
    let weights_a: &[f32] = &[1.0, 2.0, 3.0, 4.0];
    let weights_b: &[f32] = &[0.5, 1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5];
    let bytes = gguf(
        &[
            ("general.architecture", Value::Str("qwen3-tts")),
            ("qwen3-tts.tts_model_type", Value::Str("customvoice")),
            ("qwen3-tts.block_count", Value::U32(28)),
            ("qwen3-tts.rope_theta", Value::F32(1_000_000.0)),
            (
                "tokenizer.ggml.tokens",
                Value::Strs(&["one", "two", "three"]),
            ),
        ],
        &[
            ("weights.a", &[4], weights_a),
            ("weights.b", &[4, 2], weights_b),
        ],
    );
    let path = scratch.join("container.gguf");
    write(&path, &bytes);
    let file = GgufFile::open(&path).expect("a written GGUF file opens");
    assert_eq!(file.path(), path);
    assert!(!file.ctx_ptr().is_null() && file.meta_ctx_ptr().is_some());
    assert_eq!(file.n_tensors(), 2);
    assert_eq!(file.data_offset() % 32, 0);
    assert_eq!(
        file.data_offset() + 32 + 32,
        bytes.len(),
        "metadata, then two 32-byte padded tensors"
    );

    // Metadata of each kind; a missing key or a key of another type gives the default or None.
    assert!(
        file.key_index("qwen3-tts.block_count").is_some() && file.key_index("absent").is_none()
    );
    assert_eq!(file.get_u32("qwen3-tts.block_count", 0), 28);
    assert_eq!(file.get_u32("absent", 9), 9);
    assert_eq!(file.get_f32("qwen3-tts.rope_theta", 0.0), 1_000_000.0);
    assert_eq!(file.get_f32("absent", 2.5), 2.5);
    assert_eq!(
        file.get_str("general.architecture").as_deref(),
        Some("qwen3-tts")
    );
    assert_eq!(
        file.get_str("qwen3-tts.block_count"),
        None,
        "a number is not a string"
    );
    assert_eq!(file.get_str("absent"), None);
    assert_eq!(
        file.get_arr_str("tokenizer.ggml.tokens"),
        Some(vec!["one".to_string(), "two".into(), "three".into()])
    );
    assert_eq!(file.get_arr_str("absent"), None);

    // Tensors: shape, type, placement, and contents.
    let info = file.tensor_info("weights.b").unwrap();
    assert_eq!(
        (
            info.name.as_str(),
            info.dims.clone(),
            info.offset,
            info.size,
            info.ty
        ),
        ("weights.b", vec![4, 2], 32, 32, 0)
    );
    let (info, values) = file.read_tensor_f32("weights.a").unwrap();
    assert_eq!((info.dims, info.offset, info.size), (vec![4], 0, 16));
    assert_eq!(values, weights_a);
    assert_eq!(file.read_tensor_f32("weights.b").unwrap().1, weights_b);
    let (_, raw) = file.read_tensor_bytes("weights.a").unwrap();
    assert_eq!(
        raw,
        weights_a
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>()
    );
    assert!(
        matches!(file.tensor_info("nope"), Err(Qwen3TtsError::MissingTensor(name)) if name == "nope")
    );
    assert!(matches!(
        file.read_tensor_f32("nope"),
        Err(Qwen3TtsError::MissingTensor(_))
    ));
    assert!(matches!(
        file.read_tensor_bytes("nope"),
        Err(Qwen3TtsError::MissingTensor(_))
    ));

    // A file without tensors still opens; foreign and missing files are refused as invalid GGUF.
    let bare = scratch.join("bare.gguf");
    write(
        &bare,
        &gguf(&[("general.architecture", Value::Str("x"))], &[]),
    );
    let bare_file = GgufFile::open(&bare).unwrap();
    assert_eq!(bare_file.n_tensors(), 0);
    assert!(matches!(
        bare_file.tensor_info("weights.a"),
        Err(Qwen3TtsError::InvalidGguf(_) | Qwen3TtsError::MissingTensor(_))
    ));
    let junk = scratch.join("junk.gguf");
    write(&junk, b"definitely not a GGUF container");
    assert!(
        matches!(GgufFile::open(&junk), Err(Qwen3TtsError::InvalidGguf(found)) if found == junk)
    );
    let absent = scratch.join("absent.gguf");
    assert!(
        matches!(GgufFile::open(&absent), Err(Qwen3TtsError::InvalidGguf(found)) if found == absent)
    );

    // load_and_validate: the checkpoint, then the vocoder, then the container.
    let checkpoint = scratch.join("validate").join("main.gguf");
    let vocoder = scratch.join("validate").join("vocoder.onnx");
    let paths = ModelPaths::new(checkpoint.clone(), vocoder.clone());
    assert!(
        matches!(load_and_validate(&paths), Err(Qwen3TtsError::ModelFile(found)) if found == checkpoint)
    );
    write(&checkpoint, &bytes);
    assert!(
        matches!(load_and_validate(&paths), Err(Qwen3TtsError::ModelFile(found)) if found == vocoder)
    );
    write(&vocoder, b"onnx placeholder");
    load_and_validate(&paths).expect("a GGUF checkpoint and a vocoder file validate");
    write(&checkpoint, b"not gguf");
    assert!(matches!(
        load_and_validate(&paths),
        Err(Qwen3TtsError::InvalidGguf(_))
    ));
}

fn tokenizer(scratch: &Path) {
    let defaults = TokenizerConfig::default();
    assert_eq!(
        (
            defaults.vocab_size,
            defaults.pad_token_id,
            defaults.eos_token_id,
            defaults.bos_token_id
        ),
        (151_936, 151_643, 151_645, 151_644)
    );
    let path = scratch.join("tokenizer.gguf");
    tokenizer_file(&path, &TOKENS, &["h i"]);
    let tokenizer = TextTokenizer::load_from_gguf(&GgufFile::open(&path).unwrap()).unwrap();
    let config = tokenizer.config();
    assert_eq!(
        (
            config.vocab_size,
            config.pad_token_id,
            config.eos_token_id,
            config.bos_token_id
        ),
        (9, 0, 2, 1)
    );

    // Byte-level BPE: the merge joins h and i; with no merge the pieces stay single characters.
    assert_eq!(tokenizer.encode("hi"), [8]);
    assert_eq!(tokenizer.encode("hh"), [5, 5]);
    assert_eq!(tokenizer.encode("ih"), [6, 5]);
    assert!(tokenizer.encode("").is_empty());
    // The TTS prompt layouts: bos assistant newline, text, eos newline, then the assistant prefix again.
    assert_eq!(tokenizer.encode_for_tts("hi"), [1, 3, 4, 8, 2, 4, 1, 3, 4]);
    assert_eq!(tokenizer.encode_ref_for_tts("hi"), [1, 3, 4, 8, 2, 4]);
    assert_eq!(
        tokenizer.encode_for_voice_clone("hi", "hh"),
        [1, 3, 4, 8, 2, 4, 1, 3, 4, 5, 5, 2, 4, 1, 3, 4]
    );
    assert_eq!(
        tokenizer.encode_instruction_for_tts("hi").unwrap(),
        [1, 7, 4, 8, 2, 4]
    );
    // Decoding maps ids back to text; an unknown id is empty.
    assert_eq!(tokenizer.decode(&[8]), "hi");
    assert_eq!(tokenizer.decode(&[3, 4, 5, 6]), "assistant\nhi");
    assert_eq!(tokenizer.decode_token(7), "user");
    assert_eq!(tokenizer.decode_token(99), "");

    // An instruction needs a `user` token; without one it is refused.
    let no_user = scratch.join("no-user.gguf");
    tokenizer_file(&no_user, &TOKENS[..7], &[]);
    let without = TextTokenizer::load_from_gguf(&GgufFile::open(&no_user).unwrap()).unwrap();
    assert!(
        matches!(without.encode_instruction_for_tts("hi"), Err(Qwen3TtsError::MissingTokenizerToken(name)) if name == "user")
    );
    assert_eq!(
        without.encode_for_tts("hi"),
        [1, 3, 4, 5, 6, 2, 4, 1, 3, 4],
        "no merges: single characters"
    );

    // Load refusals, each with its Display text.
    let load = |name: &str, tokens: Option<&[&str]>| {
        let path = scratch.join(name);
        match tokens {
            Some(tokens) => tokenizer_file(&path, tokens, &[]),
            None => write(
                &path,
                &gguf(&[("general.architecture", Value::Str("x"))], &[]),
            ),
        }
        TextTokenizer::load_from_gguf(&GgufFile::open(&path).unwrap())
            .err()
            .expect("a refusal")
    };
    assert_eq!(
        message(&load("no-tokens.gguf", None)),
        "tokenizer error: tokenizer.ggml.tokens not found"
    );
    assert_eq!(
        message(&load("empty-vocab.gguf", Some(&[]))),
        "tokenizer error: empty vocabulary"
    );
    assert_eq!(
        message(&load("no-assistant.gguf", Some(&["<pad>", "\u{10a}"]))),
        "missing tokenizer token: assistant"
    );
    assert_eq!(
        message(&load("no-newline.gguf", Some(&["<pad>", "assistant"]))),
        "missing tokenizer token: \\n"
    );
}

fn wav_pcm16(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
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

/// A tone with a slow amplitude wobble, so frames differ a little, as speech does.
fn tone(rate: u32, seconds: f32, hertz: f32) -> Vec<i16> {
    (0..(rate as f32 * seconds) as usize)
        .map(|i| {
            let t = i as f32 / rate as f32;
            let wobble = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 3.0 * t).sin();
            ((2.0 * std::f32::consts::PI * hertz * t).sin() * wobble * 20_000.0) as i16
        })
        .collect()
}

fn speaker_encoder() {
    let defaults = SpeakerEncoderConfig::default();
    assert_eq!(
        (
            defaults.sample_rate_hz,
            defaults.frame_len,
            defaults.hop_len,
            defaults.fft_len,
            defaults.mel_bins
        ),
        (16_000, 400, 160, 512, 32)
    );
    assert_eq!(
        (
            defaults.trim_amplitude,
            defaults.min_duration_ms,
            defaults.max_duration_ms,
            defaults.output_rms
        ),
        (0.02, 250, 8_000, 0.25)
    );
    assert_eq!(
        message(&SpeakerEncoder::new(0).err().unwrap()),
        "invalid input: speaker encoder target dimension must be non-zero"
    );
    for config in [
        SpeakerEncoderConfig {
            hop_len: 0,
            ..defaults.clone()
        },
        SpeakerEncoderConfig {
            mel_bins: 0,
            ..defaults.clone()
        },
        SpeakerEncoderConfig {
            fft_len: 100,
            ..defaults.clone()
        },
    ] {
        assert_eq!(
            message(&SpeakerEncoder::with_config(16, config).err().unwrap()),
            "invalid input: speaker encoder config is invalid"
        );
    }

    let encoder = SpeakerEncoder::new(64).unwrap();
    assert_eq!(encoder.target_dim(), 64);
    let low = encoder
        .encode_wav_bytes(&wav_pcm16(16_000, 1, &tone(16_000, 1.0, 180.0)))
        .unwrap();
    let high = encoder
        .encode_wav_bytes(&wav_pcm16(16_000, 1, &tone(16_000, 1.0, 2_400.0)))
        .unwrap();
    for embedding in [&low, &high] {
        assert_eq!(embedding.len(), 64);
        assert!(embedding.iter().all(|v| v.is_finite()));
        let mean = embedding.iter().sum::<f32>() / 64.0;
        let rms = (embedding.iter().map(|v| v * v).sum::<f32>() / 64.0).sqrt();
        assert!(
            mean.abs() < 1e-4 && (rms - 0.25).abs() < 1e-3,
            "mean {mean} rms {rms}"
        );
    }
    // The same audio gives the same embedding; different audio gives a different one.
    assert_eq!(
        low,
        encoder
            .encode_wav_bytes(&wav_pcm16(16_000, 1, &tone(16_000, 1.0, 180.0)))
            .unwrap()
    );
    let spread = low
        .iter()
        .zip(&high)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        spread > 0.01,
        "two different tones must not share an embedding (largest difference {spread})"
    );
    // Other rates and layouts are accepted: resampled and averaged to mono.
    assert_eq!(
        encoder
            .encode_wav_bytes(&wav_pcm16(8_000, 1, &tone(8_000, 1.0, 180.0)))
            .unwrap()
            .len(),
        64
    );
    let stereo: Vec<i16> = tone(16_000, 1.0, 180.0)
        .iter()
        .flat_map(|s| [*s, *s])
        .collect();
    assert_eq!(
        encoder
            .encode_wav_bytes(&wav_pcm16(16_000, 2, &stereo))
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        SpeakerEncoder::new(8)
            .unwrap()
            .encode_wav_bytes(&wav_pcm16(16_000, 1, &tone(16_000, 1.0, 180.0)))
            .unwrap()
            .len(),
        8
    );

    // Refusals: not a WAV, silence, and audio that is too short once trimmed.
    let refusal = |bytes: Vec<u8>| message(&encoder.encode_wav_bytes(&bytes).unwrap_err());
    assert!(refusal(b"not a wav file".to_vec())
        .starts_with("invalid input: reference audio must be a valid WAV file"));
    assert_eq!(
        refusal(wav_pcm16(16_000, 1, &vec![0; 16_000])),
        "invalid input: reference audio is silent or degenerate"
    );
    assert_eq!(
        refusal(wav_pcm16(16_000, 1, &[])),
        "invalid input: reference WAV does not contain enough audio frames"
    );
    assert_eq!(
        refusal(wav_pcm16(16_000, 1, &tone(16_000, 0.1, 180.0))),
        "invalid input: reference audio is too short; need at least 250 ms of voiced audio"
    );
}

fn timings_and_configs() {
    let one = SynthesisStageTimings {
        speaker_encode: Duration::from_millis(10),
        tokenize: Duration::from_millis(20),
        prefill_build: Duration::from_millis(30),
        codec_rollout: Duration::from_millis(400),
        vocoder_decode: Duration::from_millis(100),
        post: Duration::from_millis(40),
        pipeline_overlap: Duration::from_millis(50),
        first_frame_latency: Duration::from_millis(60),
        codec_rollout_detail: CodecRolloutSubTimings::default(),
        generated_samples: 48_000,
        sample_rate_hz: 24_000,
    };
    assert_eq!(
        one.total(),
        Duration::from_millis(550),
        "the sum less the pipeline overlap"
    );
    assert_eq!(SynthesisStageTimings::default().total(), Duration::ZERO);
    assert_eq!(SynthesisStageTimings::average(&[]), None);
    let other = SynthesisStageTimings {
        codec_rollout: Duration::from_millis(600),
        generated_samples: 24_000,
        ..one.clone()
    };
    let mean = SynthesisStageTimings::average(&[one.clone(), other]).unwrap();
    assert_eq!(mean.codec_rollout, Duration::from_millis(500));
    assert_eq!(
        (mean.generated_samples, mean.sample_rate_hz, mean.tokenize),
        (36_000, 24_000, Duration::from_millis(20))
    );
    let table = one.format_table();
    assert!(
        table.starts_with("synthesis stage timings (wall clock, single run)"),
        "{table}"
    );
    for row in [
        "speaker_encode",
        "tokenize",
        "prefill_build",
        "codec_rollout (transformer)",
        "vocoder_decode",
        "post (flatten/trim)",
        "pipeline_overlap",
    ] {
        assert!(table.contains(row), "{row}: {table}");
    }

    assert_eq!(BackendKind::Cpu.as_str(), "CPU");
    assert_eq!(
        (
            VocoderExecutionProvider::Cpu.as_str(),
            VocoderExecutionProvider::Cpu.display_str()
        ),
        ("cpu", "ORT/CPU")
    );
    let vocoder = VocoderConfig::default();
    assert_eq!(
        (
            vocoder.sample_rate,
            vocoder.n_codebooks,
            vocoder.codebook_size,
            vocoder.codebook_dim,
            vocoder.latent_dim
        ),
        (24_000, 16, 2_048, 256, 1_024)
    );
    assert_eq!(
        (
            vocoder.hidden_dim,
            vocoder.n_pre_tfm_layers,
            vocoder.n_heads,
            vocoder.ffn_dim,
            vocoder.decoder_dim
        ),
        (512, 8, 16, 1_024, 1_536)
    );
    let talker = TtsTransformerConfig::default();
    assert_eq!(
        (
            talker.hidden_size,
            talker.n_layers,
            talker.n_attention_heads,
            talker.n_key_value_heads,
            talker.head_dim
        ),
        (1_024, 28, 16, 8, 128)
    );
    assert_eq!(
        (
            talker.n_codebooks,
            talker.codec_vocab_size,
            talker.english_language_id
        ),
        (16, 3_072, 2_050)
    );
    let predictor = CodePredictorConfig {
        hidden_size: 1_024,
        n_layers: 5,
        n_attention_heads: 16,
        n_key_value_heads: 8,
        intermediate_size: 3_072,
        head_dim: 128,
        rms_norm_eps: 1e-6,
        rope_theta: 1_000_000.0,
        vocab_size: 2_048,
    };
    assert_eq!(predictor.clone().n_layers, 5);

    // Streaming sinks are any closure over a PCM chunk.
    let mut received = Vec::new();
    let mut sink = |chunk: &[f32]| -> Result<(), Qwen3TtsError> {
        received.extend_from_slice(chunk);
        Ok(())
    };
    sink.push_pcm_chunk(&[0.25, -0.25]).unwrap();
    sink.push_pcm_chunk(&[0.5]).unwrap();
    assert_eq!(received, [0.25, -0.25, 0.5]);
    let mut failing = |_: &[f32]| -> Result<(), Qwen3TtsError> {
        Err(Qwen3TtsError::InvalidInput("stop".into()))
    };
    assert!(failing.push_pcm_chunk(&[0.0]).is_err());
}

fn engine_refusals(scratch: &Path) {
    // Every refusal below happens before a transformer, a ggml backend or an ONNX session exists.
    let dir = scratch.join("engine");
    let main = dir.join("main.gguf");
    let vocoder = dir.join("vocoder.onnx");
    let refusal = |result: Result<Qwen3TtsEngine, Qwen3TtsError>| {
        result.err().expect("a refusal before any model is built")
    };
    assert!(
        matches!(refusal(Qwen3TtsEngine::load(ModelPaths::new(main.clone(), vocoder.clone()))), Qwen3TtsError::ModelFile(found) if found == main)
    );
    write(&main, b"not gguf");
    assert!(
        matches!(refusal(Qwen3TtsEngine::load(ModelPaths::new(main.clone(), vocoder.clone()))), Qwen3TtsError::ModelFile(found) if found == vocoder)
    );
    write(&vocoder, b"onnx placeholder");
    assert!(matches!(
        refusal(Qwen3TtsEngine::load(ModelPaths::new(
            main.clone(),
            vocoder.clone()
        ))),
        Qwen3TtsError::InvalidGguf(_)
    ));
    // A GGUF with no vocabulary stops at the tokenizer.
    write(
        &main,
        &gguf(&[("general.architecture", Value::Str("qwen3-tts"))], &[]),
    );
    assert_eq!(
        message(&refusal(Qwen3TtsEngine::load(ModelPaths::new(
            main.clone(),
            vocoder.clone()
        )))),
        "tokenizer error: tokenizer.ggml.tokens not found"
    );
    assert!(matches!(
        refusal(Qwen3TtsEngine::from_model_dir(scratch.join("no-models"))),
        Qwen3TtsError::ModelFile(_)
    ));

    // Mode loads: the checkpoint's own model type must match the mode asked for.
    let customvoice = dir.join("customvoice.gguf");
    write(
        &customvoice,
        &gguf(
            &[("qwen3-tts.tts_model_type", Value::Str("custom_voice"))],
            &[],
        ),
    );
    let paths = |talker: &Path, predictor: &Path, tokenizer: &Path| ModeModelPaths {
        talker_gguf: talker.into(),
        code_predictor_gguf: predictor.into(),
        tokenizer_gguf: tokenizer.into(),
        vocoder_onnx: vocoder.clone(),
    };
    let mismatch = refusal(Qwen3TtsEngine::load_mode(
        paths(&customvoice, &customvoice, &customvoice),
        QtsMode::Base,
    ));
    assert_eq!(
        message(&mismatch),
        "invalid input: requested mode does not match GGUF model type"
    );
    let missing = dir.join("missing-predictor.gguf");
    assert!(matches!(
        refusal(Qwen3TtsEngine::load_mode(paths(&customvoice, &missing, &customvoice), QtsMode::CustomVoice)),
        Qwen3TtsError::InvalidGguf(found) if found == missing
    ));
    assert!(matches!(
        refusal(Qwen3TtsEngine::load_mode(
            paths(&dir.join("absent-talker.gguf"), &customvoice, &customvoice),
            QtsMode::Base
        )),
        Qwen3TtsError::InvalidGguf(_)
    ));
    assert_eq!(
        message(&refusal(Qwen3TtsEngine::load_mode(
            paths(&customvoice, &customvoice, &customvoice),
            QtsMode::CustomVoice
        ))),
        "tokenizer error: tokenizer.ggml.tokens not found"
    );
    assert!(matches!(
        refusal(Qwen3TtsEngine::from_model_dir_for_mode(
            scratch.join("no-models"),
            QtsMode::VoiceDesign
        )),
        Qwen3TtsError::InvalidGguf(_)
    ));
    assert!(matches!(
        refusal(Qwen3TtsEngine::from_model_dir_for_mode_with_vocoder(
            scratch.join("no-models"),
            vocoder.clone(),
            QtsMode::CustomVoice
        )),
        Qwen3TtsError::InvalidGguf(_)
    ));

    // The vocoder refuses a missing model before ONNX Runtime is touched.
    let absent = scratch.join("absent-vocoder.onnx");
    assert!(
        matches!(Vocoder::load_from_onnx(&absent).err().unwrap(), Qwen3TtsError::ModelFile(found) if found == absent)
    );
}

fn error_text() {
    let path = PathBuf::from("models/x.gguf");
    for (error, text) in [
        (
            Qwen3TtsError::Io(std::io::Error::other("disk")),
            "IO error: disk".to_string(),
        ),
        (Qwen3TtsError::InvalidPath, "invalid UTF-8 in path".into()),
        (
            Qwen3TtsError::ModelFile(path.clone()),
            format!("missing or unreadable model file: {}", path.display()),
        ),
        (
            Qwen3TtsError::InvalidGguf(path.clone()),
            format!("file is not a valid GGUF container: {}", path.display()),
        ),
        (
            Qwen3TtsError::InvalidOnnx(path.clone()),
            format!("file is not a valid ONNX model: {}", path.display()),
        ),
        (
            Qwen3TtsError::Tokenizer("t".into()),
            "tokenizer error: t".into(),
        ),
        (
            Qwen3TtsError::MissingTokenizerToken("tok".into()),
            "missing tokenizer token: tok".into(),
        ),
        (
            Qwen3TtsError::InvalidInput("i".into()),
            "invalid input: i".into(),
        ),
        (
            Qwen3TtsError::MissingTensor("w".into()),
            "missing GGUF tensor: w".into(),
        ),
        (
            Qwen3TtsError::InvalidTensor("w".into()),
            "invalid GGUF tensor: w".into(),
        ),
        (
            Qwen3TtsError::UnsupportedTensorType("w".into()),
            "unsupported GGUF tensor type for w".into(),
        ),
        (
            Qwen3TtsError::NotImplemented("stage"),
            "stage not implemented yet: stage".into(),
        ),
        (
            Qwen3TtsError::Ort("o".into()),
            "onnx runtime error: o".into(),
        ),
    ] {
        assert_eq!(error.to_string(), text);
    }
    let nul = std::ffi::CString::new("a\0b").unwrap_err();
    assert!(Qwen3TtsError::from(nul)
        .to_string()
        .starts_with("invalid string: "));
    assert!(matches!(
        Qwen3TtsError::from(std::io::Error::other("x")),
        Qwen3TtsError::Io(_)
    ));
}

fn main() {
    let scratch = std::env::temp_dir().join(format!("rk-qwen-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("create consumer temp dir");
    requests_and_modes();
    mode_request_rules();
    voice_clone_prompts();
    model_paths(&scratch);
    gguf_container(&scratch);
    tokenizer(&scratch);
    speaker_encoder();
    timings_and_configs();
    engine_refusals(&scratch);
    error_text();
    std::fs::remove_dir_all(&scratch).expect("remove consumer temp dir");
    println!("OK rightkit-tts-qwen 0.2.7 (registry consumer)");
}
