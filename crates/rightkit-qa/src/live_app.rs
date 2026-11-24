//! A journey session against the owner's real installed app: the harness machinery an app's QA crate
//! otherwise copies (snapshot arming, run lock, binary identity, state-file seeding, launch, relaunch,
//! JS driving and teardown).
//!
//! Journeys drive the real installed app with the user's own settings, data and OS permissions.
//! Everything a journey can change is snapshotted first and restored afterwards, pass or fail
//! ([`crate::snapshot`], declared in `rightkit-qa.toml` `[snapshot]`). [`LiveApp::launch_seeded`]
//! refuses to start unless a snapshot is armed ([`LiveSnapshot::take`]), and refuses a snapshot that
//! does not cover the app data folder and every root in [`LiveAppConfig::extra_roots`].
//!
//! Ordering is the contract: [`live_harness`] arms (or reuses) the snapshot before anything runs;
//! [`LiveApp::launch_seeded`] takes the proof, then seeds, then launches; dropping a [`LiveApp`] stops
//! the app, then releases the snapshot guard, then disposes the QA workspace. The restore itself runs
//! when the harness's last clone drops (or, for a runner-armed snapshot, in the runner).
//!
//! The host passes everything product-specific in [`LiveAppConfig`] (names, paths, settings, the JS that
//! says the shell is ready); the typed commands the app registers on its control server are called by
//! name through [`LiveApp::command`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::control::{self, Control, LaunchSpec, Mode};
use crate::harness::{Harness, Scenario};
use crate::workspace::{self, QaWorkspace};

/// One JSON state file the harness forces to known values before every launch.
///
/// The file is read from the app data folder (a missing or non-object file starts from
/// `{ "v": 1 }`), the listed keys are set, every other key of the owner's file is kept, the file is
/// written, read back and each key asserted, so a launch never proceeds on an unverified switch. The
/// snapshot restore puts the owner's own file back afterwards.
#[derive(Clone, Debug)]
pub struct StateSeed {
    /// File name inside the app data folder.
    pub file: String,
    /// Keys to force and their values.
    pub set: Vec<(String, Value)>,
}

/// Everything a [`LiveApp`] session needs from the host app.
#[derive(Clone, Debug)]
pub struct LiveAppConfig {
    /// The app's key: names the harness, the workspace and the snapshot marker's owner.
    pub app: String,
    /// The app's repository root: where `rightkit-qa.toml` and the settings file are found.
    pub root: PathBuf,
    /// Settings-file key naming the app binary to test.
    pub binary_setting: String,
    /// The binary tested when the setting is absent (a `.app` bundle on macOS, an executable on Windows).
    pub default_binary: PathBuf,
    /// Run-lock directory: one real app instance at a time on this machine.
    pub lock_path: PathBuf,
    /// How long a journey waits for the run lock.
    pub lock_timeout: Duration,
    /// Name the app's per-user data folder resolves from (`rightkit_fs::app_dirs::AppId`). The
    /// installed (non-dev) flat, machine-local folder is used.
    pub data_app_name: &'static str,
    /// Path of the QA config holding `[snapshot]`, relative to `root`. Default `rightkit-qa.toml`.
    pub qa_config: PathBuf,
    /// Settings file inside the app data folder that [`LiveApp::launch_seeded`] writes.
    pub settings_file: String,
    /// Extra folders the snapshot must name in `extra_paths` (as a directory entry) and must cover
    /// when armed, besides the app data folder (for example the shared model store).
    pub extra_roots: Vec<PathBuf>,
    /// State files forced to known values before every launch and again after the journey's
    /// preparation (for example to switch off remote sync).
    pub state_seeds: Vec<StateSeed>,
    /// Environment passed to every launch.
    pub env: Vec<(String, String)>,
    /// Process label recorded by the launch tracker.
    pub label: String,
    /// How long the launch waits for the control server.
    pub startup_timeout: Duration,
    /// JS body (must `return`) that is truthy once the shell has rendered.
    pub ready_js: String,
    /// Default wait for UI state to appear.
    pub ui_wait: Duration,
    /// Environment variable that, set to `1`, keeps each scenario's run directory as release
    /// evidence (retention otherwise keeps only the newest passing run of each kind). `None`
    /// disables the feature.
    pub keep_evidence_env: Option<String>,
}

