//! The shell's platform-independent decisions: navigation and popups, deep links, window
//! placement across monitor changes, the watchdog trigger, cache-dir repair, and base64.

use rightkit_shell::deep_link::{self, DeepLinkQueue};
use rightkit_shell::nav::{external_scheme_allowed, origin_of, WindowOpenDecision};
use rightkit_shell::recovery::{ensure_cache_dir, CacheDirOutcome};
use rightkit_shell::window_state::{restoreable_state, MonitorSnapshot, WindowState};
use rightkit_shell::{b64, instance, watchdog, NavDecision, NavPolicy};
use rightkit_shell::{HardeningScript, Shell};
use tauri::plugin::Plugin;
use tauri::Url;

#[test]
fn full_plugin_preserves_all_frame_default_and_supports_top_frame_or_off() {
    let default = Shell::builder("test-shell").build().plugin();
    let script = default.initialization_script_2().unwrap();
    assert!(!script.for_main_frame_only);
    assert!(script.script.contains("root.__rkShell"));
    assert!(!script.script.contains("window.top !== window.self"));

    let top = Shell::builder("test-shell")
        .hardening_script(HardeningScript::TopFrameOnly)
        .build()
        .plugin();
    let script = top.initialization_script_2().unwrap();
    assert!(script.for_main_frame_only);
    // WebView2 injects into child frames too: return before any DOM hardening runs.
    assert!(script
        .script
        .starts_with("if (window.top !== window.self) { return; }"));
    assert!(script.script.contains("root.__rkShell"));

    let off = Shell::builder("test-shell")
        .hardening_script(HardeningScript::Off)
        .build()
        .plugin();
    assert!(off.initialization_script_2().is_none());
}

#[test]
fn single_instance_plugin_has_no_hardening_script_even_with_full_shell_defaults() {
    let plugin = Shell::builder("test-single-instance")
        .build()
        .single_instance_plugin(|_app, _argv, _cwd| {});
    assert_eq!(plugin.name(), "rightkit-single-instance");
    assert!(plugin.initialization_script_2().is_none());
    assert!(plugin.initialization_script().is_none());
}

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn release_policy() -> NavPolicy {
    NavPolicy {
        allow_loopback: false,
        broker_host: Some("open.viewright.invalid".into()),
        ..NavPolicy::default()
    }
}

#[test]
fn app_content_loads_in_place_web_leaves_and_unknown_schemes_are_blocked() {
    let p = release_policy();
    assert_eq!(
        p.decide(&url("tauri://localhost/index.html")),
        NavDecision::Allow
    );
    assert_eq!(
        p.decide(&url("http://tauri.localhost/settings")),
        NavDecision::Allow
    );
    assert_eq!(
        p.decide(&url("https://example.com/a")),
        NavDecision::External {
            url: "https://example.com/a".into()
        }
    );
    assert_eq!(
        p.decide(&url("mailto:hi@example.com")),
        NavDecision::External {
            url: "mailto:hi@example.com".into()
        }
    );
    assert_eq!(
        p.decide(&url("file:///C:/Windows/win.ini")),
        NavDecision::Block
    );
    assert_eq!(p.decide(&url("javascript:alert(1)")), NavDecision::Block);
    // Loopback (a dev server) is only in place when allowed.
    assert!(matches!(
        p.decide(&url("http://localhost:5173/")),
        NavDecision::External { .. }
    ));
    let dev = NavPolicy {
        allow_loopback: true,
        ..release_policy()
    };
    assert_eq!(
        dev.decide(&url("http://localhost:5173/")),
        NavDecision::Allow
    );
}

