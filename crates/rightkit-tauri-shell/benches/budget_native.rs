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

#[cfg(feature = "host-services")]
#[derive(Default)]
struct ContentWitness {
    ready: AtomicBool,
    result: Mutex<Option<bool>>,
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

// Fixture-only store: no credential store or user secret is touched.
#[cfg(feature = "host-services")]
#[derive(Clone, Default)]
struct FixtureSecrets(Arc<Mutex<FixtureSecretsState>>);

#[cfg(feature = "host-services")]
#[derive(Default)]
struct FixtureSecretsState {
    value: Option<String>,
    calls: usize,
    writes: Vec<String>,
    failure: Option<rightkit_tauri_shell::host::SecretStoreError>,
}

#[cfg(feature = "host-services")]
impl rightkit_tauri_shell::host::SecretStore for FixtureSecrets {
    fn exists(
        &self,
        _: &rightkit_app_settings::SecretRef,
    ) -> Result<bool, rightkit_tauri_shell::host::SecretStoreError> {
        let mut state = self.0.lock().expect("fixture secrets");
        state.calls += 1;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        Ok(state.value.is_some())
    }

    fn delete(
        &self,
        _: &rightkit_app_settings::SecretRef,
    ) -> Result<(), rightkit_tauri_shell::host::SecretStoreError> {
        let mut state = self.0.lock().expect("fixture secrets");
        state.calls += 1;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        state.value = None;
        Ok(())
    }
}

#[cfg(feature = "host-services")]
impl rightkit_tauri_shell::host::SecretWriter for FixtureSecrets {
    fn store(
        &self,
        _: &rightkit_app_settings::SecretRef,
        value: &rightkit_tauri_shell::host::SecretValue,
        overwrite: bool,
    ) -> Result<(), rightkit_tauri_shell::host::SecretStoreError> {
        use rightkit_tauri_shell::host::SecretStoreError;
        let mut state = self.0.lock().expect("fixture secrets");
        state.calls += 1;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if !overwrite && state.value.is_some() {
            return Err(SecretStoreError::AlreadyExists);
        }
        state.value = Some(value.expose().to_owned());
        state.writes.push(value.expose().to_owned());
        Ok(())
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
    let secrets = FixtureSecrets::default();
    app.manage(secrets.clone());
    let mut services = HostServices::builder()
        .secrets(
            rightkit_tauri_shell::host::SecretsService::new(
                secrets.clone(),
                [rightkit_tauri_shell::host::ApiKeySlot::new(
                    rightkit_app_settings::SecretRef::new("native-budget", "fixture"),
                )
                .max_bytes(32)],
            )
            .with_writer(secrets),
            ServiceTrust::for_labels(["main", "content"]),
        )
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

// Only this native fixture constructs ACL data at runtime; production manifests stay unchanged.
#[cfg(feature = "host-services")]
fn fixture_acl(context: &mut tauri::Context<Wry>) -> String {
    use std::collections::BTreeMap;
    use tauri_utils::acl::{
        capability::Capability, manifest::Manifest, resolved::Resolved, APP_ACL_KEY,
    };
    let commands = [
        "native_fixture_report",
        "rightkit_settings_describe",
        "rightkit_settings_get",
        "rightkit_settings_patch",
        "rightkit_settings_reset",
        "rightkit_settings_subscribe",
        "rightkit_settings_unsubscribe",
        "rightkit_secrets_presence",
        "rightkit_secrets_clear",
        "rightkit_secrets_set",
    ];
    let manifest: Manifest = serde_json::from_value(serde_json::json!({
        "permissions": {"fixture-host": {"identifier": "fixture-host", "commands": {"allow": commands}}},
        "permission_sets": {}
    })).expect("fixture application manifest");
    // Exact webview grants: registered-content has neither a window nor a webview grant.
    let capability = serde_json::json!({
        "identifier": "fixture-host", "webviews": ["main", "content"],
        "permissions": ["fixture-host"]
    })
    .to_string();
    let capabilities: BTreeMap<String, Capability> = BTreeMap::from([(
        "fixture-host".into(),
        serde_json::from_str(&capability).expect("fixture capability"),
    )]);
    let acl = BTreeMap::from([(APP_ACL_KEY.into(), manifest)]);
    let resolved = Resolved::resolve(&acl, capabilities, tauri_utils::platform::Target::current())
        .expect("resolve real fixture ACL");
    assert!(resolved.has_app_acl, "fixture must enforce application ACL");
    let authority = tauri::ipc::RuntimeAuthority::new(acl, resolved);
    for command in commands {
        assert!(authority
            .resolve_access(command, "main", "main", &tauri::ipc::Origin::Local)
            .is_some());
        assert!(authority
            .resolve_access(
                command,
                "registered-content",
                "registered-content",
                &tauri::ipc::Origin::Local
            )
            .is_none());
    }
    *context.runtime_authority_mut() = authority;
    capability
}

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
        .watchdog((mode != "bare").then_some(config));
    #[cfg(feature = "host-services")]
    let (shell, fixture_capability) = {
        use rightkit_tauri_shell::{ContentWebview, DevToolsPolicy};
        assert!(
            cfg!(feature = "content-devtools"),
            "registered content fixture requires content-devtools"
        );
        let mut context = tauri::test::mock_context(NativeAssets);
        let capability = fixture_acl(&mut context);
        let registration =
            ContentWebview::new("registered-content", "registered-content").capability(&capability);
        assert!(matches!(
            rightkit_tauri_shell::devtools::build_gate(
                DevToolsPolicy::ContentOnly,
                "main",
                &[registration.clone()],
                true
            ),
            Err(rightkit_tauri_shell::DevToolsError::AppAclManifestMissing { .. })
        ));
        (
            shell
                .devtools_policy(DevToolsPolicy::ContentOnly)
                .content_webview(registration.app_acl_manifest(true)),
            capability,
        )
    };
    let shell = shell.build();
    let fixture_root = root.path().to_path_buf();
    let page_ready = Arc::clone(&ready);
    #[cfg(feature = "host-services")]
    let content_page_ready = Arc::clone(&content_ready);
    let mut context = tauri::test::mock_context(NativeAssets);
    context.config_mut().identifier = format!("app.rightkit.nativebudget.p{}", std::process::id());
    context.config_mut().app.windows.clear();
    #[cfg(feature = "host-services")]
    assert_eq!(fixture_acl(&mut context), fixture_capability);
    let builder = tauri::Builder::<Wry>::new()
        .manage(RendererReports::default())
        .invoke_handler(rightkit_tauri_shell::handler![native_fixture_report])
        .plugin(shell.plugin());
    #[cfg(feature = "host-services")]
    let builder = builder.manage(ContentWitness::default());
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
            #[cfg(feature = "host-services")]
            {
                let witness = app.handle().clone();
                let loaded = app.handle().clone();
                tauri::WebviewWindowBuilder::new(
                    app,
                    "registered-content",
                    tauri::WebviewUrl::App("index.html".into()),
                )
                .visible(false)
                .focused(false)
                .data_directory(fixture_root.join("registered-content-webview"))
                .on_navigation(move |url| {
                    if url.host_str() == Some("native-fixture.invalid") {
                        *witness
                            .state::<ContentWitness>()
                            .result
                            .lock()
                            .expect("content witness") = Some(url.path() == "/denied");
                        return false;
                    }
                    true
                })
                .on_page_load(move |_, event| {
                    if event.event() == tauri::webview::PageLoadEvent::Finished {
                        loaded
                            .state::<ContentWitness>()
                            .ready
                            .store(true, Ordering::Release);
                    }
                })
                .build()?;
            }
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
            secrets_probe(&observer_handle);
            settings_probe(&observer_handle);
            registered_content_probe(&observer_handle, &observer_root);
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
fn secrets_renderer_probe(app: &tauri::AppHandle<Wry>, label: &str, body: &str) {
    app.state::<RendererReports>()
        .0
        .lock()
        .expect("renderer reports")
        .remove(label);
    let script = format!(
        r#"(async () => {{
            const invoke = window.__TAURI_INTERNALS__.invoke;
            const secret = {{service: 'native-budget', account: 'fixture'}};
            const check = (ok, message) => {{ if (!ok) throw new Error(message); }};
            const reject = async (command, args, code, message) => {{
                let error;
                try {{ await invoke(command, args); }} catch (e) {{ error = e; }}
                check(error && error.code === code, 'typed ' + code);
                if (message) check(error.message === message, 'fixed error message');
                check(Object.keys(error).sort().join(',') === 'code,message', 'fixed error envelope');
                const wire = JSON.stringify(error);
                check(!wire.includes('CANARY') && !wire.includes('backend-private'), 'error isolation');
            }};
            try {{
                {body}
                await invoke('native_fixture_report', {{report: {{ok: true}}}});
            }} catch (error) {{
                await invoke('native_fixture_report', {{report: {{ok: false, error: String(error)}}}});
            }}
        }})();"#
    );
    app.get_webview_window(label)
        .expect("secrets fixture window")
        .eval(&script)
        .expect("dispatch secrets renderer IPC");
    wait_for("secrets renderer IPC", || {
        app.state::<RendererReports>()
            .0
            .lock()
            .expect("renderer reports")
            .contains_key(label)
    });
    let report = app
        .state::<RendererReports>()
        .0
        .lock()
        .expect("renderer reports")
        .remove(label)
        .expect("secrets renderer report");
    assert_eq!(report["ok"], true, "registered secrets IPC: {report}");
}