impl LiveAppConfig {
    /// A config with neutral defaults: `rightkit-qa.toml`, `settings.json`, a 60 minute lock wait,
    /// a 60 second startup wait, a 30 second UI wait and `RIGHTKIT_QA_NATIVE=1` in the launch
    /// environment. The host sets `ready_js`, `extra_roots` and `state_seeds` as needed.
    pub fn new(
        app: &str,
        root: impl Into<PathBuf>,
        binary_setting: &str,
        default_binary: impl Into<PathBuf>,
        data_app_name: &'static str,
    ) -> Self {
        LiveAppConfig {
            app: app.to_string(),
            root: root.into(),
            binary_setting: binary_setting.to_string(),
            default_binary: default_binary.into(),
            lock_path: std::env::temp_dir().join(format!("{app}-native-e2e.lock")),
            lock_timeout: Duration::from_secs(60 * 60),
            data_app_name,
            qa_config: PathBuf::from("rightkit-qa.toml"),
            settings_file: "settings.json".to_string(),
            extra_roots: Vec::new(),
            state_seeds: Vec::new(),
            env: vec![("RIGHTKIT_QA_NATIVE".to_string(), "1".to_string())],
            label: app.to_string(),
            startup_timeout: Duration::from_secs(60),
            ready_js: "return document.readyState === 'complete'".to_string(),
            ui_wait: Duration::from_secs(30),
            keep_evidence_env: None,
        }
    }

    /// The app under test: the settings file's `binary_setting`, else `default_binary`.
    pub fn app_binary(&self, sc: &Scenario) -> PathBuf {
        self.binary_from(sc.settings().get(&self.binary_setting))
    }

    fn binary_from(&self, setting: Option<String>) -> PathBuf {
        setting.map_or_else(|| self.default_binary.clone(), PathBuf::from)
    }

    /// The installed app's flat, machine-local data folder.
    pub fn live_data_root(&self) -> PathBuf {
        let app = rightkit_fs::app_dirs::AppId::new(self.data_app_name, None);
        let options = rightkit_fs::app_dirs::Options::current().dev(false);
        rightkit_fs::app_dirs::AppDirs::resolve_with_options(app, &options)
            .expect("resolve the installed app-data root")
            .local
    }
}

/// The app root from a crate's `CARGO_MANIFEST_DIR` (pass `env!("CARGO_MANIFEST_DIR")`): its parent
/// directory, canonicalised, in the plain form on Windows.
pub fn app_root_from(manifest_dir: &str) -> PathBuf {
    let root = Path::new(manifest_dir)
        .join("..")
        .canonicalize()
        .expect("app root");
    strip_verbatim(root)
}

/// Windows `canonicalize` returns a `\\?\` path; apps take the plain form.
#[cfg(windows)]
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().into_owned();
    match s.strip_prefix(r"\\?\") {
        Some(plain) => PathBuf::from(plain),
        None => p,
    }
}
#[cfg(not(windows))]
fn strip_verbatim(p: PathBuf) -> PathBuf {
    p
}

/// The per-user suite root the installed app really uses (`rightkit_daemon::Suite::per_user_default`).
pub fn real_suite_root() -> PathBuf {
    rightkit_daemon::Suite::per_user_default()
        .expect("resolve the installed suite's shared-data root")
        .root()
        .to_path_buf()
}

/// Keeps this scenario's run directory when `config.keep_evidence_env` is `1`.
fn keep_release_evidence(config: &LiveAppConfig, sc: &Scenario) {
    let Some(name) = &config.keep_evidence_env else {
        return;
    };
    if std::env::var(name).as_deref() != Ok("1") {
        return;
    }
    if let Some(run) = sc.dir().parent().and_then(Path::parent) {
        crate::storage::mark_keep_data(run).expect("mark release evidence to keep");
    }
}

