//! Transport and state tests: per-type stream caps, clipboard policy and DIB conversion, setup
//! recovery (exact path, validation, one-time migration), cache-directory repair, the modular plugin
//! install, the idle failure callback, and the path broker used as a plain library type.

use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime};

use rightkit_shell::broker::PathBroker;
use rightkit_shell::clipboard::{
    dib_to_bmp, item_from_snapshot, purge_stale, ClipboardImage, ClipboardItem, ClipboardPolicy,
    ClipboardSnapshot, DEFAULT_PURGE_AFTER,
};
use rightkit_shell::idle::{IdleFailure, IdleHost, IdlePhase};
use rightkit_shell::recovery::{MigrationOutcome, RecoverySpec, RecoveryStore};
use rightkit_shell::startup_cache::{ensure_cache_dir, ensure_http_cache_dir, CacheDirOutcome};
use rightkit_shell::stream::{mime_for, serve, StreamPolicy};
use rightkit_shell::watchdog::{StallDetector, WatchdogConfig, DEFAULT_POLL_INTERVAL};
use rightkit_shell::{Module, ModuleSet, Shell, ShellError, ShellPlugin};
use tauri::http::{Request, Response, StatusCode};
use tauri::plugin::Plugin;

// ---------------------------------------------------------------------------------------------
// Stream caps
// ---------------------------------------------------------------------------------------------