#[cfg(feature = "host-services")]
fn secrets_probe(app: &tauri::AppHandle<Wry>) {
    use rightkit_tauri_shell::host::SecretStoreError;
    let fixture = app.state::<FixtureSecrets>();
    secrets_renderer_probe(
        app,
        "main",
        r#"
        const presence = async expected => {
            const result = await invoke('rightkit_secrets_presence', {refs: [secret]});
            check(JSON.stringify(result) === JSON.stringify([expected]), 'presence enum only');
        };
        await presence('absent');
        check(await invoke('rightkit_secrets_set', {
            secret, value: 'CANARY-first', intent: 'create'
        }) === null, 'create null');
        await presence('present');
        await reject('rightkit_secrets_set', {
            secret, value: 'CANARY-duplicate', intent: 'create'
        }, 'secret_exists', 'a key is already stored; replace it explicitly');
        await presence('present');
        check(await invoke('rightkit_secrets_set', {
            secret, value: 'CANARY-replacement', intent: 'replace'
        }) === null, 'replace null');
        check(await invoke('rightkit_secrets_clear', {secret}) === null, 'clear null');
        await presence('absent');
        check(await invoke('rightkit_secrets_clear', {secret}) === null, 'idempotent clear null');
    "#,
    );
    {
        let state = fixture.0.lock().expect("fixture secrets");
        assert_eq!(state.writes, ["CANARY-first", "CANARY-replacement"]);
        assert!(state.value.is_none());
    }
    let before = fixture.0.lock().expect("fixture secrets").calls;
    secrets_renderer_probe(
        app,
        "main",
        r#"
        const other = {service: 'native-budget', account: 'undeclared'};
        for (const [command, args] of [
            ['rightkit_secrets_presence', {refs: [secret, other]}],
            ['rightkit_secrets_clear', {secret: other}],
            ['rightkit_secrets_set', {secret: other, value: 'CANARY-other', intent: 'replace'}]
        ]) await reject(command, args, 'slot_not_declared',
            'that credential slot is not declared by this app');
        for (const [value, message] of [
            ['', 'the key is empty'],
            ['x'.repeat(33), 'the key is too long'],
            ['line\nbreak', 'the key holds a control character'],
            ['tab\tbed', 'the key holds a control character'],
            ['nul\0byte', 'the key holds a control character']
        ]) await reject('rightkit_secrets_set', {secret, value, intent: 'replace'},
            'invalid_secret', message);
    "#,
    );
    assert_eq!(fixture.0.lock().expect("fixture secrets").calls, before);
    // This caller has secrets trust but lacks settings trust: set must stop before the writer.
    secrets_renderer_probe(
        app,
        "content",
        r#"
        check(JSON.stringify(await invoke('rightkit_secrets_presence', {refs: [secret]}))
            === '["absent"]', 'secrets-only caller presence');
        await reject('rightkit_secrets_set', {
            secret, value: 'CANARY-untrusted', intent: 'replace'
        }, 'permission_denied');
    "#,
    );
    assert_eq!(fixture.0.lock().expect("fixture secrets").calls, before + 1);
    for (failure, code, message) in [
        (
            SecretStoreError::Failed("backend-private CANARY-failed".into()),
            "secret_store_failed",
            "the credential store refused the operation",
        ),
        (
            SecretStoreError::Unavailable("backend-private CANARY-unavailable".into()),
            "secret_store_unavailable",
            "the credential store is not available",
        ),
    ] {
        fixture.0.lock().expect("fixture secrets").failure = Some(failure);
        secrets_renderer_probe(
            app,
            "main",
            &format!(
                r#"
            await reject('rightkit_secrets_set', {{
                secret, value: 'CANARY-failure', intent: 'replace'
            }}, '{code}', '{message}');
            await reject('rightkit_secrets_clear', {{secret}}, '{code}', '{message}');
            check(JSON.stringify(await invoke('rightkit_secrets_presence', {{refs: [secret]}}))
                === '["unknown"]', 'unavailable presence enum only');
        "#
            ),
        );
    }
    fixture.0.lock().expect("fixture secrets").failure = None;
    assert_eq!(
        fixture.0.lock().expect("fixture secrets").writes,
        ["CANARY-first", "CANARY-replacement"]
    );
    println!("FUNCTIONAL mode=host-registration secrets_ipc=true declared_slots=true write_only=true create_replace_clear=true invalid_values_refused=true backend_errors_redacted=true settings_trust_required=true");
}

