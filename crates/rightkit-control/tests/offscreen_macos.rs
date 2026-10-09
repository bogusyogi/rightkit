//! macOS integration test for `launch_offscreen`, using the tiny `offscreen_fixture` example
//! (Accessory-policy NSApplication, one unordered NSWindow + WKWebView; no Dock icon, no activation).
//!
//! `#[ignore]` by default because it creates a real (off-screen) window and needs a window server
//! session. Run explicitly, Mac only:
//!   rightkit cargo test -p rightkit-control --features offscreen-private-spi --test offscreen_macos -- --ignored
//! The window is placed past the right edge of ALL displays and never ordered on a display; the fixture
//! exits on its own after 90 s and `OffscreenLaunch` kills it on drop/end. Fatal signals in the fixture
//! exit silently (no crash dialog). The test asserts the frontmost app did not change.
#![cfg(all(target_os = "macos", feature = "offscreen-private-spi"))]

use rightkit_control::mac;
use rightkit_control::offscreen::{launch_offscreen, LaunchSpec};
use std::time::Duration;

fn fixture() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/offscreen_fixture")
}

#[test]
#[ignore = "creates a real off-screen window; run explicitly on a Mac with a window server"]
fn launches_offscreen_with_native_identity() {
    let front_before = mac::frontmost_pid();
    let mut spec = LaunchSpec::new(fixture());
    spec.report_timeout = Duration::from_secs(30);
    let launch = launch_offscreen(&spec).expect("launch_offscreen");
    let r = launch.report.clone();

    assert!(r.qualified, "app qualified itself");
    assert_eq!(r.pid, launch.pid());
    assert!(r.backing_scale >= 1.0);
    assert!(
        r.placed.x > r.original.x,
        "moved to the right of every display"
    );

    // Native identity: the reported window id exists, belongs to the child, and sits at the placed x (the
    // window server reported ~5 pt of drift from the requested origin on the dev Mac; still far off-display).
    // The CG window list can lag the window server by a moment (seen once as a transient x=0), so poll.
    let mut x = f64::NAN;
    for _ in 0..40 {
        let win = mac::windows(r.pid as i32)
            .into_iter()
            .find(|w| u64::from(w.id) == r.window_id)
            .expect("window id present in the window list");
        x = win.x;
        if (x - r.placed.x).abs() <= 10.0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        (x - r.placed.x).abs() <= 10.0,
        "CG bounds x {x} vs placed {}",
        r.placed.x
    );

    assert_eq!(mac::frontmost_pid(), front_before, "focus untouched");
    launch.end().unwrap();
}

#[test]
#[ignore = "creates a real off-screen window; run explicitly on a Mac with a window server"]
fn identity_cross_check_survives_after_end() {
    let launch = launch_offscreen(&LaunchSpec::new(fixture())).expect("launch_offscreen");
    let pid = launch.pid() as i32;
    launch.end().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(mac::windows(pid).is_empty(), "no window left after end");
}

