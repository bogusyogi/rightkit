//! Native Shell/HostServices adapter for the existing watchdog budget policy.
//! One real hidden Wry webview during measurement; no mock runtime, QA server or user store.
//! Three interleaved bare/watchdog pairs, default watchdog timing, 5 s settling & >=30 s idle.
//! Capture/cancellation probes run in separate unmeasured native processes.

use rightkit_child_process::{observe_idle, process_usage, BudgetLabels, OwnedCommand};
use rightkit_tauri_shell::{watchdog, Shell, WatchdogConfig};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Manager, Wry};

const IDLE_SECONDS: u64 = 30;
const PAIRS: usize = 3;

// Fixture-only completion channel: results originate in the invoking native webview.
#[derive(Default)]
struct RendererReports(Mutex<std::collections::BTreeMap<String, serde_json::Value>>);

#[tauri::command]
fn native_fixture_report(webview: tauri::Webview<Wry>, report: serde_json::Value) {
    webview
        .app_handle()
        .state::<RendererReports>()
        .0
        .lock()
        .expect("renderer reports")
        .insert(webview.label().to_owned(), report);
}

struct NativeAssets;

impl tauri::Assets<Wry> for NativeAssets {
    fn get(&self, _: &tauri::utils::assets::AssetKey) -> Option<std::borrow::Cow<'_, [u8]>> {
        Some(std::borrow::Cow::Borrowed(
            b"<!doctype html><html><head><title>Native budget</title></head><body></body></html>",
        ))
    }

    fn iter(&self) -> Box<tauri::utils::assets::AssetsIter<'_>> {
        Box::new(std::iter::empty())
    }

    fn csp_hashes(
        &self,
        _: &tauri::utils::assets::AssetKey,
    ) -> Box<dyn Iterator<Item = tauri::utils::assets::CspHash<'_>> + '_> {
        Box::new(std::iter::empty())
    }
}