/// The harness for a live-app journey crate: binds the app binary's digest into the evidence when it
/// exists, arms a snapshot from the QA config's `[snapshot]` unless a runner already armed one, and
/// takes the run lock.
///
/// A runner that already armed a snapshot (`rightkit-qa journeys run`) keeps it: arming a second
/// would collide with its restore marker. Otherwise this process arms its own (the `run-tests`
/// route); the restore runs when the last clone of the harness drops, and arming panics if the app
/// is still running.
pub fn live_harness(config: &LiveAppConfig) -> Harness {
    let h = Harness::new(&config.app, config.root.clone()).expect("rightkit-qa harness");
    let binary = config.binary_from(h.settings().get(&config.binary_setting));
    let h = if binary.is_file() {
        h.with_identity_file(&config.app, &binary)
            .expect("hash app binary")
    } else {
        // An unbuilt app is reported by `LiveApp::launch_seeded`; nothing to bind yet.
        h
    };
    // QA runs on the owner's real data, so a snapshot must be armed before anything launches.
    let h = if crate::snapshot::require_armed().is_err() {
        let qa_config = crate::config::QaConfig::load(&config.root.join(&config.qa_config))
            .expect("load the QA config");
        let snapshot = qa_config
            .require_snapshot()
            .expect("the QA config declares [snapshot]");
        assert_declared_roots(snapshot, &config.extra_roots);
        h.with_snapshot(snapshot)
    } else {
        h
    };
    h.with_run_lock(config.lock_path.clone(), config.lock_timeout)
}

/// A path resolved for snapshot comparison; a missing folder stays as given (a snapshot covers
/// missing folders too: its restore removes any created during QA).
fn snapshot_path(path: &Path) -> PathBuf {
    match path.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => panic!(
            "GC-A10: cannot resolve snapshot path {}: {error}",
            path.display()
        ),
    }
}

/// Panics unless `[snapshot] extra_paths` names every root in `roots` as a directory entry (a
/// trailing separator; `~/` is the account home) before arming.
pub fn assert_declared_roots(snapshot: &crate::config::SnapshotConfig, roots: &[PathBuf]) {
    if roots.is_empty() {
        return;
    }
    let home = rightkit_fs::account_home().expect("resolve the account home for [snapshot]");
    for required in roots {
        let required = snapshot_path(required);
        assert!(
            snapshot.extra_paths.iter().any(|entry| {
                let relative = entry
                    .strip_prefix("~/")
                    .or_else(|| entry.strip_prefix("~\\"));
                let path = relative.map_or_else(|| PathBuf::from(entry), |rest| home.join(rest));
                path.is_absolute()
                    && entry.ends_with(['/', '\\'])
                    && snapshot_path(&path) == required
            }),
            "GC-A10: [snapshot] must name the resolved root {} before arming",
            required.display()
        );
    }
}

/// Panics unless the armed snapshot's marker is armed for `config.app` and covers the app data
/// folder and every root in `config.extra_roots` (directory roots, not single files).
fn assert_armed_data_roots(config: &LiveAppConfig) {
    use crate::snapshot::{armed_marker_path, MARKER_ENV};

    let marker = armed_marker_path()
        .or_else(|| std::env::var_os(MARKER_ENV).map(PathBuf::from))
        .expect("GC-A10: the armed snapshot has no restore marker");
    let marker: Value = serde_json::from_slice(
        &std::fs::read(marker).expect("GC-A10: read the armed snapshot marker"),
    )
    .expect("GC-A10: parse the armed snapshot marker");
    assert_eq!(
        marker["schema"],
        json!(1),
        "GC-A10: unsupported snapshot marker"
    );
    assert_eq!(marker["app"], json!(config.app), "GC-A10: snapshot owner");
    assert_eq!(
        marker["state"],
        json!("armed"),
        "GC-A10: snapshot is not armed"
    );
    let roots = marker["roots"]
        .as_array()
        .expect("GC-A10: the armed snapshot has no roots");
    let mut required = vec![config.live_data_root()];
    required.extend(config.extra_roots.iter().cloned());
    for required in required {
        let required = snapshot_path(&required);
        assert!(
            roots.iter().any(|root| {
                root["file"].as_bool() == Some(false)
                    && root["livePath"].as_str().is_some_and(|path| {
                        let path = Path::new(path);
                        path.is_absolute() && required.starts_with(snapshot_path(path))
                    })
            }),
            "GC-A10: armed snapshot does not cover {}. Refusing to touch live data.",
            required.display()
        );
    }
}

/// Proof that the user's state is protected by a snapshot while a journey runs.
///
/// The snapshot is not taken here. `rightkit-qa journeys run` arms it from `[snapshot]` after the
/// build and before any test process starts, passes the restore marker to the test process
/// (`RIGHTKIT_QA_SNAPSHOT_MARKER`) and restores and verifies the data itself when the run ends
/// however it ends (a kill is restored at the next `rightkit-qa` start); a process that armed its own
/// ([`live_harness`]) restores when its harness drops. A journey therefore never restores early.
pub struct LiveSnapshot {
    _private: (),
}

