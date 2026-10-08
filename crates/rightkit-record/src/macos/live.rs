//! Live tests of the macOS backend against the built Swift helper and an off-screen fixture
//! window. All are `#[ignore]`: they need the helper and fixture binaries, and the helper
//! identity must hold Screen Recording permission. The fixture window sits entirely outside every
//! display (it checks that itself and exits if the window server disagrees).
//!
//! Run:
//! ```text
//! helpers/macos/build.sh                      # builds and ad-hoc signs both binaries
//! rightkit cargo test -p rightkit-record --no-run
//! # then run the test binary from a session whose responsible process holds Screen Recording
//! # permission (the Claude app, Terminal, ...), not through the rightkit build broker:
//! RIGHTKIT_RECORD_MACOS_HELPER=$PWD/helpers/macos/.build/out/Products/Release/rightkit-record-macos-helper \
//!   <test binary> macos::live -- --ignored --test-threads=1
//! ```
//! `RK_RECORD_LIVE_SECONDS` sets the length of the long capture (default 30).
//! `RK_RECORD_KEEP=1` keeps the sealed bundle and prints its path.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_contracts::types::record_session::{
    BundleStatus, Surface, SurfaceKind, VisibilityState,
};
use serde_json::Value;

use super::MacBackend;
use crate::backend::{Backend, BackendPlan, Codec, Failure, HelperEvent, Permission};
use crate::clock::SystemClock;
use crate::error::Code;
use crate::plan::{PixelMasking, ScenarioSpec, ScenarioStep, StartRequest, TargetSpec};
use crate::recorder::Recorder;
use crate::verify::VerifyOptions;

const FIXTURE_ENV: &str = "RIGHTKIT_RECORD_MACOS_FIXTURE";

fn helper() -> PathBuf {
    MacBackend::helper_path()
        .expect("build the helper with helpers/macos/build.sh and set RIGHTKIT_RECORD_MACOS_HELPER")
}

fn fixture_binary() -> PathBuf {
    if let Some(path) = std::env::var_os(FIXTURE_ENV) {
        return PathBuf::from(path);
    }
    helper().with_file_name("rk-fixture-window")
}

/// The off-screen fixture window process. Killed when dropped, so no test leaves it running.
struct Fixture {
    child: Child,
    pid: u32,
    window_id: u64,
}

