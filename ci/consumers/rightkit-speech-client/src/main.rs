// rightkit-speech-client 0.4.0 registry consumer, outside the workspace.
//
// Exercises the direct admission/transport surface a suite author uses without
// connecting, launching, or mocking any OS transport: role ranking and wire
// names, endpoint/path derivation under a per-run temp root, empty-root
// discovery (no live record, HostMyself plan), takeover publish/read/clear,
// TranscribeOptions builders, preflight checks, SRT cues, the 0.4.0 TimedWord
// wire (optional source_ms), schema allowlists, capability predicates, engine
// selection, and capture arbitration rules.
// State: per-run temp dir removed on drop. No network, pipe, process spawn,
// daemon transport, or real user data.
use rightkit_speech_client::{
    decide as decide_capture, endpoint_for, installed_heardright_path, is_allowed_schema,
    is_allowed_schema_for_minor, parse_srt_cues, pid_alive, plan_host, preflight_file,
    read_live_record, read_takeover, request_takeover, should_request_takeover,
    takeover_obliges_yield, Capabilities, CaptureRole, ClientError, EngineInfo, EngineSelector,
    FileTranscript, HostPlan, HostRole, ServiceRecord, StageInput, TimedWord, TranscribeOptions,
    CAPABILITY_CANCEL, CAPABILITY_CAPTURE, CAPABILITY_QUEUE, CAPABILITY_VERBATIM,
    DEFAULT_STAGE_LIMIT_BYTES,
};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

type Failure = Box<dyn std::error::Error>;

fn temp_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("rk-speech-consumer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp root");
    root
}