#[test]
fn the_open_broker_forwards_only_external_schemes() {
    let p = release_policy();
    let ok = url("https://open.viewright.invalid/?target=https%3A%2F%2Fexample.com%2Fdoc");
    assert_eq!(
        p.decide(&ok),
        NavDecision::External {
            url: "https://example.com/doc".into()
        }
    );
    let bad = url("https://open.viewright.invalid/?target=file%3A%2F%2F%2Fetc%2Fpasswd");
    assert_eq!(p.decide(&bad), NavDecision::Block);
    assert_eq!(
        p.decide(&url("https://open.viewright.invalid/")),
        NavDecision::Block
    );
    assert!(!external_scheme_allowed("https://example.com/\u{7}bell"));
    assert!(!external_scheme_allowed("javascript:alert(1)"));
}

#[test]
fn only_sized_popups_to_pinned_oauth_origins_open_in_app() {
    let p = release_policy();
    let github = url("https://github.com/login/oauth/authorize?client_id=x");
    let app = url("tauri://localhost/");
    assert_eq!(
        p.decide_window_open(&github, true, Some(&app)),
        WindowOpenDecision::Popup
    );
    assert!(matches!(
        p.decide_window_open(&github, false, Some(&app)),
        WindowOpenDecision::External { .. }
    ));
    let foreign_opener = url("https://evil.example/");
    assert!(matches!(
        p.decide_window_open(&github, true, Some(&foreign_opener)),
        WindowOpenDecision::External { .. }
    ));
    assert!(matches!(
        p.decide_window_open(&url("https://evil.example/x"), true, Some(&app)),
        WindowOpenDecision::External { .. }
    ));
    assert_eq!(
        p.decide_window_open(&url("file:///x"), true, Some(&app)),
        WindowOpenDecision::Deny
    );
    assert_eq!(
        origin_of(&url("https://github.com:443/a")).as_deref(),
        Some("https://github.com")
    );
    assert_eq!(
        origin_of(&url("http://127.0.0.1:8080/")).as_deref(),
        Some("http://127.0.0.1:8080")
    );
}

#[test]
fn deep_links_parse_strictly_and_queue_until_the_frontend_is_ready() {
    let schemes = vec!["viewright".to_string()];
    let link = deep_link::parse(
        "ViewRight://open/photos/caf%C3%A9.jpg?zoom=2&mode=a+b",
        &schemes,
    )
    .unwrap();
    assert_eq!(link.scheme, "viewright");
    assert_eq!(link.host, "open");
    assert_eq!(link.segments, ["photos", "café.jpg"]);
    assert_eq!(link.query.get("mode").map(String::as_str), Some("a b"));
    for bad in [
        "other://open/x",
        "viewright://open/../secrets",
        "viewright://open/a%2Fb",
        "viewright://open/x\u{0}",
        "viewright:///no-host",
    ] {
        assert!(
            deep_link::parse(bad, &schemes).is_none(),
            "{bad:?} must be rejected"
        );
    }
    let too_long = format!("viewright://open/{}", "a".repeat(5000));
    assert!(deep_link::parse(&too_long, &schemes).is_none());

    let queue = DeepLinkQueue::default();
    let pending = queue.push_all(
        ["viewright://open/a", "other://x", "viewright://open/b"],
        &schemes,
    );
    assert_eq!(pending, 2, "invalid links never queue");
    let drained = queue.drain();
    assert_eq!(
        drained
            .iter()
            .map(|l| l.segments.concat())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(queue.drain().is_empty());
}

fn monitor(x: f64, y: f64, w: f64, h: f64) -> MonitorSnapshot {
    MonitorSnapshot {
        x,
        y,
        width: w,
        height: h,
        scale: 1.0,
    }
}

fn state(x: f64, y: f64, w: f64, h: f64, on: MonitorSnapshot) -> WindowState {
    WindowState {
        x,
        y,
        width: w,
        height: h,
        maximized: false,
        minimized: false,
        monitor: on,
        version: 1,
    }
}

#[test]
fn windows_reopen_on_screen_after_monitors_change() {
    let laptop = monitor(0.0, 0.0, 1440.0, 900.0);
    let external = monitor(1440.0, 0.0, 2560.0, 1440.0);
    let saved = state(1600.0, 100.0, 1200.0, 800.0, external);

    // Same layout: unchanged.
    let r = restoreable_state(&saved, &[laptop, external]);
    assert_eq!(
        (r.x, r.y, r.width, r.height),
        (1600.0, 100.0, 1200.0, 800.0)
    );

    // External display unplugged: the window lands on the laptop, fully reachable.
    let r = restoreable_state(&saved, &[laptop]);
    assert!(
        r.x >= laptop.x - r.width && r.x <= laptop.x + laptop.width - 80.0,
        "{r:?}"
    );
    assert!(r.y >= 0.0 && r.y <= laptop.height - 40.0, "{r:?}");
    assert!(
        r.width <= laptop.width && r.height <= laptop.height,
        "{r:?}"
    );

    // A tiny saved size is raised to the minimum usable size.
    let r = restoreable_state(&state(10.0, 10.0, 100.0, 50.0, laptop), &[laptop]);
    assert_eq!((r.width, r.height), (600.0, 400.0));
}

#[test]
fn watchdog_fires_once_per_stall() {
    assert!(!watchdog::should_fire(1_000, 1_500, false, 1_000));
    assert!(watchdog::should_fire(1_000, 2_500, false, 1_000));
    assert!(!watchdog::should_fire(1_000, 2_500, true, 1_000));
    assert!(
        !watchdog::should_fire(5_000, 1_000, false, 1_000),
        "clock skew never fires"
    );
    assert_eq!(
        instance::service_name("dev.rightapps.viewright"),
        "dev.rightapps.viewright-shell"
    );
}

#[test]
fn cache_dirs_are_created_kept_or_repaired() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("cache");
    assert_eq!(ensure_cache_dir(&dir).unwrap(), CacheDirOutcome::Created);
    assert_eq!(ensure_cache_dir(&dir).unwrap(), CacheDirOutcome::Existing);
    let file = root.path().join("was-a-file");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(ensure_cache_dir(&file).unwrap(), CacheDirOutcome::Repaired);
    assert!(file.is_dir());
}

