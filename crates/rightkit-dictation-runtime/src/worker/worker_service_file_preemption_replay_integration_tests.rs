//! Provisioned-machine integration coverage for file work yielding to a live
//! dictation. Replay replaces only microphone ingress; worker scheduling,
//! file decode, Parakeet ASR, & transcript finalization stay production code.

use super::*;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc::TryRecvError, Arc, OnceLock};
use std::time::{Duration, Instant};

const FILE_WAIT: Duration = Duration::from_secs(300);
const SESSION_WAIT: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(5);
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static PREEMPTION_TEST_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();

struct TempSuite {
    root: PathBuf,
}

impl TempSuite {
    fn new() -> Self {
        let suffix = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("hr-fp-{}-{suffix}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create isolated preemption suite root");
        Self { root }
    }
}

impl Drop for TempSuite {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn new() -> Self {
        Self {
            previous: Vec::new(),
        }
    }

    fn set(&mut self, key: &'static str, value: impl AsRef<std::ffi::OsStr>) {
        self.previous.push((key, std::env::var_os(key)));
        std::env::set_var(key, value);
    }

    fn remove(&mut self, key: &'static str) {
        self.previous.push((key, std::env::var_os(key)));
        std::env::remove_var(key);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct ReplayGuard {
    previous_trigger: bool,
    previous_capture: Option<ReplayCaptureController>,
}

impl ReplayGuard {
    fn install_real_main_asr() -> (Self, ReplayCaptureController) {
        let previous_trigger = TRIGGER_REPLAY_ACTIVE.swap(false, Ordering::AcqRel);
        let previous_capture = REPLAY_CAPTURE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let controller = install_trigger_replay();
        // Replay ingress remains installed, while this false-at-spawn value
        // prevents the test-only ProbeDisabled main-ASR shortcut.
        TRIGGER_REPLAY_ACTIVE.store(false, Ordering::Release);
        (
            Self {
                previous_trigger,
                previous_capture,
            },
            controller,
        )
    }
}

impl Drop for ReplayGuard {
    fn drop(&mut self) {
        *REPLAY_CAPTURE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.previous_capture.take();
        TRIGGER_REPLAY_ACTIVE.store(self.previous_trigger, Ordering::Release);
    }
}

struct WorkerGuard(WorkerHandle);

impl std::ops::Deref for WorkerGuard {
    type Target = WorkerHandle;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let _ = self.0.send(WorkerCmd::Shutdown);
    }
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../rightkit-dictation-engine/tests/fixtures")
        .join(name)
}

#[cfg(target_os = "macos")]
fn provisioned_macos_model_dir() -> PathBuf {
    use rightkit_dictation_models::{model_paths, shared::store};

    let shared_root = store::paths::models_root();
    let spec = store::catalog::parakeet_spec();
    if let Some(dir) = store::resolve_ready_dir(&shared_root, &spec) {
        return dir;
    }

    let legacy_subdir = model_paths::subdir_for_backend("parakeet-tdt");
    let legacy = model_paths::install_base().join(legacy_subdir);
    if legacy.is_dir() {
        legacy
    } else {
        panic!(
            "provisioned CoreML Parakeet missing: shared {} has no ready {}, and legacy {legacy_subdir} is unavailable",
            shared_root.display(),
            spec.id,
        )
    }
}

fn read_fixture_wav(path: &Path) -> Vec<f32> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    assert_eq!(
        wav.sample_rate,
        SAMPLE_RATE,
        "{} sample rate",
        path.display()
    );
    wav.samples
}

fn write_repeated_wav(path: &Path, samples: &[f32], repeats: usize) {
    let repeated: Vec<f32> = samples.repeat(repeats);
    rightkit_audio::wav::write_wav_mono_i16(path, SAMPLE_RATE, &repeated)
        .expect("write repeated file fixture");
}

fn wait_until(label: &str, timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::park_timeout(POLL);
    }
}

