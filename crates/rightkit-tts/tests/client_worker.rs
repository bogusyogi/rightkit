//! Client supervision against a scripted stand-in worker: no model and no ONNX Runtime needed.
//! The stand-in records its argv and every start next to itself, answers `ready`, then `synth`
//! and `clone` requests. Unix only (it is a shell script). The worker slot and the environment are
//! process-global, so every test holds `LOCK`.
#![cfg(all(unix, feature = "client"))]

use rightkit_tts::models::MANIFEST;
use rightkit_tts::worker_args::{parse, WorkerArgs};
use rightkit_tts::{Mode, ModeLocation, QtsClient, SynthOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

const SCRIPT: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$0.args"
echo started >> "$0.starts"
printf '@@GQTS@@ {"type":"ready","backend":"cpu","vocoder":"cpu","load_ms":1,"version":"t"}\n'
while IFS= read -r line; do
  case "$line" in
    *'"op":"quit"'*) exit 0 ;;
  esac
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  if [ -f "$0.fail" ]; then
    printf '@@GQTS@@ {"type":"error","id":%s,"message":"model_missing: voice_design: /nowhere"}\n' "$id"
    continue
  fi
  case "$line" in
    *'"op":"clone"'*)
      out=$(printf '%s' "$line" | sed -n 's/.*"out":"\([^"]*\)".*/\1/p')
      : > "$out"
      printf '@@GQTS@@ {"type":"done","id":%s,"samples":0,"sample_rate":24000,"gen_ms":1}\n' "$id" ;;
    *)
      out=$(printf '%s' "$line" | sed -n 's/.*"out_pcm":"\([^"]*\)".*/\1/p')
      printf '\350\003\350\003\350\003' > "$out"
      printf '@@GQTS@@ {"type":"done","id":%s,"samples":3,"sample_rate":24000,"gen_ms":1}\n' "$id" ;;
  esac
done
"#;