#[test]
fn base64_round_trips_and_rejects_garbage() {
    for data in [&b""[..], b"f", b"fo", b"foo", b"\x00\xff\x10binary"] {
        assert_eq!(b64::decode(&b64::encode(data)).unwrap(), data);
    }
    assert_eq!(b64::encode(b"foobar"), "Zm9vYmFy");
    assert!(b64::decode("not base64!").is_none());
}

#[test]
fn a_host_can_add_internal_schemes_and_the_default_set_is_unchanged() {
    let default = release_policy();
    assert_eq!(
        default.decide(&url("pulse-app://localhost/x")),
        NavDecision::Block
    );

    let p = release_policy().with_internal_scheme("Pulse-App").unwrap();
    assert_eq!(
        p.decide(&url("pulse-app://localhost/x")),
        NavDecision::Allow
    );
    // The built-ins and the other decisions are untouched.
    assert_eq!(
        p.decide(&url("tauri://localhost/index.html")),
        NavDecision::Allow
    );
    assert_eq!(p.decide(&url("ftp://example.com/")), NavDecision::Block);
    assert!(matches!(
        p.decide(&url("https://example.com/")),
        NavDecision::External { .. }
    ));
}

#[test]
fn schemes_that_would_open_the_webview_to_the_world_are_refused() {
    let refused = [
        "",
        "javascript",
        "file",
        "http",
        "https",
        "mailto",
        "vbscript",
        "9app",
        "a b",
        "a:b",
    ];
    for bad in refused {
        assert!(
            release_policy().with_internal_scheme(bad).is_err(),
            "{bad:?}"
        );
    }
    let p = release_policy().with_internal_scheme("my-app+x.y").unwrap();
    assert_eq!(p.internal_schemes, vec!["my-app+x.y".to_owned()]);
}