fn reject_unexpected_terminal_events(worker: &WorkerHandle, session_id: &str) {
    loop {
        let event = { worker.event_rx.lock().try_recv() };
        match event {
            Ok(WorkerEvent::RecordingLevel { .. }) => {}
            Ok(event) => {
                panic!("{session_id}: unexpected terminal event before manual stop: {event:?}")
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                panic!("{session_id}: worker event stream disconnected")
            }
        }
    }
}

fn await_manual_transcript(worker: &WorkerHandle, session_id: &str) -> FinalTranscript {
    let deadline = Instant::now() + SESSION_WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "{session_id}: timed out awaiting manual transcript"
        );
        let event = worker
            .event_rx
            .lock()
            .recv_timeout(remaining)
            .unwrap_or_else(|error| panic!("{session_id}: waiting for transcript: {error}"));
        match event {
            WorkerEvent::RecordingLevel { .. } => {}
            WorkerEvent::TranscriptFinal {
                session_id: observed,
                result,
                ..
            } if observed == session_id => {
                return result.unwrap_or_else(|error| {
                    panic!("{session_id}: production final ASR failed: {error}")
                })
            }
            event => panic!("{session_id}: unexpected event while awaiting transcript: {event:?}"),
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "explicit provisioned Mac production worker file-preemption replay; needs ready CoreML Parakeet, KWS assets, and bundled WAV fixtures"]
fn provisioned_service_worker_replay_dictation_preempts_file_work() {
    let _serial = PREEMPTION_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Read shared-store readiness before tests isolate app roots. This performs
    // no activation, migration, download, or shared-reference mutation.
    let coreml_model_dir = provisioned_macos_model_dir();
    let models_dir = coreml_model_dir
        .parent()
        .expect("CoreML Parakeet has models parent")
        .to_path_buf();
    let suite = TempSuite::new();
    let mut env = EnvGuard::new();
    let kws_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws");
    assert!(
        kws_dir.join("tokens.txt").is_file(),
        "bundled KWS assets missing"
    );
    env.set("RIGHTKIT_APP_DATA_DIR", suite.root.join("app-data"));
    env.set("RIGHTKIT_COREML_MODEL_DIR", &coreml_model_dir);
    env.set("ORTHIC_SHARED_DIR", suite.root.join("shared"));
    env.set("RIGHT_SUITE_DIR", suite.root.join("suite"));
    env.set("RIGHTKIT_SHERPA_KWS_DIR", &kws_dir);
    env.set("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
    env.set("RIGHTKIT_DICTATION_LANG", "auto");
    env.set("RIGHTKIT_SCHEDULED_STATIC15", "0");
    env.set(
        "ORT_DYLIB_PATH",
        "/Applications/Example.app/Contents/Resources/runtime/libonnxruntime.dylib",
    );
    env.set(
        "RIGHTKIT_FFMPEG_PATH",
        "/Applications/Example.app/Contents/Resources/runtime/ffmpeg",
    );
    env.remove("RIGHTKIT_DISABLE_KWS");

    let long_pcm = read_fixture_wav(&fixture("long_38s.wav"));
    let dictated_pcm = read_fixture_wav(&fixture("short_9s.wav"));
    let long_file = suite.root.join("file-work-long.wav");
    write_repeated_wav(&long_file, &long_pcm, 4);

    let (_replay, controller) = ReplayGuard::install_real_main_asr();
    let worker = WorkerGuard(
        spawn_worker_with_clock(
            models_dir,
            AsrEp::Cpu,
            Arc::new(Mutex::new(FocusTracker::new())),
            controller.virtual_clock(),
        )
        .expect("spawn production worker with real main ASR"),
    );

    let (file_reply_tx, file_reply_rx) = channel();
    let file_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let file_progress = Arc::new(FileProgress::default());
    worker
        .send(WorkerCmd::TranscribeFileJob {
            path: long_file,
            verbatim: false,
            cancel: Arc::clone(&file_cancel),
            progress: Arc::clone(&file_progress),
            reply: file_reply_tx,
        })
        .expect("queue real file work");

    wait_until("first real file window", FILE_WAIT, || {
        match file_reply_rx.try_recv() {
            Ok(result) => panic!("file finished before dictation could preempt it: {result:?}"),
            Err(TryRecvError::Disconnected) => panic!("file reply channel disconnected"),
            Err(TryRecvError::Empty) => {}
        }
        file_progress.done.load(Ordering::Acquire) > 0
    });

    controller.load(dictated_pcm);
    let session_id = "file-preemption-dictation";
    worker
        .send(WorkerCmd::StartRecording {
            session_id: session_id.to_string(),
        })
        .expect("start replayed dictation");

    wait_until("file work paused by live dictation", SESSION_WAIT, || {
        match file_reply_rx.try_recv() {
            Ok(result) => panic!("file completed while dictation should own ASR: {result:?}"),
            Err(TryRecvError::Disconnected) => panic!("file reply channel disconnected"),
            Err(TryRecvError::Empty) => {}
        }
        file_progress.paused.load(Ordering::Acquire)
    });
    let paused_at = file_progress.done.load(Ordering::Acquire);

    wait_until(
        "dictation WAV drained through production capture",
        SESSION_WAIT,
        || {
            reject_unexpected_terminal_events(&worker, session_id);
            controller.finished().is_some()
        },
    );
    assert_eq!(
        file_progress.done.load(Ordering::Acquire),
        paused_at,
        "file made progress while live dictation owned ASR"
    );

    worker
        .send(WorkerCmd::StopRecording { send_enter: false })
        .expect("manually stop replayed dictation");
    let transcript = await_manual_transcript(&worker, session_id);
    assert!(
        matches!(transcript.stop_evidence, StopEvidence::Manual),
        "dictation completion did not retain manual-stop evidence"
    );
    let text = transcript.text.to_ascii_lowercase();
    assert!(
        text.contains("timer") && text.contains("minutes"),
        "dictation transcript: {text:?}"
    );

    wait_until("file work resumed after dictation", SESSION_WAIT, || {
        !file_progress.paused.load(Ordering::Acquire)
    });
    let file = file_reply_rx
        .recv_timeout(FILE_WAIT)
        .expect("file reply after dictation")
        .expect("file transcription succeeds after resume");
    assert!(
        file.text.to_ascii_lowercase().contains("healthy"),
        "file transcript: {:?}",
        file.text
    );
    assert!(
        file_progress.done.load(Ordering::Acquire) > paused_at,
        "file did not advance after dictation finalization"
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "provisioned Mac: real ASR, replayed mic, real speech socket"]
fn provisioned_service_verbatim_survives_dictation_pause() {
    let _serial = PREEMPTION_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap();
    let model = provisioned_macos_model_dir();
    let models = model.parent().unwrap().to_path_buf();
    let suite = TempSuite::new();
    let mut env = EnvGuard::new();
    env.set("RIGHTKIT_APP_DATA_DIR", suite.root.join("app-data"));
    env.set("RIGHTKIT_COREML_MODEL_DIR", &model);
    env.set("ORTHIC_SHARED_DIR", suite.root.join("shared"));
    env.set("RIGHT_SUITE_DIR", suite.root.join("shared"));
    env.set(
        "RIGHTKIT_SHERPA_KWS_DIR",
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws"),
    );
    env.set("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
    env.set("RIGHTKIT_DICTATION_LANG", "auto");
    env.set("RIGHTKIT_SCHEDULED_STATIC15", "0");
    env.set(
        "RIGHTKIT_FFMPEG_PATH",
        "/Applications/Example.app/Contents/Resources/runtime/ffmpeg",
    );
    env.set(
        "ORT_DYLIB_PATH",
        "/Applications/Example.app/Contents/Resources/runtime/libonnxruntime.dylib",
    );
    env.set(
        "RIGHTKIT_FFMPEG_PATH",
        "/Applications/Example.app/Contents/Resources/runtime/ffmpeg",
    );
    env.remove("RIGHTKIT_DISABLE_KWS");
    let filler = fixture("filler_retake.wav");
    let pcm = read_fixture_wav(&filler);
    let long = suite.root.join("repeated-filler.wav");
    write_repeated_wav(&long, &pcm, 4);
    let (_replay, controller) = ReplayGuard::install_real_main_asr();
    let worker = WorkerGuard(
        spawn_worker_with_clock(
            models.clone(),
            AsrEp::Cpu,
            Arc::new(Mutex::new(FocusTracker::new())),
            controller.virtual_clock(),
        )
        .unwrap(),
    );
    let mut runtime = crate::runtime::EngineRuntime::new(models);
    runtime.worker = Some(worker.0.clone());
    let runtime = Arc::new(Mutex::new(runtime));
    let _service = crate::speech_service::start(
        runtime.clone(),
        rightkit_speech_client::HostRole::HeardRightShell,
    )
    .unwrap();
    let root = suite.root.join("shared");
    let mut client = rightkit_speech_client::connect(&root).unwrap();
    let caps = client.capabilities(Duration::from_secs(10)).unwrap();
    assert!(caps.supports_verbatim() && caps.supports_queue());
    let (progress_tx, progress_rx) = channel();
    let file_thread = std::thread::spawn(move || {
        let mut options = rightkit_speech_client::TranscribeOptions::new(FILE_WAIT)
            .verbatim()
            .request_id("verbatim-pause-proof")
            .on_progress(move |p| {
                let _ = progress_tx.send(p.clone());
            });
        client
            .transcribe(&long, &mut options)
            .expect("verbatim service file")
            .transcript
    });
    // Wait for actual first ASR window, not just model preparation.
    loop {
        if let rightkit_speech_client::Progress::Running {
            fraction: Some(f), ..
        } = progress_rx.recv_timeout(FILE_WAIT).unwrap()
        {
            if f > 0.0 {
                break;
            }
        }
    }
    controller.load(pcm);
    runtime
        .lock()
        .begin_recording("verbatim-dictation", None, None)
        .unwrap();
    worker
        .send(WorkerCmd::StartRecording {
            session_id: "verbatim-dictation".into(),
        })
        .unwrap();
    let mut observer = rightkit_speech_client::connect(&root).unwrap();
    wait_until(
        "queued state while dictation owns model",
        SESSION_WAIT,
        || {
            matches!(observer.job_status("verbatim-pause-proof").unwrap(),
            rightkit_speech_client::JobStatus::Active(p) if p.reason == Some(rightkit_speech_client::QueueReason::DictationActive))
        },
    );
    std::thread::park_timeout(Duration::from_millis(2300));
    wait_until("mic replay consumed filler fixture", SESSION_WAIT, || {
        reject_unexpected_terminal_events(&worker, "verbatim-dictation");
        controller.finished().is_some()
    });
    worker
        .send(WorkerCmd::StopRecording { send_enter: false })
        .unwrap();
    let dictated = await_manual_transcript(&worker, "verbatim-dictation");
    runtime.lock().state = crate::runtime::EngineState::Idle;
    let verbatim = file_thread.join().unwrap();
    fn fillers(text: &str) -> usize {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|s| ["um", "uh"].contains(&s.to_ascii_lowercase().as_str()))
            .count()
    }
    assert_eq!(fillers(&dictated.text), 0, "dictation: {}", dictated.text);
    assert!(
        fillers(&verbatim.text) > 0,
        "verbatim lost spoken fillers: {}",
        verbatim.text
    );
    assert!(verbatim.words.iter().any(|w| fillers(&w.text) > 0));
    assert!(fillers(&verbatim.srt) > 0 && fillers(&verbatim.vtt) > 0);
    assert!(matches!(
        observer.job_status("verbatim-pause-proof").unwrap(),
        rightkit_speech_client::JobStatus::Done(_)
    ));
    println!(
        "dictation={:?}; verbatim={:?}; retained_words={}",
        dictated.text,
        verbatim.text,
        verbatim.words.len()
    );
}
