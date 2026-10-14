//! Live Windows integration test for `launch_offscreen`, using the `offscreen_fixture_win` example
//! (a plain Win32 window hosting WebView2, never activated; see that file for its env contract).
//!
//! `#[ignore]` by default because it creates a real window in the interactive desktop session.
//! Run explicitly, Windows only:
//!   rightkit cargo test -p rightkit-qa-driver --no-default-features --features qa-control,wv2-0_38 --test offscreen_windows -- --ignored
//! The window is moved off every display, so nothing appears on screen after placement. Geometry, identity and
//! focus are read from this test process (GetWindowRect, EnumDisplayMonitors, IsWindowVisible, ...), not from
//! the fixture, so the fixture's self-report is only used for EcoQoS, which only it can read.
#![cfg(windows)]

use rightkit_qa_driver::offscreen::{launch_offscreen, LaunchSpec, OffscreenError};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Hwnd = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
extern "system" {
    fn GetWindowRect(h: Hwnd, r: *mut Rect) -> i32;
    fn IsWindow(h: Hwnd) -> i32;
    fn IsWindowVisible(h: Hwnd) -> i32;
    fn IsIconic(h: Hwnd) -> i32;
    fn GetWindowThreadProcessId(h: Hwnd, pid: *mut u32) -> u32;
    fn GetForegroundWindow() -> Hwnd;
    fn EnumDisplayMonitors(
        hdc: Hwnd,
        clip: *const Rect,
        cb: unsafe extern "system" fn(Hwnd, Hwnd, *mut Rect, isize) -> i32,
        data: isize,
    ) -> i32;
}

/// Serializes the live tests: they share the foreground window and the desktop.
static LOCK: Mutex<()> = Mutex::new(());

fn fixture() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join("offscreen_fixture_win.exe")
}

fn unique_prefix() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("rk-offscreen-win-{}-{nonce}", std::process::id()))
}

fn suffixed(prefix: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{suffix}.json", prefix.display()))
}

fn window_rect(h: Hwnd) -> Rect {
    let mut r = Rect::default();
    assert!(unsafe { GetWindowRect(h, &mut r) } != 0, "GetWindowRect");
    r
}

unsafe extern "system" fn collect_monitor(_: Hwnd, _: Hwnd, r: *mut Rect, data: isize) -> i32 {
    let out = &mut *(data as *mut Vec<Rect>);
    out.push(*r);
    1
}

fn monitors() -> Vec<Rect> {
    let mut v: Vec<Rect> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            collect_monitor,
            &mut v as *mut Vec<Rect> as isize,
        )
    };
    v
}