fn stream_sync(broker: &PathBroker, policy: &StreamPolicy, id: &str) -> Response<Vec<u8>> {
    let request = Request::builder()
        .uri(format!("demo-stream://localhost/{id}"))
        .body(Vec::new())
        .unwrap();
    let (tx, rx) = mpsc::channel();
    serve(broker, policy, &request, move |r| {
        let _ = tx.send(r);
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap()
}

fn granted_file(dir: &tempfile::TempDir, name: &str, len: usize) -> (PathBroker, String) {
    let path = dir.path().join(name);
    std::fs::write(&path, vec![b'x'; len]).unwrap();
    let broker = PathBroker::default();
    let granted = broker.grant_file(&path).unwrap();
    let id = broker.grant_stream(&granted).unwrap();
    (broker, id)
}

#[test]
fn ico_is_served_as_an_icon() {
    assert_eq!(mime_for(Path::new("app.ico")), Some("image/x-icon"));
    assert_eq!(mime_for(Path::new("APP.ICO")), Some("image/x-icon"));
}

#[test]
fn per_kind_cap_refuses_oversize_images_and_leaves_other_kinds_on_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let policy = StreamPolicy::default().with_kind_cap("image", 10);

    let (broker, png) = granted_file(&dir, "big.png", 20);
    assert_eq!(
        stream_sync(&broker, &policy, &png).status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );

    let (broker, pdf) = granted_file(&dir, "doc.pdf", 20);
    assert_eq!(stream_sync(&broker, &policy, &pdf).status(), StatusCode::OK);
}

#[test]
fn extension_key_beats_family_and_full_mime_key_applies_to_its_type() {
    let dir = tempfile::tempdir().unwrap();
    let policy = StreamPolicy::default()
        .with_kind_cap("image", 10)
        .with_kind_cap(".png", 100)
        .with_kind_cap("application/pdf", 5);

    let (broker, png) = granted_file(&dir, "wide.png", 20);
    assert_eq!(stream_sync(&broker, &policy, &png).status(), StatusCode::OK);

    let (broker, jpg) = granted_file(&dir, "photo.jpg", 20);
    assert_eq!(
        stream_sync(&broker, &policy, &jpg).status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );

    let (broker, pdf) = granted_file(&dir, "tiny.pdf", 10);
    assert_eq!(
        stream_sync(&broker, &policy, &pdf).status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[test]
fn max_bytes_for_resolves_the_most_specific_cap() {
    let policy = StreamPolicy {
        max_bytes: 1000,
        ..StreamPolicy::default()
    }
    .with_kind_cap("video/*", 500)
    .with_kind_cap("mov", 200);
    assert_eq!(policy.max_bytes_for(Path::new("a.mov")), 200);
    assert_eq!(policy.max_bytes_for(Path::new("a.mp4")), 500);
    assert_eq!(policy.max_bytes_for(Path::new("a.pdf")), 1000);
    assert_eq!(
        StreamPolicy::default().max_bytes_for(Path::new("a.mp4")),
        150 * 1024 * 1024
    );
}

// ---------------------------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------------------------

/// A 1x1 24-bit DIB: a 40-byte BITMAPINFOHEADER and one padded pixel row.
fn one_pixel_dib() -> Vec<u8> {
    let mut dib = vec![0u8; 40];
    dib[0..4].copy_from_slice(&40u32.to_le_bytes());
    dib[4..8].copy_from_slice(&1i32.to_le_bytes());
    dib[8..12].copy_from_slice(&1i32.to_le_bytes());
    dib[12..14].copy_from_slice(&1u16.to_le_bytes());
    dib[14..16].copy_from_slice(&24u16.to_le_bytes());
    dib.extend_from_slice(&[0, 0, 255, 0]);
    dib
}

#[test]
fn dib_becomes_a_bmp_with_the_pixel_offset_after_the_headers() {
    let dib = one_pixel_dib();
    let bmp = dib_to_bmp(&dib).unwrap();
    assert_eq!(&bmp[0..2], b"BM");
    assert_eq!(bmp.len(), 14 + dib.len());
    assert_eq!(
        u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize,
        bmp.len()
    );
    assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);
    assert_eq!(&bmp[14..], &dib[..]);
    assert!(dib_to_bmp(&[0u8; 8]).is_none(), "too short to be a DIB");
}

#[test]
fn clipboard_priority_is_files_then_image_then_text() {
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("temp");
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "hello").unwrap();
    let broker = PathBroker::default();
    let policy = ClipboardPolicy::default();

    let files = ClipboardSnapshot {
        files: vec![file.clone()],
        image: Some(ClipboardImage {
            extension: "png".into(),
            bytes: vec![1, 2, 3],
        }),
        text: Some("ignored".into()),
    };
    match item_from_snapshot(&files, &broker, &temp, policy).unwrap() {
        ClipboardItem::Files { paths } => {
            assert_eq!(paths.len(), 1);
            assert!(
                broker.require_file(&paths[0]).is_ok(),
                "file must be granted"
            );
        }
        other => panic!("files must win, got {other:?}"),
    }

    let image_only = ClipboardSnapshot {
        image: Some(ClipboardImage {
            extension: "bmp".into(),
            bytes: one_pixel_dib(),
        }),
        text: Some("ignored".into()),
        ..Default::default()
    };
    let path = match item_from_snapshot(&image_only, &broker, &temp, policy).unwrap() {
        ClipboardItem::Image { path } => path,
        other => panic!("image must beat text, got {other:?}"),
    };
    assert!(path.ends_with(".bmp"));
    assert_eq!(std::fs::read(&path).unwrap(), one_pixel_dib());
    assert!(
        broker.require_file(&path).is_ok(),
        "temp image must be granted"
    );

    let text = ClipboardSnapshot {
        text: Some("hi".into()),
        ..Default::default()
    };
    assert_eq!(
        item_from_snapshot(&text, &broker, &temp, policy).unwrap(),
        ClipboardItem::Text { text: "hi".into() }
    );
    let empty_text = ClipboardSnapshot {
        text: Some(String::new()),
        ..Default::default()
    };
    assert_eq!(
        item_from_snapshot(&empty_text, &broker, &temp, policy).unwrap(),
        ClipboardItem::None
    );
}

#[test]
fn clipboard_items_serialise_with_a_kind_tag() {
    assert_eq!(
        serde_json::to_value(ClipboardItem::None).unwrap(),
        serde_json::json!({"kind": "none"})
    );
    assert_eq!(
        serde_json::to_value(ClipboardItem::Image {
            path: "/tmp/x.png".into()
        })
        .unwrap(),
        serde_json::json!({"kind": "image", "path": "/tmp/x.png"})
    );
}

fn backdate(path: &Path, age: Duration) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - age).unwrap();
}

#[test]
fn temp_files_older_than_the_purge_age_are_removed_and_fresh_ones_kept() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("clipboard-old.png");
    let fresh = dir.path().join("clipboard-fresh.png");
    std::fs::write(&old, b"old").unwrap();
    std::fs::write(&fresh, b"new").unwrap();
    backdate(&old, Duration::from_secs(2 * 60 * 60));

    assert_eq!(purge_stale(dir.path(), Duration::from_secs(60 * 60)), 1);
    assert!(!old.exists());
    assert!(fresh.exists());
}

