#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_contracts::types::record_event::{EventKind, PolicyRef, RecordEvent};
use rightkit_contracts::types::record_session::{Surface, SurfaceKind};
use rightkit_record::fake::FakeBackend;
use rightkit_record::{
    FixedKeySource, KeyHandle, ManualClock, Recorder, RedactionPolicy, ScenarioSpec, ScenarioStep,
    StartRequest, TargetSpec,
};

pub type Rec = Recorder<FakeBackend, ManualClock>;

/// A bundle directory under cargo's per-target temp area. Read-only bundles
/// are made writable again before the directory is removed.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("rk-record-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        Scratch(root)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn bundle(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        make_writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Restores write permission under `path`, so a test can tamper with a sealed bundle.
// Windows has no world-writable hazard: `set_readonly(false)` is the only way to clear the flag there.
#[allow(clippy::permissions_set_readonly_false)]
pub fn make_writable(path: &Path) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
    }
    let mut permissions = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o700);
    }
    #[cfg(not(unix))]
    {
        permissions.set_readonly(false);
    }
    let _ = fs::set_permissions(path, permissions);
}

pub fn surface() -> Surface {
    Surface {
        surface_id: "main".into(),
        kind: SurfaceKind::MainWindow,
        parent_surface_id: None,
        native_id: "4242".into(),
        supported: true,
        unsupported_reason: None,
    }
}

/// The fixed key every test take hashes under, so no test touches the OS credential store.
pub const TEST_KEY: [u8; 32] = [0x5a; 32];

pub fn request(out: PathBuf) -> StartRequest {
    let mut request = start_request(out);
    request.key_source = KeyHandle::new(FixedKeySource::new(TEST_KEY));
    request
}

fn start_request(out: PathBuf) -> StartRequest {
    StartRequest::new(
        TargetSpec {
            app_id: "cutright".into(),
            window_id: 4242,
            pid: 77,
            surfaces: vec![surface()],
        },
        ScenarioSpec {
            scenario_id: "demo.one".into(),
            steps: vec![
                ScenarioStep {
                    step_id: "step-1".into(),
                    label: "Open".into(),
                    say: None,
                },
                ScenarioStep {
                    step_id: "step-2".into(),
                    label: "Save".into(),
                    say: Some("save the project".into()),
                },
            ],
        },
        out,
    )
}

pub fn recorder(backend: FakeBackend, now_ms: u64) -> Rec {
    Recorder::open(backend, ManualClock::new(now_ms), &[], 30_000)
        .expect("open")
        .0
}

pub fn blank_event(kind: EventKind, host_ns: u64) -> RecordEvent {
    RecordEvent {
        contract: "record-event".into(),
        contract_version: "1.1.0".into(),
        session_id: String::new(),
        seq: 0,
        kind,
        input_kind: None,
        phase: None,
        effect_id: None,
        sub_event_id: None,
        step_index: 0,
        step_id: None,
        host_ns,
        timing: None,
        geometry_epoch: 0,
        dispatch: None,
        pointer: None,
        target: None,
        text: None,
        key: None,
        modifiers: Vec::new(),
        delta: None,
        admitted: None,
        denied: None,
        postcondition: None,
        frame: None,
        frame_dropped: None,
        intruders: Vec::new(),
        redaction_policy: PolicyRef {
            policy_id: "redaction.rk3".into(),
            policy_version: "1.0.0".into(),
        },
    }
}

pub fn strict_policy() -> RedactionPolicy {
    RedactionPolicy::strict()
}

pub fn read_manifest(dir: &Path) -> serde_json::Value {
    let text = fs::read_to_string(dir.join("hashes.json")).expect("manifest");
    serde_json::from_str(&text).expect("manifest json")
}