/// Hidden in-app control (`mac_view`): after `pointer down` the page must report `:active` and a
/// screenshot must show it, even when the control has a CSS transition. A never-shown window runs no
/// rendering updates, which used to pin the transition at its start value (pressed == hover pixels).
///
/// Builds the Tauri control fixture (own workspace) itself unless `RK_CONTROL_TEST_APP` names a binary:
///   rightkit cargo test -p rightkit-control --features offscreen-private-spi,tauri-plugin \
///     --test offscreen_macos hidden_press -- --ignored
/// The app is bundled and launched with `open -g -j -n` (no activation); the test asserts the frontmost
/// app never changed and the window never became key or visible.
#[cfg(feature = "tauri-plugin")]
mod hidden_press {
    use rightkit_control::mac;
    use rightkit_service::{Client, Suite};
    use serde_json::{json, Value};
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    struct Fixture {
        pid: Option<i64>,
        dir: std::path::PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(pid) = self.pid {
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// `RK_CONTROL_TEST_APP` if set, else build the separate-workspace fixture and read its path from
    /// cargo's JSON artifact stream (its target dir is managed, so it cannot be guessed).
    fn control_test_app() -> String {
        if let Ok(p) = std::env::var("RK_CONTROL_TEST_APP") {
            return p;
        }
        let manifest = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/control-test-app/Cargo.toml"
        );
        let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "build",
                "--message-format=json",
                "--manifest-path",
                manifest,
            ])
            .output()
            .expect("cargo build control-test-app");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| {
                v["reason"] == "compiler-artifact" && v["target"]["name"] == "control-test-app"
            })
            .filter_map(|v| v["executable"].as_str().map(str::to_string))
            .next_back()
            .expect("control-test-app executable in cargo output")
    }

    fn eval(c: &Client, js: &str) -> Value {
        let v = c.call("eval", json!({ "js": js })).expect("eval");
        v["value"].clone()
    }

    fn shot(c: &Client) -> Vec<u8> {
        let v = c.call("screenshot", json!({})).expect("screenshot");
        std::fs::read(v["path"].as_str().expect("shot path")).expect("read png")
    }

    fn point(c: &Client, phase: &str, x: f64, y: f64) {
        c.call("pointer", json!({ "phase": phase, "x": x, "y": y }))
            .unwrap_or_else(|e| panic!("pointer {phase}: {e:?}"));
    }

    #[test]
    #[ignore = "launches the Tauri control fixture hidden; builds it first"]
    fn mouse_down_shows_active_styles_including_transitions() {
        let app_bin = control_test_app();
        let dir = std::env::temp_dir().join(format!("rkct-press-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = dir.join("ct.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::copy(&app_bin, app.join("Contents/MacOS/control-test-app")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>control-test-app</string><key>CFBundleIdentifier</key><string>app.rightkit.ct</string><key>CFBundleName</key><string>ct</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleVersion</key><string>1</string></dict></plist>"#,
        )
        .unwrap();
        let mut fixture = Fixture { pid: None, dir };
        let suite_root = fixture.dir.join("suite");
        let front_before = mac::frontmost_pid();

        let status = Command::new("open")
            .args(["-g", "-j", "-n"])
            .args(["--env", "RIGHTKIT_QA_HIDDEN=1"])
            .args(["--env", "RIGHTKIT_QA_BACKGROUND=1"])
            .args(["--env", "RIGHTKIT_CONTROL_SERVICE=control-ct"])
            .args([
                "--env",
                &format!("RIGHTKIT_SUITE_ROOT={}", suite_root.display()),
            ])
            .arg(&app)
            .status()
            .expect("open");
        assert!(status.success());
        let record = suite_root.join("runtime/control-ct/service.json");
        let start = Instant::now();
        while !Path::new(&record).exists() && start.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(250));
        }
        let rec: Value =
            serde_json::from_slice(&std::fs::read(&record).expect("service.json")).unwrap();
        fixture.pid = rec["pid"].as_i64();

        let suite = Suite::at(&suite_root).unwrap();
        let c = loop {
            match Client::connect(&suite, "control-ct", "right-qa") {
                Ok(c) => break c,
                Err(e) if start.elapsed() > Duration::from_secs(30) => panic!("connect: {e:?}"),
                Err(_) => std::thread::sleep(Duration::from_millis(250)),
            }
        };
        while eval(&c, "return document.readyState;") != json!("complete") {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "page never loaded"
            );
            std::thread::sleep(Duration::from_millis(100));
        }

        // `#target` flips blue -> red on :active; `#ttarget` does the same through a 120 ms transition.
        for id in ["target", "ttarget"] {
            let xy = eval(
                &c,
                &format!("const r=document.getElementById('{id}').getBoundingClientRect();return [r.x+r.width/2,r.y+r.height/2];"),
            );
            let (x, y) = (xy[0].as_f64().unwrap(), xy[1].as_f64().unwrap());
            let q = format!("document.getElementById('{id}')");

            point(&c, "move", x, y);
            let hover = shot(&c);
            assert_eq!(
                eval(&c, &format!("return {q}.matches(':active');")),
                json!(false)
            );

            point(&c, "down", x, y);
            assert_eq!(
                eval(&c, &format!("return {q}.matches(':active');")),
                json!(true),
                "{id}: page must report :active after Mouse::Down"
            );
            assert!(
                eval(&c, "return document.querySelectorAll(':active').length;")
                    .as_u64()
                    .unwrap()
                    >= 1
            );
            let pressed = shot(&c);
            assert_ne!(
                hover, pressed,
                "{id}: pressed screenshot must differ from hover (:active styling visible)"
            );

            point(&c, "up", x, y);
            assert_eq!(
                eval(&c, &format!("return {q}.matches(':active');")),
                json!(false)
            );
            let released = shot(&c);
            assert_eq!(
                hover, released,
                "{id}: release returns to the hover rendering"
            );
        }

        let state = c.call("command", json!({"name": "window_state"})).unwrap();
        assert_eq!(state["result"], json!({"key": false, "visible": false}));
        assert_eq!(mac::frontmost_pid(), front_before, "focus untouched");
    }
}