#[cfg(not(feature = "host-services"))]
fn secrets_probe(_: &tauri::AppHandle<Wry>) {}

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

#[cfg(feature = "host-services")]
fn registered_content_probe(app: &tauri::AppHandle<Wry>, root: &Path) {
    let fixture = app.state::<FixtureSecrets>();
    let before = {
        let state = fixture.0.lock().expect("fixture secrets");
        (state.calls, state.value.clone(), state.writes.clone())
    };
    let settings_before = fs::read(root.join("settings.json")).expect("committed fixture settings");
    let content = app
        .get_webview_window("registered-content")
        .expect("registered content webview");
    wait_for("registered content page load", || {
        app.state::<ContentWitness>().ready.load(Ordering::Acquire)
    });
    let native_webview = app
        .get_webview("registered-content")
        .expect("native content identity");
    assert!(
        matches!(
            rightkit_tauri_shell::caller_gate::require_app_caller(&native_webview),
            Err(rightkit_tauri_shell::caller_gate::CallerError::Forbidden(
                rightkit_tauri_shell::caller_gate::Denial::ContentWebview
            ))
        ),
        "native caller must resolve to registered content"
    );
    content.eval(r#"
        (async () => {
            const invoke = window.__TAURI_INTERNALS__.invoke;
            const secret = {service: 'native-budget', account: 'fixture'};
            let denied = true;
            for (const [command, args] of [
                ['rightkit_settings_get', {}],
                ['rightkit_settings_patch', {patch: {probe: 'CANARY-content'}, expectedRevision: 0}],
                ['rightkit_secrets_presence', {refs: [secret]}],
                ['rightkit_secrets_clear', {secret}],
                ['rightkit_secrets_set', {secret, value: 'CANARY-content', intent: 'replace'}],
                ['native_fixture_report', {report: {ok: true}}]
            ]) {
                let refused = false;
                try { await invoke(command, args); } catch (error) {
                    refused = typeof error === 'string' && error.includes('not allowed');
                }
                denied = denied && refused;
            }
            location.href = 'https://native-fixture.invalid/' + (denied ? 'denied' : 'failed');
        })();
    "#).expect("dispatch registered content IPC");
    wait_for("registered content native navigation witness", || {
        app.state::<ContentWitness>()
            .result
            .lock()
            .expect("content witness")
            .is_some()
    });
    assert_eq!(
        *app.state::<ContentWitness>()
            .result
            .lock()
            .expect("content witness"),
        Some(true),
        "registered content must receive ACL refusal for every command"
    );
    let state = fixture.0.lock().expect("fixture secrets");
    assert_eq!(
        (state.calls, state.value.clone(), state.writes.clone()),
        before
    );
    assert_eq!(
        fs::read(root.join("settings.json")).expect("fixture settings"),
        settings_before
    );
    content
        .destroy()
        .expect("destroy registered content before idle measurement");
    println!("FUNCTIONAL mode=host-registration registered_content_acl_denied=true settings_unchanged=true secrets_backend_unchanged=true report_command_denied=true");
}

#[cfg(not(feature = "host-services"))]
fn registered_content_probe(_: &tauri::AppHandle<Wry>, _: &Path) {}

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