fn main() {
    assert!(
        cfg!(any(windows, target_os = "macos")),
        "native Windows/macOS desktop required"
    );
    let args: Vec<_> = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--bench")
        .collect();
    if args.first().is_some_and(|arg| arg == "--native-child") {
        let mode = args.get(1).expect("native child mode");
        let seconds: u64 = args
            .get(2)
            .expect("idle seconds")
            .parse()
            .expect("integer seconds");
        assert!(seconds >= IDLE_SECONDS, "at least 30 idle seconds required");
        assert!(["bare", "watchdog", "capture", "cancel"].contains(&mode.as_str()));
        native(mode.clone(), seconds);
        return;
    }
    assert!(
        args.is_empty(),
        "workload has fixed default timings & 30 second idle window"
    );
    #[cfg(feature = "crash")]
    for mode in ["capture", "cancel"] {
        let output = child(mode);
        assert!(output.contains(&format!("FUNCTIONAL mode={mode} complete=true")));
        print!("{output}");
    }
    let mut pairs = Vec::new();
    for index in 1..=PAIRS {
        let bare = child("bare");
        let bare_rate = bare
            .lines()
            .find_map(|line| line.strip_prefix("BASELINE wakeups_per_s="))
            .expect("native baseline counter")
            .parse::<f64>()
            .expect("available baseline wakeups");
        let output = child("watchdog");
        let line = output
            .lines()
            .find(|line| line.starts_with("RESULT budget "))
            .expect("native result");
        let rate = line
            .split_whitespace()
            .find_map(|word| word.strip_prefix("wakeups_per_s="))
            .expect("native wakeups")
            .parse::<f64>()
            .expect("available native wakeups");
        assert!(bare_rate.is_finite() && rate.is_finite());
        let delta = round3(rate - bare_rate);
        println!("PAIR {index}/{PAIRS} bare_wakeups_per_s={bare_rate:.3} watchdog_wakeups_per_s={rate:.3} delta_per_s={delta:.3}");
        pairs.push((delta, bare_rate, output));
    }
    pairs.sort_by(|left, right| left.0.total_cmp(&right.0));
    let (delta, bare, output) = pairs.swap_remove(PAIRS / 2);
    for line in output.lines() {
        if line.starts_with("RESULT budget ") {
            println!("{line} bare_wakeups_per_s={bare:.3} wakeups_delta_per_s={delta:.3}");
        } else {
            println!("{line}");
        }
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn child(mode: &str) -> String {
    let dir = tempfile::tempdir().expect("isolated child output");
    let stdout = dir.path().join("stdout.txt");
    let stderr = dir.path().join("stderr.txt");
    let mut command = OwnedCommand::new(std::env::current_exe().expect("native bench executable"));
    command
        .command_mut()
        .args(["--native-child", mode, &IDLE_SECONDS.to_string()])
        .env("RK_NATIVE_BUDGET_ROOT", dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            fs::File::create(&stdout).expect("child stdout"),
        ))
        .stderr(Stdio::from(
            fs::File::create(&stderr).expect("child stderr"),
        ));
    command.windows_hide();
    let mut child = command.spawn().expect("contained native child");
    let status = child
        .wait_timeout(Duration::from_secs(IDLE_SECONDS + 90))
        .expect("native child status")
        .expect("native child deadline; owned drop terminates process tree");
    assert!(
        status.success(),
        "native {mode} failed: {}",
        fs::read_to_string(&stderr).unwrap_or_default()
    );
    fs::read_to_string(stdout).expect("native child output")
}

fn wait_for(name: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "native fixture timed out: {name}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(feature = "host-services")]
fn host_services(app: &tauri::AppHandle<Wry>, root: &Path) {
    use rightkit_tauri_shell::host::{
        HostServices, ServiceTrust, SettingsBridge, SettingsHostService, StaticSchema,
    };
    let document =
        rightkit_app_settings::Document::open_path(root.join("settings.json"), "native-budget")
            .expect("isolated settings document");
    let bridge = SettingsBridge::new(document, StaticSchema::new(1).keys(["probe"]));
    let mut services = HostServices::builder()
        .settings(
            SettingsHostService::new(bridge),
            ServiceTrust::for_labels(["main"]),
        )
        .guard_shell_commands(ServiceTrust::for_labels(["main"]));
    #[cfg(feature = "logs")]
    {
        services = services.logs(ServiceTrust::for_labels(["main"]));
    }
    services
        .build()
        .expect("concrete host policy")
        .install(app)
        .expect("install concrete host services");
}

#[cfg(not(feature = "host-services"))]
fn host_services(_: &tauri::AppHandle<Wry>, _: &Path) {}

#[cfg(feature = "logs")]
fn logs(app: &tauri::AppHandle<Wry>, root: &Path) {
    use rightkit_tauri_shell::logs_commands::{self, LogsConfig, LogsRuntime};
    logs_commands::install(
        app,
        LogsConfig::new(
            "native-budget",
            env!("CARGO_PKG_VERSION"),
            root.join("renderer.jsonl"),
        ),
    )
    .expect("install concrete logs runtime");
    let runtime = app.state::<LogsRuntime>();
    let report = runtime.write(vec![serde_json::json!({
        "schema_version": 1, "ts_ms": 0, "event": "native-budget-ready", "level": "info"
    })]);
    assert_eq!(
        report.queued, 1,
        "native log queue must accept real warm-up event"
    );
    assert!(
        runtime.flush(Duration::from_secs(5)),
        "native log queue must flush"
    );
}

#[cfg(not(feature = "logs"))]
fn logs(_: &tauri::AppHandle<Wry>, _: &Path) {}

fn native(mode: String, idle_seconds: u64) {
    // Native desktop/webview creation itself is the prerequisite check. Any error fails closed.
    // Parent owns enclosing directory, so forced process termination still removes fixture data.
    let parent_root = std::env::var_os("RK_NATIVE_BUDGET_ROOT").expect("parent-owned fixture root");
    let root = tempfile::tempdir_in(parent_root).expect("isolated native namespace");
    let namespace = format!("rk-native-budget-{}", std::process::id());
    let ready = Arc::new(AtomicBool::new(false));
    let capture_started = Arc::new(AtomicBool::new(false));
    #[cfg(feature = "host-services")]
    let content_ready = Arc::new(AtomicBool::new(false));
    let mut config = WatchdogConfig::default();
    #[cfg(feature = "crash")]
    {
        use watchdog::{CaptureConfig, CrashCaptureOptions};
        let real = watchdog::crash_capture(
            rightkit_crash::CrashConfig::new(
                &namespace,
                env!("CARGO_PKG_VERSION"),
                root.path().join("crash"),
            ),
            "native-budget-stall",
            CrashCaptureOptions::default(),
        );
        let started = Arc::clone(&capture_started);
        let cancel = mode == "cancel";
        config = config.with_capture(CaptureConfig::new(move |request| {
            started.store(true, Ordering::Release);
            if cancel {
                // Functional cancellation seam only: defer the real adapter until recovery.
                let deadline = Instant::now() + Duration::from_secs(15);
                while !request.is_recovered() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if !request.is_recovered() {
                    return watchdog::CaptureOutcome::Failed("native cancellation deadline".into());
                }
            }
            (real.run)(request)
        }));
    }
    #[cfg(not(feature = "crash"))]
    assert!(
        mode != "capture" && mode != "cancel",
        "crash adapter required for capture probes"
    );

    let shell = Shell::builder(&namespace)
        .app_name(&namespace)
        .main_label("main")
        .never_show(true)
        .single_instance(false)
        .state_dir(root.path().join("shell-state"))
        .cache_dir(root.path().join("shell-cache"))
        .autostart_options(rightkit_tauri_shell::autostart::AutostartOptions {
            config_home: Some(root.path().join("home")),
            ..Default::default()
        })
        .webview_idle(rightkit_tauri_shell::idle::DEFAULT_SUSPEND_AFTER)
        .watchdog((mode != "bare").then_some(config))
        .build();
    let fixture_root = root.path().to_path_buf();
    let page_ready = Arc::clone(&ready);
    #[cfg(feature = "host-services")]
    let content_page_ready = Arc::clone(&content_ready);
    let mut context = tauri::test::mock_context(NativeAssets);
    context.config_mut().identifier = format!("app.rightkit.nativebudget.p{}", std::process::id());
    context.config_mut().app.windows.clear();
    let builder = tauri::Builder::<Wry>::new()
        .manage(RendererReports::default())
        .invoke_handler(rightkit_tauri_shell::handler![native_fixture_report])
        .plugin(shell.plugin());
    let mut app = builder
        .setup(move |app| {
            host_services(app.handle(), &fixture_root);
            logs(app.handle(), &fixture_root);
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .visible(false)
            .focused(false)
            .data_directory(fixture_root.join("webview"))
            .on_page_load(move |_, event| {
                if event.event() == tauri::webview::PageLoadEvent::Finished {
                    page_ready.store(true, Ordering::Release);
                }
            })
            .build()?;
            #[cfg(feature = "host-services")]
            tauri::WebviewWindowBuilder::new(
                app,
                "content",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .visible(false)
            .focused(false)
            .data_directory(fixture_root.join("content-webview"))
            .on_page_load(move |_, event| {
                if event.event() == tauri::webview::PageLoadEvent::Finished {
                    content_page_ready.store(true, Ordering::Release);
                }
            })
            .build()?;
            Ok(())
        })
        .build(context)
        .expect("native Wry desktop & webview prerequisites");
    // Tauri 2.11 configures activation on App, not Builder. Set it before run(): Tao applies
    // this policy at application launch before activation, keeping this hidden probe background-only.
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Prohibited);
    let handle = app.handle().clone();
    let observer_handle = handle.clone();
    let crash_dir = root.path().join("crash");
    let observer_root = root.path().to_path_buf();
    let observer = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            wait_for("real webview page load", || ready.load(Ordering::Acquire));
            #[cfg(feature = "host-services")]
            wait_for("content webview page load", || {
                content_ready.load(Ordering::Acquire)
            });
            settings_probe(&observer_handle);
            main_probe(&observer_handle, &observer_root);
            if mode == "capture" || mode == "cancel" {
                capture_probe(&observer_handle, &mode, &capture_started, &crash_dir);
            } else {
                // Keep the existing bench's explicit 5-second settling period.
                std::thread::sleep(Duration::from_secs(5));
                let warm = process_usage().expect("native process sample");
                let start = watchdog::status().watchdog;
                assert_eq!(start.installed, mode == "watchdog");
                let idle =
                    observe_idle(Duration::from_secs(idle_seconds)).expect("native idle sample");
                let end = watchdog::status().watchdog;
                if mode == "watchdog" {
                    assert!(
                        end.installed && end.last_beat_age_ms < 5000,
                        "default heartbeat must remain live"
                    );
                    assert_eq!(end.stalls_total, start.stalls_total, "idle UI stalled");
                    assert_eq!(end.capture_failures, 0);
                    print!("{}", idle.result_lines(&warm, &BudgetLabels {
                        krate: "rightkit-tauri-shell", version: env!("CARGO_PKG_VERSION"),
                        scale_unit: "watchdogs", scale: 1,
                        what: "default watchdog on real hidden Wry Shell; activated services named by workload feature matrix",
                        scope: "synthetic",
                    }));
                } else {
                    let rate = idle
                        .rates
                        .wakeups_per_second
                        .expect("native baseline wake-up counter");
                    println!("BASELINE wakeups_per_s={rate:.3}");
                }
                let idle_state = rightkit_tauri_shell::idle::status(&observer_handle);
                assert!(
                    idle_state.enabled && idle_state.hidden && idle_state.suspended,
                    "default 30-second hidden-webview timer must fire: {idle_state:?}"
                );
                main_probe(&observer_handle, &observer_root);
            }
            if let Some(report) = watchdog::shutdown() {
                assert!(
                    report.stopped && report.threads_left == 0 && !report.capture_in_flight,
                    "watchdog shutdown must complete within its production bound: {report:?}"
                );
            }
            std::io::stdout().flush().expect("flush native evidence");
        }));
        observer_handle.exit(if result.is_ok() { 0 } else { 1 });
        result.is_ok()
    });
    let exit_code = app.run_return(|_, _| {});
    assert!(
        observer.join().expect("native observer thread"),
        "native functional/runtime failure"
    );
    // Root remains alive until app/plugin shutdown, then every fixture path is removed.
    drop(root);
    assert_eq!(exit_code, 0, "native event-loop exit status");
}