impl LiveSnapshot {
    /// Refuse unless a verified snapshot is armed ([`crate::snapshot::require_armed`]) and it covers
    /// the config's roots. With none armed this panics, so a test run by hand can never touch live
    /// data.
    pub fn take(config: &LiveAppConfig) -> LiveSnapshot {
        if let Err(refusal) = crate::snapshot::require_armed() {
            panic!(
                "GC-A10: journeys run on the live installed app only behind a rightkit-qa snapshot (rightkit#1437); \
                 start them with `rightkit-qa journeys run`. Refusing to touch live data: {refusal}"
            );
        }
        assert_armed_data_roots(config);
        LiveSnapshot { _private: () }
    }

    /// Nothing to do: the runner or the harness owns the restore (see the type's docs). It runs when
    /// the run ends, not when one app instance drops, so a journey that relaunches keeps its state
    /// across launches.
    pub fn restore(&self) {}
}

/// Force `seeds` into the JSON state files of `app_data` and read each back (see [`StateSeed`]).
pub fn seed_state_files(app_data: &Path, seeds: &[StateSeed]) {
    for seed in seeds {
        let path = app_data.join(&seed.file);
        let mut state: Value = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({ "v": 1 }));
        for (key, value) in &seed.set {
            state[key.as_str()] = value.clone();
        }
        std::fs::write(&path, state.to_string())
            .unwrap_or_else(|e| panic!("seed {}: {e}", path.display()));
        let back: Value = serde_json::from_slice(
            &std::fs::read(&path).unwrap_or_else(|e| panic!("re-read {}: {e}", path.display())),
        )
        .unwrap_or_else(|e| panic!("re-read {}: {e}", path.display()));
        for (key, value) in &seed.set {
            assert_eq!(
                &back[key.as_str()],
                value,
                "{} {key} is not the seeded QA value",
                seed.file
            );
        }
    }
}

/// A launched app driven through its in-app control server. Dropping it stops the app and removes
/// its QA home.
pub struct LiveApp {
    /// The control-server client.
    pub ctl: Control,
    config: LiveAppConfig,
    ws: QaWorkspace,
    spec: LaunchSpec,
    app_data: PathBuf,
    snapshot: LiveSnapshot,
}

impl LiveApp {
    /// Launch `mode` from `binary` (else the configured binary) with `settings` as the seeded settings
    /// file; `prepare` runs on the live app data folder before the app starts (model fixtures,
    /// markers). Both happen after the snapshot proof. With `wait_ready` the call also waits for
    /// `config.ready_js`.
    pub fn launch_seeded(
        config: &LiveAppConfig,
        sc: &Scenario,
        mode: Mode,
        binary: Option<PathBuf>,
        settings: &str,
        prepare: impl FnOnce(&Path),
        wait_ready: bool,
    ) -> LiveApp {
        let binary = binary.unwrap_or_else(|| config.app_binary(sc));
        assert!(
            binary.exists(),
            "{} app binary missing at {}: run the QA build suite first, or set {} in the rightkit-qa settings",
            config.app,
            binary.display(),
            config.binary_setting
        );
        keep_release_evidence(config, sc);
        // Before anything is written: the restore puts the user's state back.
        let snapshot = LiveSnapshot::take(config);
        let ws = workspace::create(&sc.scratch("ws"), None, &config.app).expect("qa workspace");
        let app_data = config.live_data_root();
        std::fs::create_dir_all(&app_data).expect("live app data folder");
        std::fs::write(app_data.join(&config.settings_file), settings).expect("seed settings");
        seed_state_files(&app_data, &config.state_seeds);
        prepare(&app_data);
        // Again after `prepare`: whatever a journey's preparation writes, the seeded switches hold.
        seed_state_files(&app_data, &config.state_seeds);
        let spec = LaunchSpec {
            binary,
            mode,
            env: config.env.clone(),
            startup_timeout: config.startup_timeout,
            label: config.label.clone(),
            focus: false,
        };
        let ctl = control::launch_declared(&spec, &ws, sc.tracker(), &[])
            .unwrap_or_else(|e| panic!("launch {}: {e}", config.label));
        // Built before the wait so a failed wait still stops the app and restores the snapshot.
        let app = LiveApp {
            ctl,
            config: config.clone(),
            ws,
            spec,
            app_data,
            snapshot,
        };
        if wait_ready {
            app.wait_js(&app.config.ready_js, app.config.ui_wait);
        }
        app
    }