impl Fixture {
    fn launch() -> Fixture {
        let mut child = Command::new(fixture_binary())
            // The fixture ends itself after this long even if the test dies.
            .args(["--seconds", "180"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the fixture window");
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let info: Value =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("fixture printed {line:?}: {e}"));
        assert_eq!(
            info["ok"], true,
            "fixture refused to place its window: {info}"
        );
        assert_eq!(
            info["screenIsNil"], true,
            "the fixture window must be off every display: {info}"
        );
        Fixture {
            pid: info["pid"].as_u64().unwrap() as u32,
            window_id: info["windowId"].as_u64().unwrap(),
            child,
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("rk-record-mac-{name}-{}", std::process::id()));
        let _ = make_writable(&dir);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::env::var_os("RK_RECORD_KEEP").is_some() {
            eprintln!("kept {}", self.0.display());
            return;
        }
        let _ = make_writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A sealed bundle is read-only; make it removable again.
fn make_writable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(path)? {
            let _ = make_writable(&entry?.path());
        }
    } else {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn plan(out: &Path, fixture: &Fixture, codec: Codec) -> BackendPlan {
    BackendPlan {
        out_dir: out.to_path_buf(),
        window_id: fixture.window_id,
        pid: fixture.pid,
        fps: 30,
        scale: 1.0,
        audio: Vec::new(),
        visibility: VisibilityState::OffScreenVisible,
        masking: PixelMasking::Declared(Vec::new()),
        codec,
    }
}

fn request(out: PathBuf, fixture: &Fixture, codec: Codec) -> StartRequest {
    let mut request = StartRequest::new(
        TargetSpec {
            app_id: "dev.rightkit.fixture".into(),
            window_id: fixture.window_id,
            pid: fixture.pid,
            surfaces: vec![Surface {
                surface_id: "main".into(),
                kind: SurfaceKind::MainWindow,
                parent_surface_id: None,
                native_id: fixture.window_id.to_string(),
                supported: true,
                unsupported_reason: None,
            }],
        },
        ScenarioSpec {
            scenario_id: "live.fixture".into(),
            steps: vec![ScenarioStep {
                step_id: "step-1".into(),
                label: "Watch".into(),
                say: None,
            }],
        },
        out,
    );
    request.fps = 30;
    request.codec = codec;
    request
}

fn seconds(var: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(var)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default),
    )
}

fn wait_for(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if done() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Records the fixture for `length` through the core's recorder, seals the bundle, and checks it.
fn record_and_verify(codec: Codec, length: Duration, name: &str) {
    let fixture = Fixture::launch();
    let scratch = Scratch::new(name);
    let bundle = scratch.0.join("take");
    let backend = MacBackend::launch(&helper()).expect("launch the helper");
    assert_eq!(
        backend.permission(),
        Permission::Granted,
        "the helper identity needs Screen Recording permission"
    );
    let (mut recorder, _) = Recorder::open(backend, SystemClock, &[], 30_000).unwrap();
    recorder
        .record_start(request(bundle.clone(), &fixture, codec))
        .expect("start the take");

    let began = Instant::now();
    let mut last = None;
    while began.elapsed() < length {
        thread::sleep(Duration::from_millis(100));
        let status = recorder.record_status().expect("status");
        assert!(
            status.failure.is_none(),
            "take failed: {:?}",
            status.failure
        );
        last = Some(status);
    }
    let live = last.expect("at least one status");
    let outcome = recorder.record_stop().expect("stop the take");
    eprintln!(
        "{name}: {:?} after {:?}, {} frames seen live, {} dropped, bundle {}",
        outcome.status,
        began.elapsed(),
        live.frames,
        live.dropped,
        bundle.display()
    );
    assert_eq!(
        outcome.status,
        BundleStatus::Complete,
        "reason {:?}, helper stderr: {}",
        outcome.reason,
        recorder.backend().helper_stderr()
    );
    assert_eq!(outcome.manifest_problem, None);

    let report = recorder
        .bundle_verify(&bundle, &VerifyOptions::default())
        .expect("verify");
    assert!(report.ok(), "bundle_verify problems: {:?}", report.problems);
    assert!(bundle.join("clean.mp4").is_file());

    // The fixture draws on every tick, so a 30 fps take delivers close to 30 frames a second.
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(bundle.join("hashes.json")).unwrap()).unwrap();
    let frames = manifest["frames"]["count"].as_u64().unwrap();
    let floor = length.as_secs() * 30 * 8 / 10;
    assert!(
        frames >= floor,
        "{frames} frames recorded, expected at least {floor}"
    );
    eprintln!("{name}: manifest frames {frames}, floor {floor}");
}

#[test]
#[ignore = "needs the built helper (helpers/macos/build.sh); see the module docs"]
fn hello_reports_the_identity_without_prompting() {
    let backend = MacBackend::launch(&helper()).expect("launch");
    assert_eq!(
        backend.os(),
        rightkit_contracts::types::record_session::RecorderOs::Macos
    );
    assert_eq!(backend.helper().identity, "rightkit-record-macos-helper");
    assert!(backend.os_build().contains('('), "{}", backend.os_build());
    assert!(matches!(
        backend.permission(),
        Permission::Granted | Permission::Denied
    ));
}

#[test]
#[ignore = "needs the built helper (helpers/macos/build.sh); see the module docs"]
fn without_permission_start_is_refused_and_nothing_is_written() {
    let scratch = Scratch::new("denied");
    let out = scratch.0.join("take");
    fs::create_dir_all(&out).unwrap();
    let mut backend =
        MacBackend::launch_with_env(&helper(), &[("RIGHTKIT_RECORD_TEST_PERMISSION", "denied")])
            .expect("launch");
    assert_eq!(backend.permission(), Permission::Denied);
    let fixture = Fixture::launch();
    let error = backend
        .start(&plan(&out, &fixture, Codec::H264))
        .unwrap_err();
    assert_eq!(error.code, Code::PermissionDenied, "{error}");
    assert_eq!(
        fs::read_dir(&out).unwrap().count(),
        0,
        "nothing may be written"
    );
}

#[test]
#[ignore = "needs the helper and Screen Recording permission; see the module docs"]
fn an_unknown_window_is_not_found_and_nothing_is_written() {
    let scratch = Scratch::new("unknown");
    let out = scratch.0.join("take");
    fs::create_dir_all(&out).unwrap();
    let mut backend = MacBackend::launch(&helper()).expect("launch");
    let fixture = Fixture::launch();
    let mut request = plan(&out, &fixture, Codec::H264);
    request.window_id = 4_000_000_000;
    request.pid = 0;
    let error = backend.start(&request).unwrap_err();
    assert_eq!(error.code, Code::NotFound, "{error}");
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
}

#[test]
#[ignore = "needs the helper and Screen Recording permission; see the module docs"]
fn an_unsupported_plan_is_refused_before_capture() {
    let scratch = Scratch::new("refused");
    let out = scratch.0.join("take");
    fs::create_dir_all(&out).unwrap();
    let mut backend = MacBackend::launch(&helper()).expect("launch");
    let fixture = Fixture::launch();

    let mut masked = plan(&out, &fixture, Codec::H264);
    masked.masking = PixelMasking::Unknown;
    assert_eq!(
        backend.start(&masked).unwrap_err().code,
        Code::MaskGeometryUnknown
    );

    let mut hidden = plan(&out, &fixture, Codec::H264);
    hidden.visibility = VisibilityState::Minimized;
    assert_eq!(backend.start(&hidden).unwrap_err().code, Code::Unsupported);

    let mut audio = plan(&out, &fixture, Codec::H264);
    audio.audio = vec![rightkit_contracts::types::record_session::TrackKind::AppAudio];
    assert_eq!(backend.start(&audio).unwrap_err().code, Code::Unsupported);

    // On screen is not what an off-screen take may claim; the fixture is off every display, so a
    // take that declares on-screen is refused.
    let mut onscreen = plan(&out, &fixture, Codec::H264);
    onscreen.visibility = VisibilityState::OnScreenVisible;
    assert_eq!(
        backend.start(&onscreen).unwrap_err().code,
        Code::Unsupported
    );
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
}

#[test]
#[ignore = "needs the helper and Screen Recording permission; see the module docs"]
fn the_target_exiting_mid_take_is_reported_and_the_media_is_kept() {
    let scratch = Scratch::new("exit");
    let out = scratch.0.join("take");
    fs::create_dir_all(&out).unwrap();
    let mut backend = MacBackend::launch(&helper()).expect("launch");
    let mut fixture = Fixture::launch();
    backend
        .start(&plan(&out, &fixture, Codec::H264))
        .expect("start");

    let mut events = Vec::new();
    assert!(
        wait_for(Duration::from_secs(10), || {
            events.extend(backend.poll());
            events.iter().any(|e| matches!(e, HelperEvent::Ready(_)))
        }),
        "no readiness marker: {events:?}"
    );
    thread::sleep(Duration::from_secs(1));
    fixture.kill();
    assert!(
        wait_for(Duration::from_secs(10), || {
            events.extend(backend.poll());
            events
                .iter()
                .any(|e| matches!(e, HelperEvent::Failed(Failure::TargetExited)))
        }),
        "target exit was not reported: {events:?}"
    );
    backend.abort();
    // Aborting finalizes what was captured, so the kept media plays.
    let media = out.join("clean.mp4");
    assert!(fs::metadata(&media)
        .map(|m| m.len() > 1000)
        .unwrap_or(false));
}

#[test]
#[ignore = "needs the helper and Screen Recording permission; see the module docs"]
fn a_short_hevc_take_seals_and_verifies() {
    record_and_verify(Codec::Hevc, Duration::from_secs(5), "hevc5");
}

#[test]
#[ignore = "needs the helper and Screen Recording permission; see the module docs"]
fn a_live_capture_of_the_off_screen_fixture_seals_a_verified_bundle() {
    // 30 seconds of H.264 at 30 fps through the core recorder: sealed `complete` and bundle_verify ok.
    record_and_verify(Codec::H264, seconds("RK_RECORD_LIVE_SECONDS", 30), "live30");
}