fn main() -> Result<(), Failure> {
    // Roles: rank order shell < headless < scrape < cut; wire names round-trip.
    assert!(HostRole::HeardRightShell.outranks(HostRole::CutRight));
    assert!(!HostRole::CutRight.outranks(HostRole::HeardRightShell));
    assert_eq!(HostRole::from_wire("heard_right_shell"), Some(HostRole::HeardRightShell));
    assert_eq!(HostRole::from_wire("bogus"), None);
    assert_eq!(HostRole::parse_env("heardright"), Some(HostRole::HeardRightShell));
    assert_eq!(HostRole::ScrapeRight.as_str(), "scrape_right");
    assert_eq!(HostRole::rank_table()["cut_right"], 3);

    // Paths and endpoint derive under the root; endpoint names the user tag.
    let root = temp_root();
    let endpoint = endpoint_for(&root, "qa-user");
    assert!(endpoint.contains("qa-user"), "endpoint names the user tag");
    assert!(installed_heardright_path(&root).starts_with(&root));
    assert!(read_live_record(&root).is_none(), "empty root has no live record");
    assert!(pid_alive(std::process::id()), "own pid is alive");

    // Empty-root admission: a CutRight caller hosts itself; nothing is launched.
    assert_eq!(plan_host(&root, HostRole::CutRight), HostPlan::HostMyself);

    // Takeover rules against synthetic records; publish/read/clear touches only temp root.
    let other = ServiceRecord {
        protocol_major: 1,
        protocol_minor: 4,
        role: HostRole::CutRight,
        pid: std::process::id().wrapping_add(1),
        endpoint: endpoint.clone(),
        engine_version: "0.4.0".into(),
    };
    assert!(should_request_takeover(HostRole::HeardRightShell, Some(&other)));
    assert!(!should_request_takeover(HostRole::CutRight, Some(&other)));
    let me = std::process::id();
    assert!(takeover_obliges_yield(HostRole::CutRight, Some((0, me.wrapping_add(1))), me, &pid_alive));
    assert!(!takeover_obliges_yield(HostRole::HeardRightShell, Some((3, me.wrapping_add(1))), me, &pid_alive));
    request_takeover(&root, HostRole::ScrapeRight)?;
    let pending = read_takeover(&root).expect("takeover readable");
    assert_eq!(pending.role, HostRole::ScrapeRight);
    rightkit_speech_client::clear_takeover_if_ours(&root);

    // Transcribe options: builders set every public field; staging defaults hold.
    let opts = TranscribeOptions::new(Duration::from_secs(5))
        .engine(EngineSelector::parakeet())
        .language("en")
        .request_id("job-1")
        .verbatim()
        .stage(StageInput::Never)
        .stage_limit(1024);
    assert_eq!(opts.engine, Some(EngineSelector::parakeet()));
    assert_eq!(opts.language.as_deref(), Some("en"));
    assert_eq!(opts.request_id.as_deref(), Some("job-1"));
    assert!(opts.verbatim);
    assert_eq!(opts.stage_limit, 1024);
    assert_eq!(DEFAULT_STAGE_LIMIT_BYTES, 4 * 1024 * 1024 * 1024);
    assert_eq!(EngineSelector::parse("whisper"), EngineSelector::whisper());
    assert_eq!(EngineSelector::parse(""), EngineSelector::Auto);

    // Preflight: missing absolute path reports the host's own E_ENGINE message.
    let missing = root.join("no-such-input.wav");
    match preflight_file(&missing) {
        Err(ClientError::Engine { code, message }) => {
            assert_eq!(code, "E_ENGINE");
            assert!(message.contains("file not found"));
        }
        other => panic!("expected E_ENGINE, got {other:?}"),
    }
    assert!(preflight_file(std::path::Path::new("relative/input.wav")).is_ok());

    // Cues parse from the host's own SRT shape.
    let srt = "1\r\n00:00:00,000 --> 00:00:00,400\nhello world\n\n2\n00:00:01.000 --> 00:00:01,500\nsecond\n";
    let cues = parse_srt_cues(srt);
    assert_eq!(cues.len(), 2);
    assert_eq!(cues[0].text, "hello world");
    assert_eq!((cues[0].start_ms, cues[0].end_ms), (0, 400));
    let transcript = FileTranscript {
        text: "hello world".into(),
        srt: srt.into(),
        vtt: String::new(),
        words: vec![TimedWord::new("hello", 0, 200)],
    };
    assert_eq!(transcript.cues().len(), 2);

    // 0.4.0 wire: a 0.3 payload without source_ms parses with None and
    // serialises exactly as before; a sourced word round-trips its span.
    let legacy: FileTranscript = serde_json::from_value(json!({
        "text": "hi", "srt": "", "vtt": "",
        "words": [{"text": "hi", "start_ms": 0, "end_ms": 400}]
    }))?;
    assert_eq!(legacy.words, vec![TimedWord::new("hi", 0, 400)]);
    assert_eq!(legacy.words[0].source_ms, None);
    let wire = serde_json::to_value(&legacy)?;
    assert_eq!(wire["words"][0], json!({"text": "hi", "start_ms": 0, "end_ms": 400}));
    let mut mapped = TimedWord::new("hi", 100, 300);
    mapped.source_ms = Some((1000, 1200));
    let value = serde_json::to_value(&mapped)?;
    assert_eq!(value["source_ms"], json!([1000, 1200]));
    assert_eq!(serde_json::from_value::<TimedWord>(value)?, mapped);

    // Schemas and capabilities stay version-gated.
    assert!(is_allowed_schema("file_transcription_request"));
    assert!(!is_allowed_schema("request_cancel"));
    assert!(is_allowed_schema_for_minor("request_cancel", 4));
    assert!(!is_allowed_schema_for_minor("request_cancel", 3));
    let caps = Capabilities {
        capabilities: vec![
            CAPABILITY_VERBATIM.into(),
            CAPABILITY_QUEUE.into(),
            CAPABILITY_CANCEL.into(),
            CAPABILITY_CAPTURE.into(),
            "file_transcription".into(),
        ],
        engines: vec![EngineInfo {
            id: "parakeet".into(),
            languages: vec!["en".into()],
            ready: true,
            loaded: false,
            model: None,
        }],
    };
    assert!(caps.supports_verbatim() && caps.supports_queue());
    assert!(caps.supports_cancel() && caps.supports_capture());
    assert_eq!(caps.engine("parakeet").unwrap().id, "parakeet");
    assert_eq!(caps.ready_for(Some("en")).len(), 1);

    // Capture: dictation preempts batch, a meeting holds without confirmation.
    use rightkit_speech_client::CaptureDecision;
    assert_eq!(
        decide_capture(Some(CaptureRole::Batch), CaptureRole::HeardRightDictation, false),
        CaptureDecision::Preempt
    );
    assert_eq!(
        decide_capture(Some(CaptureRole::Meeting), CaptureRole::HeardRightDictation, false),
        CaptureDecision::ConfirmRequired
    );
    assert_eq!(
        decide_capture(Some(CaptureRole::Meeting), CaptureRole::HeardRightDictation, true),
        CaptureDecision::Preempt
    );
    assert_eq!(decide_capture(None, CaptureRole::Batch, false), CaptureDecision::Free);

    std::fs::remove_dir_all(&root).ok();
    println!("speech-client 0.4.0 consumer assertions passed");
    Ok(())
}