    /// Stop the app and start it again in the same workspace, keeping its data (what a user's next
    /// launch sees). With `wait_ready` the call waits for the shell.
    pub fn restart(&mut self, sc: &Scenario, wait_ready: bool) {
        self.restart_prepared(sc, wait_ready, |_| {});
    }

    /// Prepare the next launch's state after the old process and control endpoint are gone.
    pub fn restart_prepared(
        &mut self,
        sc: &Scenario,
        wait_ready: bool,
        prepare: impl FnOnce(&Path),
    ) {
        let stopped = self.ctl.stop().expect("stop the app before relaunch");
        assert!(
            stopped.process_gone && stopped.endpoint_closed,
            "{} remained active before relaunch",
            self.config.label
        );
        let _snapshot = LiveSnapshot::take(&self.config);
        prepare(&self.app_data);
        self.ctl = control::launch_declared(&self.spec, &self.ws, sc.tracker(), &[])
            .unwrap_or_else(|e| panic!("relaunch {}: {e}", self.config.label));
        if wait_ready {
            self.wait_js(&self.config.ready_js, self.config.ui_wait);
        }
    }

    /// The config this session was launched with.
    pub fn config(&self) -> &LiveAppConfig {
        &self.config
    }

    /// The live app data folder (settings, models, markers).
    pub fn app_data_dir(&self) -> &Path {
        &self.app_data
    }

    /// The per-user suite root the installed app really uses ([`real_suite_root`]). This is the
    /// owner's real store: read it, never write to it outside a snapshot-protected run.
    pub fn suite_root(&self) -> PathBuf {
        real_suite_root()
    }

    /// Run a native command from the webview the way the UI does (`__TAURI_INTERNALS__.invoke`).
    /// `Ok` is the command's value, `Err` its refusal text; a command that never answers panics at
    /// the UI wait through [`LiveApp::wait_js`].
    pub fn invoke(&self, command: &str, args: Value) -> Result<Value, String> {
        self.js(&format!(
            "window.__qaInvoke = {{ done: false }}; window.__TAURI_INTERNALS__.invoke({}, {}).then(\
             (value) => {{ window.__qaInvoke = {{ done: true, ok: true, value }}; }}, \
             (error) => {{ window.__qaInvoke = {{ done: true, ok: false, error: typeof error === 'string' ? error : JSON.stringify(error) }}; }}); return true;",
            Value::String(command.into()),
            args
        ));
        let outcome = self.wait_js(
            "return window.__qaInvoke && window.__qaInvoke.done ? window.__qaInvoke : null",
            self.config.ui_wait,
        );
        if outcome["ok"] == json!(true) {
            Ok(outcome["value"].clone())
        } else {
            Err(outcome["error"].as_str().unwrap_or("").to_string())
        }
    }

    /// Capture the window as PNG into the scenario evidence under `name`.
    pub fn shot(&self, sc: &Scenario, name: &str) {
        let path = sc.dir().join(format!("{name}.png"));
        self.ctl
            .screenshot_to(&path)
            .expect("save scenario screenshot");
        sc.keep(name, &path);
    }

    /// Real pointer click on the first enabled `button` whose trimmed text starts with `text`.
    pub fn click_button(&self, text: &str) {
        let tagged = self.js(&format!(
            "const t = {}; const b = [...document.querySelectorAll('button')].find(b => !b.disabled && b.textContent.trim().startsWith(t)); if (!b) return false; b.setAttribute('data-qa-pick', '1'); return true;",
            Value::String(text.into())
        ));
        assert!(
            tagged == json!(true),
            "no enabled button starting with {text:?}"
        );
        self.click("[data-qa-pick]");
        self.js("document.querySelector('[data-qa-pick]')?.removeAttribute('data-qa-pick'); return true;");
    }