#[cfg(feature = "host-services")]
fn settings_probe(app: &tauri::AppHandle<Wry>) {
    use rightkit_tauri_shell::host::settings;
    use tauri::Listener;
    let window = app
        .get_webview_window("main")
        .expect("settings controller window");
    let (tx, rx) = mpsc::sync_channel(4);
    let listener = window.listen(settings::EVENT_SETTINGS_CHANGED, move |event| {
        let _ = tx.try_send(event.payload().to_owned());
    });
    // Every settings operation travels from Wry's renderer through handler! registration.
    window
        .eval(
            r#"
        (async () => {
            const invoke = window.__TAURI_INTERNALS__.invoke;
            const check = (ok, message) => { if (!ok) throw new Error(message); };
            try {
                const description = await invoke('rightkit_settings_describe');
                const initial = await invoke('rightkit_settings_get');
                check(description.writable && description.revision === initial.revision,
                    'registered settings description');
                const revision = await invoke('rightkit_settings_subscribe');
                check(revision === initial.revision, 'subscription revision');
                const saved = await invoke('rightkit_settings_patch', {
                    patch: {probe: true}, expectedRevision: revision
                });
                check(saved.values.probe === true && saved.revision > revision, 'CAS commit');
                let conflict;
                try {
                    await invoke('rightkit_settings_patch', {
                        patch: {probe: false}, expectedRevision: revision
                    });
                } catch (error) { conflict = error; }
                check(conflict && conflict.code === 'revision_conflict', 'typed stale conflict');
                const current = await invoke('rightkit_settings_get');
                check(current.revision === saved.revision && current.values.probe === true,
                    'stale write changed committed settings');
                const reset = await invoke('rightkit_settings_reset', {
                    keys: ['probe'], expectedRevision: saved.revision
                });
                check(!Object.hasOwn(reset.values, 'probe') && reset.revision > saved.revision,
                    'reset commit');
                await invoke('rightkit_settings_unsubscribe');
                await invoke('native_fixture_report', {report: {
                    ok: true, savedRevision: saved.revision, resetRevision: reset.revision
                }});
            } catch (error) {
                await invoke('native_fixture_report', {report: {ok: false, error: String(error)}});
            }
        })();
    "#,
        )
        .expect("dispatch trusted renderer IPC");
    wait_for("trusted renderer settings IPC", || {
        app.state::<RendererReports>()
            .0
            .lock()
            .expect("renderer reports")
            .contains_key("main")
    });
    let report = app
        .state::<RendererReports>()
        .0
        .lock()
        .expect("renderer reports")
        .get("main")
        .cloned()
        .expect("trusted renderer report");
    assert_eq!(report["ok"], true, "registered settings IPC: {report}");
    // Delivery follows each committed mutation; stale CAS must emit nothing.
    for revision in [&report["savedRevision"], &report["resetRevision"]] {
        let payload: serde_json::Value = serde_json::from_str(
            &rx.recv_timeout(Duration::from_secs(15))
                .expect("postcommit settings event"),
        )
        .expect("settings event JSON");
        assert_eq!(
            payload,
            serde_json::json!({"keys": ["probe"], "revision": revision})
        );
    }
    assert!(
        rx.try_recv().is_err(),
        "stale CAS emitted a settings change"
    );
    window.unlisten(listener);
    let content = app
        .get_webview_window("content")
        .expect("untrusted content window");
    content.eval(r#"
        (async () => {
            const invoke = window.__TAURI_INTERNALS__.invoke;
            let error;
            try { await invoke('rightkit_settings_get'); } catch (e) { error = e; }
            await invoke('native_fixture_report', {report: {
                ok: !!error && error.code === 'permission_denied' &&
                    error.message === 'forbidden: the caller\'s window is not trusted by the service', error
            }});
        })();
    "#).expect("dispatch content renderer IPC");
    wait_for("content renderer refusal", || {
        app.state::<RendererReports>()
            .0
            .lock()
            .expect("renderer reports")
            .contains_key("content")
    });
    let report = app
        .state::<RendererReports>()
        .0
        .lock()
        .expect("renderer reports")
        .get("content")
        .cloned()
        .expect("content renderer report");
    assert_eq!(
        report["ok"], true,
        "content caller must be refused: {report}"
    );
    content
        .destroy()
        .expect("destroy content fixture before idle measurement");
    println!("FUNCTIONAL mode=host-registration settings_ipc=true cas=true stale_conflict=true postcommit_events=true reset=true untrusted_window_refused=true");
}

#[cfg(not(feature = "host-services"))]
fn settings_probe(_: &tauri::AppHandle<Wry>) {}

fn main_probe(app: &tauri::AppHandle<Wry>, root: &Path) {
    let (tx, rx) = mpsc::sync_channel(1);
    let handle = app.clone();
    let profile = root.join("probe-webview");
    app.run_on_main_thread(move || {
        let window = handle
            .get_webview_window("main")
            .expect("native main webview");
        assert!(
            !window.is_visible().expect("native visibility"),
            "fixture must remain hidden"
        );
        // Lifecycle of a second actual native webview, kept hidden & destroyed on its owner thread.
        let secondary = tauri::WebviewWindowBuilder::new(
            &handle,
            "probe",
            tauri::WebviewUrl::App("index.html".into()),
        )
        .visible(false)
        .focused(false)
        .data_directory(profile)
        .build()
        .expect("native secondary webview");
        assert!(!secondary.is_visible().expect("secondary visibility"));
        secondary
            .destroy()
            .expect("destroy native secondary webview");
        rightkit_tauri_shell::idle::note_visible(&handle);
        rightkit_tauri_shell::idle::note_hidden(&handle, false);
        tx.send(()).expect("native probe receiver");
    })
    .expect("dispatch native lifecycle probe");
    rx.recv_timeout(Duration::from_secs(15))
        .expect("native lifecycle probe deadline");
}

fn capture_probe(app: &tauri::AppHandle<Wry>, mode: &str, started: &AtomicBool, crash_dir: &Path) {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        entered_tx.send(()).expect("stall probe observer");
        release_rx
            .recv_timeout(Duration::from_secs(15))
            .expect("bounded native stall release");
    })
    .expect("dispatch bounded native stall");
    entered_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("native stall entered");
    wait_for("real capture callback", || started.load(Ordering::Acquire));
    if mode == "capture" {
        wait_for("real crash report before UI recovery", || {
            crash_dir.exists()
                && fs::read_dir(crash_dir)
                    .unwrap()
                    .filter_map(Result::ok)
                    .any(|entry| {
                        entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "json")
                    })
        });
    }
    release_tx.send(()).expect("release real native UI");
    wait_for("native watchdog recovery", || {
        watchdog::status().watchdog.recoveries >= 1
    });
    if mode == "cancel" {
        wait_for("capture cancellation", || {
            watchdog::status().watchdog.captures_skipped >= 1
        });
        assert!(!crash_dir.exists() || fs::read_dir(crash_dir).unwrap().next().is_none());
    } else {
        wait_for("real crash report", || {
            crash_dir.exists() && fs::read_dir(crash_dir).unwrap().next().is_some()
        });
    }
    assert_eq!(watchdog::status().watchdog.capture_failures, 0);
    println!("FUNCTIONAL mode={mode} complete=true");
}