#[test]
fn the_purge_age_is_configurable_and_defaults_to_one_hour() {
    assert_eq!(DEFAULT_PURGE_AFTER, Duration::from_secs(60 * 60));
    assert_eq!(ClipboardPolicy::default().purge_after, DEFAULT_PURGE_AFTER);

    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("clipboard-old.bin");
    std::fs::write(&old, b"old").unwrap();
    backdate(&old, Duration::from_secs(2 * 60 * 60));
    // A three-hour policy keeps a two-hour-old file through an ordinary paste.
    let policy = ClipboardPolicy {
        purge_after: Duration::from_secs(3 * 60 * 60),
    };
    let text = ClipboardSnapshot {
        text: Some("x".into()),
        ..Default::default()
    };
    item_from_snapshot(&text, &PathBroker::default(), dir.path(), policy).unwrap();
    assert!(old.exists());
}

// ---------------------------------------------------------------------------------------------
// Setup recovery
// ---------------------------------------------------------------------------------------------

fn snapshot_for(path: &Path) -> String {
    serde_json::json!({
        "version": 1,
        "documents": [{"document": {"path": path.to_string_lossy()}}]
    })
    .to_string()
}

#[test]
fn an_exact_snapshot_path_is_used_verbatim_and_round_trips_through_the_broker() {
    let dir = tempfile::tempdir().unwrap();
    let exact = dir.path().join("custom").join("session.snapshot.json");
    let store = RecoveryStore::new(
        RecoverySpec::documents().at_path(&exact),
        &dir.path().join("state"),
    );
    assert_eq!(store.path(), exact.as_path());

    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, "x").unwrap();
    let broker = PathBroker::default();
    let raw = snapshot_for(&doc);

    // Authority: an ungranted document is refused before anything is written.
    assert!(matches!(
        store.save(&raw, &broker),
        Err(ShellError::BrokerDenied(_))
    ));
    assert!(!exact.exists());

    broker.grant_file(&doc).unwrap();
    store.save(&raw, &broker).unwrap();
    assert!(exact.exists());
    // Atomic write leaves no sidecar files behind.
    let names: Vec<String> = std::fs::read_dir(exact.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["session.snapshot.json".to_string()]);

    let reader = PathBroker::default();
    assert_eq!(store.load(&reader).unwrap().as_deref(), Some(raw.as_str()));
    assert!(
        reader.require_file(doc.to_str().unwrap()).is_ok(),
        "load re-grants"
    );

    store.clear().unwrap();
    assert_eq!(store.load(&reader).unwrap(), None);
}

#[test]
fn default_recovery_path_keeps_the_versioned_file_name() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::new(RecoverySpec::documents(), dir.path());
    assert_eq!(
        store.path(),
        dir.path().join("recovery").join("setup-recovery-v1.json")
    );
}

#[test]
fn legacy_snapshot_migrates_once_and_never_clobbers_a_newer_one() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("old").join("setup-recovery-v1.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let raw = r#"{"version":1,"documents":[]}"#;
    std::fs::write(&legacy, raw).unwrap();

    let store = RecoveryStore::new(
        RecoverySpec::documents().at_path(dir.path().join("current.json")),
        dir.path(),
    );
    assert_eq!(
        store.migrate_legacy(&legacy).unwrap(),
        MigrationOutcome::Migrated
    );
    assert_eq!(std::fs::read_to_string(store.path()).unwrap(), raw);
    assert!(!legacy.exists(), "a migrated legacy file is removed");
    assert_eq!(
        store.migrate_legacy(&legacy).unwrap(),
        MigrationOutcome::NoLegacy
    );

    // A current snapshot wins: the legacy file is kept, nothing is overwritten.
    std::fs::write(
        &legacy,
        r#"{"version":1,"documents":[{"document":{"path":"untitled:1"}}]}"#,
    )
    .unwrap();
    assert_eq!(
        store.migrate_legacy(&legacy).unwrap(),
        MigrationOutcome::AlreadyCurrent
    );
    assert!(legacy.exists());
    assert_eq!(std::fs::read_to_string(store.path()).unwrap(), raw);
}