struct Env {
    root: PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Env {
    fn new(name: &str) -> Self {
        let guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        rightkit_tts::client::shutdown();
        for var in [
            "RIGHTKIT_TTS_WORKER",
            "GENRIGHT_QTS_WORKER",
            "RIGHTKIT_TTS_MODEL_DIR",
            "GENRIGHT_QTS_MODEL_DIR",
        ] {
            std::env::remove_var(var);
        }
        let root = std::env::temp_dir().join(format!("rk-tts-client-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self { root, _guard: guard }
    }

    fn dir(&self, name: &str) -> PathBuf {
        let d = self.root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn worker(&self, name: &str) -> PathBuf {
        let path = self.dir(name).join("worker.sh");
        std::fs::write(&path, SCRIPT).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A 1.7B mode folder with the three files the worker needs, plus its vocoder.
    fn mode_models(&self, name: &str, mode: Mode) -> PathBuf {
        let dir = self.dir(name);
        let stem = match mode {
            Mode::CustomVoice => "qwen3-tts-1.7b-customvoice",
            _ => "qwen3-tts-1.7b-voicedesign",
        };
        for file in [
            format!("{stem}-talker-q8_0.gguf"),
            format!("{stem}-code-predictor-q8_0.gguf"),
            format!("{stem}-tokenizer.gguf"),
            "qwen3-tts-vocoder.onnx".to_string(),
        ] {
            std::fs::write(dir.join(file), b"x").unwrap();
        }
        dir
    }

    /// Base files at the manifest sizes (sparse), with the verified-cache entries the client
    /// writes after a hash pass, so verification accepts them without hashing 1.7 GB of zeros.
    fn base_models(&self, name: &str) -> PathBuf {
        let dir = self.dir(name);
        let mut entries = Vec::new();
        for f in MANIFEST.iter() {
            let path = dir.join(f.name);
            let file = std::fs::File::create(&path).unwrap();
            file.set_len(f.bytes).unwrap();
            let mtime = std::fs::metadata(&path)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            entries.push(format!(
                r#"{{"name":"{}","bytes":{},"mtime_ns":{}}}"#,
                f.name, f.bytes, mtime
            ));
        }
        std::fs::write(
            dir.join(".verified.json"),
            format!(r#"{{"files":[{}]}}"#, entries.join(",")),
        )
        .unwrap();
        dir
    }

    fn client(&self) -> QtsClient {
        QtsClient::new(60).with_data_root(self.dir("data"))
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        rightkit_tts::client::shutdown();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn args_of(worker: &Path) -> WorkerArgs {
    let raw = std::fs::read_to_string(format!("{}.args", worker.display())).expect("worker ran");
    parse(raw.lines().map(str::to_owned)).expect("worker args parse")
}

fn starts(worker: &Path) -> usize {
    std::fs::read_to_string(format!("{}.starts", worker.display()))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

fn voice_design() -> SynthOptions {
    SynthOptions {
        mode: Mode::VoiceDesign,
        instruct: Some("a calm narrator".into()),
        ..SynthOptions::default()
    }
}

fn custom_voice() -> SynthOptions {
    SynthOptions {
        mode: Mode::CustomVoice,
        speaker: Some("vivian".into()),
        ..SynthOptions::default()
    }
}

#[test]
fn explicit_worker_exe_beats_environment_and_is_what_launches() {
    let env = Env::new("exe-precedence");
    let explicit = env.worker("explicit");
    let ambient = env.worker("ambient");
    std::env::set_var("RIGHTKIT_TTS_WORKER", &ambient);
    std::env::set_var("GENRIGHT_QTS_WORKER", &ambient);
    let vd = env.mode_models("vd", Mode::VoiceDesign);
    let client = env
        .client()
        .with_worker_exe(explicit.clone())
        .with_mode_model_dir(Mode::VoiceDesign, vd);
    assert_eq!(client.worker_exe().unwrap(), explicit);
    client.synthesize("hello there", &voice_design()).unwrap();
    assert_eq!(starts(&explicit), 1, "the explicit worker launched");
    assert_eq!(starts(&ambient), 0, "the environment worker did not");
    // Without the setter the environment is still honoured (existing behaviour).
    let plain = env.client();
    assert_eq!(plain.worker_exe().unwrap(), ambient);
}

#[test]
fn absent_explicit_worker_exe_never_falls_back_to_discovery() {
    let env = Env::new("exe-absent");
    let ambient = env.worker("ambient");
    std::env::set_var("RIGHTKIT_TTS_WORKER", &ambient);
    let vd = env.mode_models("vd", Mode::VoiceDesign);
    let missing = env.root.join("not-there").join("worker.sh");
    let client = env
        .client()
        .with_worker_exe(missing.clone())
        .with_mode_model_dir(Mode::VoiceDesign, vd);
    // The passive status check and the real launch resolve through one function and agree.
    let status = client.worker_exe().unwrap_err();
    assert_eq!(status.code, "worker_missing");
    assert!(status.message.contains("not-there"), "{}", status.message);
    let launch = client.synthesize("hello there", &voice_design()).unwrap_err();
    assert_eq!(launch, status);
    assert_eq!(starts(&ambient), 0, "no fall back to the environment worker");
}

#[test]
fn per_mode_locations_reach_the_worker_before_ready_and_modes_share_one_worker() {
    let env = Env::new("locations");
    let worker = env.worker("w");
    let base = env.base_models("base");
    let cv = env.mode_models("cv", Mode::CustomVoice);
    let vd = env.mode_models("vd", Mode::VoiceDesign);
    let shared_vocoder = env.dir("shared").join("voc.onnx");
    std::fs::write(&shared_vocoder, b"x").unwrap();
    let client = env
        .client()
        .with_worker_exe(worker.clone())
        .with_model_dir(base.clone())
        .with_mode_location(ModeLocation::new(Mode::CustomVoice, &cv).with_vocoder(&shared_vocoder))
        .with_mode_model_dir(Mode::VoiceDesign, vd.clone());
    // The first spawn is for VoiceDesign, the second mode.
    client.synthesize("hello there", &voice_design()).unwrap();
    let pid = client.worker_pid().expect("warm worker");
    let args = args_of(&worker);
    assert_eq!(args.mode, Mode::VoiceDesign);
    let at = |mode: Mode| {
        args.mode_locations
            .iter()
            .find(|l| l.mode == mode)
            .unwrap_or_else(|| panic!("no location forwarded for {mode:?}: {args:?}"))
            .clone()
    };
    assert_eq!(at(Mode::VoiceDesign).model_dir, vd);
    assert_eq!(at(Mode::VoiceDesign).vocoder, Some(vd.join("qwen3-tts-vocoder.onnx")));
    assert_eq!(at(Mode::CustomVoice).model_dir, cv);
    assert_eq!(at(Mode::CustomVoice).vocoder, Some(shared_vocoder));
    // Base is forwarded though the worker started for VoiceDesign: Clone needs it.
    assert_eq!(at(Mode::Base).model_dir, base);
    assert_eq!(args.model_dir, Some(vd.clone()), "--model-dir is the starting mode's folder");

    // Another mode, and cloning, reuse the same worker process.
    client.synthesize("hello there", &custom_voice()).unwrap();
    let wav = env.dir("ref").join("ref.wav");
    std::fs::write(&wav, b"RIFF").unwrap();
    let out = env.dir("out").join("v.xvector.pb");
    client.clone_from_wav(&wav, &out).unwrap();
    assert_eq!(client.worker_pid(), Some(pid));
    assert_eq!(starts(&worker), 1, "no respawn on mode change");
}

#[test]
fn clone_checks_the_base_location_even_when_the_worker_started_for_voice_design() {
    let env = Env::new("clone-base");
    let worker = env.worker("w");
    let vd = env.mode_models("vd", Mode::VoiceDesign);
    let empty_base = env.dir("empty-base");
    let client = env
        .client()
        .with_worker_exe(worker.clone())
        .with_model_dir(empty_base.clone())
        .with_mode_model_dir(Mode::VoiceDesign, vd);
    client.synthesize("hello there", &voice_design()).unwrap();
    let wav = env.dir("ref").join("ref.wav");
    std::fs::write(&wav, b"RIFF").unwrap();
    let err = client
        .clone_from_wav(&wav, &env.dir("out").join("v.pb"))
        .unwrap_err();
    assert_eq!(err.code, "models_missing");
    assert!(err.message.contains(&empty_base.display().to_string()), "{}", err.message);
}

#[test]
fn engine_info_runs_on_the_clients_own_configuration() {
    let env = Env::new("engine-info");
    let worker = env.worker("w");
    let base = env.base_models("base");
    let runtime = env.dir("rt").join(rightkit_ort_name());
    std::fs::write(&runtime, b"x").unwrap();
    let client = env
        .client()
        .with_worker_exe(worker.clone())
        .with_model_dir(base.clone())
        .with_ort_runtime(runtime.clone());
    assert_eq!(client.engine_info(Some("cpu")).unwrap(), ("cpu".into(), "cpu".into()));
    let args = args_of(&worker);
    assert_eq!(args.mode, Mode::Base);
    assert_eq!(args.model_dir, Some(base));
    assert_eq!(args.backend.as_deref(), Some("cpu"));
    assert_eq!(args.data_root, Some(env.root.join("data")));
    assert_eq!(args.ort_runtime, Some(runtime));
}

fn rightkit_ort_name() -> &'static str {
    rightkit_tts::models::runtime_lib_name()
}

#[test]
fn engine_info_without_models_names_the_clients_folder_not_the_default() {
    let env = Env::new("engine-info-missing");
    let base = env.dir("my-base");
    let client = env
        .client()
        .with_worker_exe(env.worker("w"))
        .with_model_dir(base.clone());
    let err = client.engine_info(None).unwrap_err();
    assert_eq!(err.code, "models_missing");
    assert!(err.message.contains(&base.display().to_string()), "{}", err.message);
}

#[test]
fn idle_window_is_forwarded_only_when_set_explicitly() {
    let env = Env::new("idle");
    let worker = env.worker("w");
    let vd = env.mode_models("vd", Mode::VoiceDesign);
    let base_client = || {
        env.client()
            .with_worker_exe(worker.clone())
            .with_mode_model_dir(Mode::VoiceDesign, vd.clone())
    };
    base_client().synthesize("hello there", &voice_design()).unwrap();
    assert_eq!(args_of(&worker).idle_secs, None, "nothing explicit: the environment decides");
    let client = base_client().with_idle_secs(77);
    assert_eq!(client.idle_secs(), 77);
    client.synthesize("hello there", &voice_design()).unwrap();
    assert_eq!(args_of(&worker).idle_secs, Some(77));
    // Zero is passed through unchanged, not clamped.
    let zero = base_client().with_idle_secs(0);
    zero.synthesize("hello there", &voice_design()).unwrap();
    assert_eq!(args_of(&worker).idle_secs, Some(0));
}

#[test]
fn a_different_client_configuration_respawns_the_shared_worker_and_the_same_one_does_not() {
    let env = Env::new("respawn");
    let worker = env.worker("w");
    let vd = env.mode_models("vd", Mode::VoiceDesign);
    let make = || {
        env.client()
            .with_worker_exe(worker.clone())
            .with_mode_model_dir(Mode::VoiceDesign, vd.clone())
    };
    // All three stay alive: dropping a client stops the shared worker.
    let (a, b, c) = (make(), make().with_idle_secs(5), make());
    a.synthesize("hello there", &voice_design()).unwrap();
    let first = a.worker_pid().unwrap();
    c.synthesize("hello there", &voice_design()).unwrap();
    assert_eq!(c.worker_pid(), Some(first), "equal configuration reuses the worker");
    assert_eq!(starts(&worker), 1);
    b.synthesize("hello there", &voice_design()).unwrap();
    assert_ne!(b.worker_pid(), Some(first), "a different idle window respawns");
    assert_eq!(starts(&worker), 2);
    assert_eq!(args_of(&worker).idle_secs, Some(5));
}

#[test]
fn a_model_missing_reply_surfaces_as_models_missing() {
    let env = Env::new("reply-class");
    let worker = env.worker("w");
    let vd = env.mode_models("vd", Mode::VoiceDesign);
    std::fs::write(format!("{}.fail", worker.display()), b"").unwrap();
    let client = env
        .client()
        .with_worker_exe(worker)
        .with_mode_model_dir(Mode::VoiceDesign, vd);
    let err = client.synthesize("hello there", &voice_design()).unwrap_err();
    assert_eq!(err.code, "models_missing", "{err}");
    assert!(err.message.starts_with("model_missing: voice_design"), "{}", err.message);
}

#[test]
fn missing_mode_files_fail_before_any_worker_starts() {
    let env = Env::new("precheck");
    let worker = env.worker("w");
    let empty = env.dir("empty-cv");
    let client = env
        .client()
        .with_worker_exe(worker.clone())
        .with_mode_model_dir(Mode::CustomVoice, empty.clone());
    let err = client.synthesize("hello there", &custom_voice()).unwrap_err();
    assert_eq!(err.code, "models_missing");
    assert!(err.message.contains("custom_voice"), "{}", err.message);
    assert!(err.message.contains("talker"), "{}", err.message);
    assert!(err.message.contains(&empty.display().to_string()), "{}", err.message);
    assert_eq!(starts(&worker), 0, "no worker was started");
}
