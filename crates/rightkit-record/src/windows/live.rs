//! Live test of the Windows helper driven out of process by the core recorder, against an
//! off-screen rk2 target window. `#[ignore]`: it needs the built helper, the rk2 spike target and
//! ffmpeg/ffprobe.
//!
//! Run:
//! ```text
//! rightkit cargo build --release --bin rightkit-record-helper
//! RIGHTKIT_RECORD_WINDOWS_HELPER=<path to rightkit-record-helper.exe> \
//! RIGHTKIT_RECORD_FFMPEG_DIR=<dir holding ffmpeg.exe and ffprobe.exe> \
//!   rightkit cargo test -p rightkit-record --lib windows::live -- --ignored --test-threads=1
//! ```
//! `RIGHTKIT_RECORD_WINDOWS_TARGET` overrides the rk2 target path. `RK_RECORD_KEEP=1` keeps the bundle.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_contracts::types::record_session::{
    BundleStatus, Surface, SurfaceKind, VisibilityState,
};
use serde_json::Value;

use crate::backend::Codec;
use crate::clock::SystemClock;
use crate::plan::{ScenarioSpec, ScenarioStep, StartRequest, TargetSpec};
use crate::process::ProcessBackend;
use crate::recorder::Recorder;
use crate::verify::VerifyOptions;

const DEFAULT_TARGET: &str =
    r"D:\Claude\tools\rightkit\spikes\rk2-windows\bin\Release\net10.0-windows10.0.22621.0\rk2.exe";

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from)
}

fn helper() -> PathBuf {
    env_path("RIGHTKIT_RECORD_WINDOWS_HELPER")
        .expect("set RIGHTKIT_RECORD_WINDOWS_HELPER to the built rightkit-record-helper.exe")
}

fn media_tool(name: &str) -> PathBuf {
    env_path("RIGHTKIT_RECORD_FFMPEG_DIR")
        .map(|dir| dir.join(format!("{name}.exe")))
        .unwrap_or_else(|| PathBuf::from(name))
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("rk-record-win-{name}-{}", std::process::id()));
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
    let meta = fs::symlink_metadata(path)?;
    let mut perms = meta.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(path, perms)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            let _ = make_writable(&entry?.path());
        }
    }
    Ok(())
}

/// The rk2 target window, moved fully off every monitor. Told to exit when dropped.
struct Target {
    child: Child,
    control: PathBuf,
    hwnd: u64,
}