fn intersects(a: &Rect, b: &Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

fn json_u32(body: &str, key: &str) -> Option<u32> {
    let pat = format!("\"{key}\":");
    let rest = &body[body.find(&pat)? + pat.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn json_u64(body: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\":");
    let rest = &body[body.find(&pat)? + pat.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn raw_rect(h: Hwnd) -> Option<Rect> {
    let mut r = Rect::default();
    (unsafe { GetWindowRect(h, &mut r) } != 0).then_some(r)
}

/// One observation of the window from outside the fixture.
#[derive(Clone, Copy, Debug)]
struct Sample {
    visible: bool,
    rect: Rect,
    foreground: usize,
}

/// Spawns a tight polling loop that starts as soon as the fixture publishes its HWND, and stops when `done`
/// is set. Returns the HWND (if seen) and every sample taken in between.
fn spawn_observer(
    created: PathBuf,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<(Option<usize>, Vec<Sample>)> {
    std::thread::spawn(move || {
        let mut hwnd: Option<usize> = None;
        let mut samples = Vec::new();
        while !done.load(Ordering::Acquire) {
            match hwnd {
                None => {
                    hwnd = std::fs::read_to_string(&created)
                        .ok()
                        .and_then(|b| json_u64(&b, "hwnd"))
                        .map(|v| v as usize);
                }
                Some(h) => {
                    let h = h as Hwnd;
                    if let Some(rect) = raw_rect(h) {
                        samples.push(Sample {
                            visible: unsafe { IsWindowVisible(h) } != 0,
                            rect,
                            foreground: unsafe { GetForegroundWindow() } as usize,
                        });
                    } else {
                        break; // window destroyed
                    }
                }
            }
        }
        (hwnd, samples)
    })
}

fn wait_for_file(path: &Path, timeout: Duration) -> String {
    let start = Instant::now();
    loop {
        if let Ok(body) = std::fs::read_to_string(path) {
            return body;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn base_spec(prefix: &Path, stop: &Path) -> LaunchSpec {
    let mut spec = LaunchSpec::new(fixture());
    spec.report_timeout = Duration::from_secs(30);
    spec.env = vec![
        ("RK_FIXTURE_OUT".into(), prefix.display().to_string()),
        ("RK_FIXTURE_STOP".into(), stop.display().to_string()),
        ("RK_FIXTURE_TTL".into(), "90".into()),
    ];
    spec
}

#[test]
#[ignore = "creates a real window in the interactive desktop; run explicitly on Windows"]
fn live_session_moves_window_off_all_displays_without_focus() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prefix = unique_prefix();
    let stop = PathBuf::from(format!("{}.stop", prefix.display()));
    let session_file = suffixed(&prefix, "session");
    let after_file = suffixed(&prefix, "after");
    let _ = std::fs::remove_file(&stop);

    let fg_before = unsafe { GetForegroundWindow() };
    let launch = launch_offscreen(&base_spec(&prefix, &stop)).expect("launch_offscreen");
    let r = launch.report.clone();
    let hwnd = r.window_id as usize as Hwnd;

    assert!(r.qualified, "fixture qualified itself");
    assert_eq!(r.pid, launch.pid(), "reported pid is the child");
    assert!(unsafe { IsWindow(hwnd) } != 0, "reported HWND exists");
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
    assert_eq!(owner, launch.pid(), "HWND belongs to the launched process");

    // Live geometry, read from this process.
    let rect = window_rect(hwnd);
    let displays = monitors();
    assert!(
        !displays.is_empty(),
        "EnumDisplayMonitors returned monitors"
    );
    for d in &displays {
        assert!(
            !intersects(&rect, d),
            "window {rect:?} intersects monitor {d:?}"
        );
    }
    assert!(
        (rect.left - r.placed.x as i32).abs() <= 1,
        "rect.left {} vs placed.x {}",
        rect.left,
        r.placed.x
    );
    assert!(
        (rect.top - r.placed.y as i32).abs() <= 1,
        "rect.top {} vs placed.y {}",
        rect.top,
        r.placed.y
    );
    assert!(
        unsafe { IsWindowVisible(hwnd) } != 0,
        "window visible during session"
    );
    assert_eq!(
        unsafe { IsIconic(hwnd) },
        0,
        "window not minimized during session"
    );
    assert_eq!(
        unsafe { GetForegroundWindow() },
        fg_before,
        "foreground unchanged after launch"
    );

    // EcoQoS during the session, read inside the fixture.
    let session = wait_for_file(&session_file, Duration::from_secs(10));
    eprintln!(
        "[offscreen_windows] pid={} hwnd={:#x} placed={:?} original={:?} rect={rect:?} monitors={displays:?} \
         visible={} iconic={} fg_before={fg_before:?} fg_during={:?} session={session}",
        r.pid,
        r.window_id,
        r.placed,
        r.original,
        unsafe { IsWindowVisible(hwnd) },
        unsafe { IsIconic(hwnd) },
        unsafe { GetForegroundWindow() },
    );
    assert_eq!(
        json_u32(&session, "control_mask"),
        Some(1),
        "EXECUTION_SPEED control mask set: {session}"
    );
    assert_eq!(
        json_u32(&session, "state_mask"),
        Some(0),
        "opt-out state (StateMask 0): {session}"
    );

    // End the session in the fixture (restore + throttling reset), then check from outside.
    std::fs::write(&stop, b"").expect("write stop file");
    let after = wait_for_file(&after_file, Duration::from_secs(10));
    assert!(
        !after.contains("end_error"),
        "end_active succeeded: {after}"
    );
    assert_eq!(
        json_u32(&after, "control_mask"),
        Some(0),
        "EcoQoS reset to system-managed: {after}"
    );

    let restored = window_rect(hwnd);
    assert_eq!(
        unsafe { IsWindowVisible(hwnd) },
        0,
        "window hidden after end"
    );
    assert_eq!(
        unsafe { IsIconic(hwnd) },
        0,
        "window not minimized after end"
    );
    assert_eq!(restored.left, r.original.x as i32, "restored x");
    assert_eq!(restored.top, r.original.y as i32, "restored y");
    assert_eq!(
        restored.right - restored.left,
        r.original.w as i32,
        "width unchanged"
    );
    assert_eq!(
        restored.bottom - restored.top,
        r.original.h as i32,
        "height unchanged"
    );
    assert_eq!(
        unsafe { GetForegroundWindow() },
        fg_before,
        "foreground unchanged after end"
    );
    eprintln!(
        "[offscreen_windows] after_end rect={restored:?} visible={} iconic={} fg_after={:?} qos_after={after}",
        unsafe { IsWindowVisible(hwnd) },
        unsafe { IsIconic(hwnd) },
        unsafe { GetForegroundWindow() },
    );

    launch.end().expect("kill fixture");
    let _ = std::fs::remove_file(&session_file);
    let _ = std::fs::remove_file(&after_file);
}

#[test]
#[ignore = "creates a real window in the interactive desktop; run explicitly on Windows"]
fn hidden_at_start_window_is_placed_and_shown_without_ever_appearing_on_a_display() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prefix = unique_prefix();
    let stop = PathBuf::from(format!("{}.stop", prefix.display()));
    let created = suffixed(&prefix, "created");
    let _ = std::fs::remove_file(&created);
    let displays = monitors();
    let fg_before = unsafe { GetForegroundWindow() };

    // The observer runs from the moment the fixture publishes its (hidden) HWND until the launch returns.
    let done = Arc::new(AtomicBool::new(false));
    let observer = spawn_observer(created.clone(), done.clone());
    let launch = launch_offscreen(&base_spec(&prefix, &stop));
    done.store(true, Ordering::Release);
    let (hwnd, samples) = observer.join().expect("observer thread");
    let launch = launch.expect("launch_offscreen");
    let r = launch.report.clone();
    let hwnd = hwnd.expect("fixture published its HWND") as Hwnd;

    assert_eq!(
        r.window_id as usize as Hwnd, hwnd,
        "report HWND is the published HWND"
    );
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
    assert_eq!(owner, launch.pid(), "HWND belongs to the launched process");

    // Never visible on a display: every sample taken while visible must be off every monitor.
    let hidden_samples = samples.iter().filter(|s| !s.visible).count();
    let on_display: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.visible && displays.iter().any(|d| intersects(&s.rect, d)))
        .collect();
    eprintln!(
        "[offscreen_windows] observed {} samples: {} hidden, {} visible, {} visible on a display; created_at={:?}",
        samples.len(),
        hidden_samples,
        samples.len() - hidden_samples,
        on_display.len(),
        samples.first().map(|s| s.rect),
    );
    assert!(
        hidden_samples > 0,
        "observed the window hidden before placement"
    );
    assert!(
        on_display.is_empty(),
        "window was visible on a display: {on_display:?}"
    );
    assert!(
        samples.iter().all(|s| s.foreground == fg_before as usize),
        "foreground changed while observed"
    );

    // Final state: visible, off every monitor, at the placed frame, foreground unchanged.
    let rect = window_rect(hwnd);
    assert!(
        unsafe { IsWindowVisible(hwnd) } != 0,
        "window visible after start"
    );
    assert_eq!(
        unsafe { IsIconic(hwnd) },
        0,
        "window not minimized after start"
    );
    for d in &displays {
        assert!(
            !intersects(&rect, d),
            "window {rect:?} intersects monitor {d:?}"
        );
    }
    assert!(
        (rect.left - r.placed.x as i32).abs() <= 1,
        "rect.left {} vs placed.x {}",
        rect.left,
        r.placed.x
    );
    assert_eq!(
        r.original.x, 100.0,
        "fixture created the window at its on-screen origin"
    );
    assert_eq!(
        unsafe { GetForegroundWindow() },
        fg_before,
        "foreground unchanged after start"
    );

    launch.end().expect("kill fixture");
    let _ = std::fs::remove_file(&created);
}

#[test]
#[ignore = "creates a real window in the interactive desktop; run explicitly on Windows"]
fn minimized_window_is_refused() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prefix = unique_prefix();
    let stop = PathBuf::from(format!("{}.stop", prefix.display()));
    let mut spec = base_spec(&prefix, &stop);
    spec.env
        .push(("RK_FIXTURE_MODE".into(), "minimized".into()));
    match launch_offscreen(&spec) {
        Err(OffscreenError::AppReportedError(msg)) => {
            assert!(
                msg.contains("WindowMiniaturized"),
                "refusal names WindowMiniaturized: {msg}"
            );
        }
        Err(e) => panic!("expected the fixture to report a refusal, got {e:?}"),
        Ok(launch) => {
            let pid = launch.pid();
            launch.end().ok();
            panic!("minimized window was accepted (pid {pid})");
        }
    }
    let _ = std::fs::remove_file(suffixed(&prefix, "created"));
}