#[test]
fn an_invalid_legacy_snapshot_is_an_error_and_is_left_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy.json");
    std::fs::write(&legacy, r#"{"version":2,"documents":[]}"#).unwrap();
    let store = RecoveryStore::new(
        RecoverySpec::documents().at_path(dir.path().join("current.json")),
        dir.path(),
    );
    assert!(matches!(
        store.migrate_legacy(&legacy),
        Err(ShellError::Invalid(_))
    ));
    assert!(legacy.exists());
    assert!(!store.path().exists());
}

// ---------------------------------------------------------------------------------------------
// Cache-directory repair
// ---------------------------------------------------------------------------------------------

#[test]
fn cache_dirs_are_created_kept_or_repaired_from_a_stray_file() {
    let dir = tempfile::tempdir().unwrap();
    let created = dir.path().join("created");
    assert_eq!(
        ensure_cache_dir(&created).unwrap(),
        CacheDirOutcome::Created
    );
    assert_eq!(
        ensure_cache_dir(&created).unwrap(),
        CacheDirOutcome::Existing
    );

    let stray = dir.path().join("stray");
    std::fs::write(&stray, "not a dir").unwrap();
    assert_eq!(ensure_cache_dir(&stray).unwrap(), CacheDirOutcome::Repaired);
    assert!(stray.is_dir());
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_is_replaced_by_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("cache");
    std::os::unix::fs::symlink(dir.path().join("gone"), &link).unwrap();
    assert_eq!(ensure_cache_dir(&link).unwrap(), CacheDirOutcome::Repaired);
    assert!(link.is_dir());
    assert!(!std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn http_cache_identifier_must_be_one_path_component() {
    for bad in ["", ".", "..", "a/b", "a\\b"] {
        let err = ensure_http_cache_dir(bad).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{bad:?}");
    }
}

#[test]
fn http_cache_dir_is_created_under_the_user_cache_root() {
    // A managed, non-interactive session (a build host's job) may have no user cache root at all.
    if rightkit_shell::startup_cache::user_cache_root().is_none() {
        eprintln!(
            "SKIP: this session has no user cache directory (no LOCALAPPDATA or HOME cache root)"
        );
        return;
    }
    let id = format!("rightkit-shell-test-{}", std::process::id());
    let outcome = ensure_http_cache_dir(&id).unwrap();
    assert!(matches!(
        outcome,
        CacheDirOutcome::Created | CacheDirOutcome::Existing
    ));
    let root = rightkit_shell::startup_cache::user_cache_root().unwrap();
    assert!(root.join(&id).is_dir());
    let _ = std::fs::remove_dir(root.join(&id));
}

// ---------------------------------------------------------------------------------------------
// Modular install
// ---------------------------------------------------------------------------------------------

#[test]
fn module_set_names_every_module_and_toggles_membership() {
    let all = ModuleSet::all();
    assert_eq!(all.names().len(), Module::ALL.len());
    assert_eq!(all.names()[0], "js_hardening");
    assert_eq!(all.names().last().copied(), Some("clipboard"));

    let state = ModuleSet::state_only();
    assert_eq!(
        state.names(),
        vec![
            "reveal_on_reopen",
            "broker",
            "stream",
            "recovery",
            "frameless",
            "clipboard"
        ]
    );
    assert!(!state.contains(Module::Presence));
    assert!(state.with(Module::Presence).contains(Module::Presence));
    assert!(!all.without(Module::Watchdog).contains(Module::Watchdog));
}

#[test]
fn opting_out_js_hardening_drops_the_injected_script() {
    let full = ShellPlugin::builder()
        .shell(Shell::builder("test-modular").build())
        .build();
    assert!(full.initialization_script_2().is_some());

    let no_js = ShellPlugin::builder()
        .shell(Shell::builder("test-modular").build())
        .without(Module::JsHardening)
        .build();
    assert_eq!(no_js.name(), "rightkit-shell");
    assert!(no_js.initialization_script_2().is_none());

    let state_only = ShellPlugin::builder()
        .shell(Shell::builder("test-modular").build())
        .install_state_only();
    assert_eq!(state_only.name(), "rightkit-shell");
    assert!(state_only.initialization_script_2().is_none());
}

#[test]
#[should_panic(expected = "needs .shell")]
fn a_modular_install_without_a_shell_config_is_a_startup_error() {
    let _ = ShellPlugin::builder().build();
}

// ---------------------------------------------------------------------------------------------
// Idle failure callback
// ---------------------------------------------------------------------------------------------

#[test]
fn idle_suspend_and_resume_failures_reach_the_callback() {
    let seen: Arc<Mutex<Vec<IdleFailure>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let host = IdleHost::new(Duration::from_secs(30)).with_on_error(Some(Arc::new(
        move |f: &IdleFailure| {
            sink.lock().unwrap().push(f.clone());
        },
    )));

    host.report_failure(IdlePhase::Suspend, "main", "TrySuspend failed: 0x80004005");
    host.report_failure(IdlePhase::Resume, "main", "Resume failed");

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            IdleFailure {
                phase: IdlePhase::Suspend,
                window: "main".into(),
                error: "TrySuspend failed: 0x80004005".into(),
            },
            IdleFailure {
                phase: IdlePhase::Resume,
                window: "main".into(),
                error: "Resume failed".into(),
            },
        ]
    );
    assert_eq!(
        serde_json::to_value(&got[0]).unwrap(),
        serde_json::json!({"phase": "suspend", "window": "main", "error": "TrySuspend failed: 0x80004005"})
    );
}