    /// Visible text of the document body.
    pub fn body_text(&self) -> String {
        self.js("return document.body.innerText")
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    /// The process id of the app under test.
    pub fn pid(&self) -> u32 {
        self.ctl.pid
    }

    /// One command the app registered on the control server. The arguments are one JSON value; the
    /// reply is the command's JSON, or the control server's error text.
    pub fn try_command(&self, name: &str, args: Value) -> Result<Value, String> {
        self.ctl.command(name, &args).map_err(|e| e.to_string())
    }

    /// [`LiveApp::try_command`], panicking with the command name when it fails.
    pub fn command(&self, name: &str, args: Value) -> Value {
        self.try_command(name, args)
            .unwrap_or_else(|e| panic!("command {name} failed: {e}"))
    }

    /// Synchronous JS body (must `return`).
    pub fn js(&self, body: &str) -> Value {
        self.ctl.eval(body).expect("eval")
    }

    /// Poll a JS body until it returns something truthy, or panic at the deadline with what the page
    /// shows instead (the first failure on a new host is otherwise blind).
    pub fn wait_js(&self, body: &str, timeout: Duration) -> Value {
        self.wait_js_impl(body, timeout, false)
    }

    /// [`LiveApp::wait_js`] that first finishes every FINITE running animation while the window is
    /// hidden (`Control::eval_with`, `finishHiddenAnimations`; infinite ones are left alone). A hidden
    /// web view produces no frames, so `document.timeline` stays at its first value and every finite
    /// CSS animation stays pending for ever; the end state is what the page settles to when it does
    /// render. Replaces a host-side script prefix that did the same by hand.
    pub fn wait_js_finishing(&self, body: &str, timeout: Duration) -> Value {
        self.wait_js_impl(body, timeout, true)
    }

    fn wait_js_impl(&self, body: &str, timeout: Duration, finish_hidden_animations: bool) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(v) = self.ctl.eval_with(body, finish_hidden_animations) {
                if !matches!(v, Value::Null | Value::Bool(false)) && v != json!(0) && v != json!("")
                {
                    return v;
                }
            }
            if Instant::now() > deadline {
                let seen = self
                    .ctl
                    .eval("return location.href + ' | ' + (document.body?.innerText || '').slice(0, 400)")
                    .unwrap_or_else(|e| json!(format!("<eval failed: {e}>")));
                panic!("timed out after {timeout:?} waiting for: {body}\npage: {seen}");
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// The `data-nav-id` of the current shell navigation item (`aria-current="page"`).
    pub fn nav_current(&self) -> String {
        self.js(
            "return document.querySelector('.rk-nav [aria-current=page]')?.dataset.navId || null",
        )
        .as_str()
        .unwrap_or("")
        .to_string()
    }

    /// Real pointer click at the centre of the first element matching `selector`, scrolled into view.
    pub fn click(&self, selector: &str) {
        self.js(&format!(
            "const e = document.querySelector({}); if (e) e.scrollIntoView({{ block: 'center', inline: 'nearest' }}); return true;",
            Value::String(selector.into())
        ));
        self.ctl
            .click_selector(selector)
            .unwrap_or_else(|e| panic!("click {selector}: {e}"));
    }
}

impl Drop for LiveApp {
    fn drop(&mut self) {
        let _ = self.ctl.stop();
        self.snapshot.restore();
        // The QA home is removed pass or fail; a retained home is named so it can be cleaned by hand.
        let report = workspace::dispose(&self.ws, false, 20, 250);
        for (path, why) in &report.retained {
            eprintln!("QA home not removed: {} ({why})", path.display());
        }
    }
}

/// Collapse every run of whitespace (including newlines and non-breaking spaces) to one space.
pub fn norm(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Case-insensitive containment for rendered text: `innerText` applies CSS `text-transform`, so a
/// display heading may be upper-case.
pub fn has_text(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Poll `probe` every 120 ms until it returns `Some`, or panic with `what` and the last error text
/// at the deadline. A probe that errors is retried.
pub fn wait_until<T>(
    what: &str,
    timeout: Duration,
    mut probe: impl FnMut() -> Result<Option<T>, String>,
) -> T {
    let deadline = Instant::now() + timeout;
    let mut last = String::from("no reading");
    loop {
        match probe() {
            Ok(Some(value)) => return value,
            Ok(None) => {}
            Err(error) => last = error,
        }
        if Instant::now() > deadline {
            panic!("{what}: timed out after {timeout:?}; last: {last}");
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}