impl Target {
    fn launch(dir: &Path) -> Target {
        let exe =
            env_path("RIGHTKIT_RECORD_WINDOWS_TARGET").unwrap_or_else(|| DEFAULT_TARGET.into());
        let (hwnd_file, ready, control) = (
            dir.join("hwnd.txt"),
            dir.join("ready.txt"),
            dir.join("control.txt"),
        );
        let child = Command::new(&exe)
            .arg("target")
            .arg("--hwnd-file")
            .arg(&hwnd_file)
            .arg("--ready-file")
            .arg(&ready)
            .arg("--control")
            .arg(&control)
            .args(["--x", "100", "--y", "100", "--w", "800", "--h", "520"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("start the rk2 target {}: {e}", exe.display()));
        assert!(
            wait_for(Duration::from_secs(60), || ready.is_file()),
            "rk2 target never became ready"
        );
        let hwnd: u64 = fs::read_to_string(&hwnd_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let moved = Command::new(&exe)
            .args([
                "window",
                "--hwnd",
                &hwnd.to_string(),
                "--op",
                "move",
                "--x",
                "-10000",
                "--y",
                "-10000",
            ])
            .status()
            .expect("move the target off-screen");
        assert!(moved.success(), "rk2 window move failed");
        thread::sleep(Duration::from_secs(1));
        Target {
            child,
            control,
            hwnd,
        }
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        let _ = fs::write(&self.control, "exit");
        if !wait_for(Duration::from_secs(5), || {
            matches!(self.child.try_wait(), Ok(Some(_)))
        }) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn request(out: PathBuf, target: &Target) -> StartRequest {
    let mut request = StartRequest::new(
        TargetSpec {
            app_id: "dev.rightkit.rk2-target".into(),
            window_id: target.hwnd,
            pid: target.child.id(),
            surfaces: vec![Surface {
                surface_id: "main".into(),
                kind: SurfaceKind::MainWindow,
                parent_surface_id: None,
                native_id: target.hwnd.to_string(),
                supported: true,
                unsupported_reason: None,
            }],
        },
        ScenarioSpec {
            scenario_id: "live.rk2".into(),
            steps: vec![ScenarioStep {
                step_id: "step-1".into(),
                label: "Watch".into(),
                say: None,
            }],
        },
        out,
    );
    request.key_source =
        crate::redact::KeyHandle::new(crate::redact::FixedKeySource::new([0x5a; 32]));
    request.fps = 30;
    request.codec = Codec::H264;
    request.visibility = VisibilityState::OffScreenVisible;
    request
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

/// R14 on Windows: the helper is terminated (TerminateProcess) about five seconds into a take
/// driven by the core. The fragmented MP4 it leaves must decode, the take must quarantine as
/// `helper_lost` (never `complete`), and the decoder must count the frames the core recovered.
#[test]
#[ignore = "needs the built helper, the rk2 target and ffmpeg/ffprobe; see the module docs"]
fn a_terminated_helper_leaves_quarantined_media_that_decodes() {
    let scratch = Scratch::new("terminate");
    let target = Target::launch(&scratch.0);
    let bundle = scratch.0.join("take");

    let mut child = Command::new(helper())
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the helper");
    let helper_pid = child.id();
    let backend =
        ProcessBackend::connect(child.stdout.take().unwrap(), child.stdin.take().unwrap())
            .expect("connect to the helper");
    let (mut recorder, _) = Recorder::open(backend, SystemClock, &[], 30_000).unwrap();
    recorder
        .record_start(request(bundle.clone(), &target))
        .expect("start the take");

    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(100));
        let status = recorder.record_status().expect("status");
        assert!(
            status.failure.is_none(),
            "take failed early: {:?}",
            status.failure
        );
    }
    // Child::kill is TerminateProcess on Windows.
    child.kill().expect("TerminateProcess the helper");
    let _ = child.wait();

    assert!(
        wait_for(Duration::from_secs(10), || {
            recorder
                .record_status()
                .map(|s| s.outcome.is_some())
                .unwrap_or(false)
        }),
        "the core never noticed the helper's death"
    );
    let outcome = recorder.record_stop().expect("outcome");
    eprintln!("terminate (helper pid {helper_pid}): {outcome:?}");
    assert_eq!(
        outcome.status,
        BundleStatus::PartialQuarantined,
        "{outcome:?}"
    );
    assert_eq!(outcome.reason.as_deref(), Some("helper_lost"));

    let media = outcome
        .recovered_media
        .expect("the quarantined media must be playable and recorded");
    assert!(
        media.duration_ms >= 4_000,
        "recovered only {} ms of media",
        media.duration_ms
    );
    let report = recorder
        .bundle_verify(&bundle, &VerifyOptions::default())
        .expect("verify");
    assert!(report.ok(), "bundle_verify problems: {:?}", report.problems);

    let reader = crate::reader::BundleReader::open(&bundle).expect("open the quarantined bundle");
    assert!(reader.frames().len() as u64 >= media.samples);

    let clip = bundle.join("clean.mp4");
    let decode = Command::new(media_tool("ffmpeg"))
        .args(["-v", "error", "-i"])
        .arg(&clip)
        .args(["-fps_mode", "passthrough", "-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    let decode_err = String::from_utf8_lossy(&decode.stderr);
    assert!(
        decode.status.success()
            && !decode_err.contains("Invalid data")
            && !decode_err.contains("error while decoding"),
        "ffmpeg could not decode the killed take's media: {decode_err}"
    );
    let probe = Command::new(media_tool("ffprobe"))
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_frames:format=duration",
            "-of",
            "json",
        ])
        .arg(&clip)
        .output()
        .expect("run ffprobe");
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let info: Value = serde_json::from_slice(&probe.stdout).unwrap();
    let decoded_frames: u64 = info["streams"][0]["nb_read_frames"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let seconds: f64 = info["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    eprintln!(
        "terminate: decoded {decoded_frames} frames, {seconds:.2} s; core recovered {} frames, {} ms of {} accepted",
        media.samples, media.duration_ms, media.accepted_frames
    );
    assert!(seconds >= 4.0, "media decodes only {seconds} s");
    assert!(
        decoded_frames >= 4 * 10,
        "only {decoded_frames} frames decode"
    );
    assert_eq!(
        decoded_frames, media.samples,
        "the structural probe and the decoder must count the same frames"
    );
}