#[test]
fn idle_failures_without_a_callback_are_silent_and_never_panic() {
    let host = IdleHost::new(Duration::from_secs(30));
    host.report_failure(IdlePhase::Suspend, "main", "ignored");
    assert!(host.on_error.is_none());
}

// ---------------------------------------------------------------------------------------------
// The broker as a plain library type
// ---------------------------------------------------------------------------------------------

#[test]
fn the_broker_works_as_a_plain_library_type_without_any_plugin() {
    // No Tauri app, no plugin: an app that owns its own commands can still use grants, stream ids,
    // site roots and recovery authority directly.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.pdf");
    std::fs::write(&file, b"%PDF").unwrap();
    let broker = PathBroker::new();

    let display = broker.grant_file(&file).unwrap();
    assert!(broker.require_file(&display).is_ok());
    let id = broker.grant_stream(&display).unwrap();
    assert!(broker.resolve_stream(&id).is_ok());
    let root = broker.grant_site_root(dir.path()).unwrap();
    assert!(broker.resolve_site_root(&root).is_ok());
    broker.revoke_path(&display).unwrap();
    assert!(broker.resolve_stream(&id).is_err());
}

#[test]
fn an_imported_image_has_a_png_extension_when_the_clipboard_offered_png() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = ClipboardSnapshot {
        image: Some(ClipboardImage {
            extension: "png".into(),
            bytes: vec![0x89, b'P', b'N', b'G'],
        }),
        ..Default::default()
    };
    let item = item_from_snapshot(
        &snapshot,
        &PathBroker::default(),
        &dir.path().join("temp"),
        ClipboardPolicy::default(),
    )
    .unwrap();
    match item {
        ClipboardItem::Image { path } => assert!(path.ends_with(".png")),
        other => panic!("expected an image, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Watchdog
// ---------------------------------------------------------------------------------------------

#[test]
fn watchdog_polls_every_250_milliseconds_by_default_and_the_interval_is_configurable() {
    assert_eq!(DEFAULT_POLL_INTERVAL, Duration::from_millis(250));
    let defaults = WatchdogConfig::default();
    assert_eq!(defaults.poll_interval, Duration::from_millis(250));
    assert_eq!(defaults.threshold, Duration::from_secs(5));
    let fast = WatchdogConfig {
        poll_interval: Duration::from_millis(50),
        ..WatchdogConfig::default()
    };
    assert_eq!(fast.poll_interval, Duration::from_millis(50));
}

#[test]
fn stall_detector_reports_each_episode_once_with_the_stall_duration() {
    let mut detector = StallDetector::new(Duration::from_secs(5));
    assert!(
        detector.observe(1_000, 4_000).is_none(),
        "within the threshold"
    );

    let report = detector
        .observe(1_000, 7_000)
        .expect("the stall is reported");
    assert_eq!(report.stalled_ms, 6_000);
    assert_eq!(report.threshold_ms, 5_000);

    assert!(
        detector.observe(1_000, 9_000).is_none(),
        "one report per episode"
    );
    assert!(
        detector.observe(8_500, 9_000).is_none(),
        "a fresh beat re-arms the detector"
    );

    let next = detector
        .observe(8_500, 20_000)
        .expect("a new episode is reported");
    assert_eq!(next.stalled_ms, 11_500);
}
