//! Drive the real Tauri app through the in-app `rightkit-control` server.
//!
//! The app is launched hidden or backgrounded with `RIGHTKIT_CONTROL_SERVICE` and an
//! isolated `RIGHTKIT_SUITE_ROOT`, discovers the `rightkit-service` record
//! (`service.json`), and talks over the current-user-only socket / named pipe as the
//! allowlisted app `right-qa`. No TCP and no shared secrets. The harness never searches for
//! processes by name to kill them: it records the pid it started and kills that
//! tree only.
use crate::process::{is_alive, kill_tree, Tracker};
use crate::util::{err, new_id, sleep_ms, Error, Result};
use crate::workspace::QaWorkspace;
use rightkit_process::OwnedChild;
#[cfg(not(target_os = "macos"))]
use rightkit_process::OwnedCommand;
use rightkit_service::{Client, ServiceError, Suite};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(not(target_os = "macos"))]
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Window never shown (`open -g -j -n` on macOS, hidden window on Windows).
    Hidden,
    /// Window shown but the app is not activated and frontmost is unchanged.
    Background,
    Visible,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "hidden" => Ok(Mode::Hidden),
            "background" => Ok(Mode::Background),
            "visible" => Ok(Mode::Visible),
            other => err(format!(
                "unknown ui mode '{other}' (hidden|background|visible)"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// A `.app` bundle, or a plain executable (wrapped into a throwaway bundle on macOS).
    pub binary: PathBuf,
    pub mode: Mode,
    pub env: Vec<(String, String)>,
    pub startup_timeout: Duration,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct StopReport {
    pub pid: u32,
    pub endpoint_closed: bool,
    pub process_gone: bool,
    pub frontmost_before: Option<String>,
    pub frontmost_after: Option<String>,
    pub frontmost_unchanged: bool,
    /// Every distinct foreground state sampled during the session (empty when unmeasurable).
    pub frontmost_states: Vec<String>,
    /// Every sampled foreground PID belonged to no app process started by this control session.
    /// This is the background invariant; `frontmost_unchanged` remains diagnostic only.
    pub owned_never_frontmost: bool,
    /// Owned fixture PIDs observed in foreground, if any.
    pub owned_frontmost_pids: Vec<u32>,
}

pub struct Control {
    pub pid: u32,
    /// `rightkit-service` name the app serves (`control-xxxxxxxx`).
    pub service: String,
    pub log_path: PathBuf,
    raw_log: PathBuf,
    suite: Suite,
    endpoint: String,
    client: Mutex<Option<Arc<Client>>>,
    child: Option<OwnedChild>,
    tracker: Tracker,
    frontmost_before: Option<String>,
    focus: Option<FocusMonitor>,
    request_timeout: Duration,
    stopped: bool,
}

const APP: &str = "right-qa";

fn svc_err(e: ServiceError) -> Error {
    Error(e.to_string())
}

enum CallError {
    TimedOut,
    Service(ServiceError),
}

/// Run `f` on a helper thread and give up after `timeout`; the thread is left to finish
/// on its own (it ends when the app is killed or the connection is aborted).
fn bounded<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).ok()
}

fn call_bounded(
    client: &Arc<Client>,
    method: &str,
    params: Value,
    timeout: Duration,
) -> std::result::Result<Value, CallError> {
    let c = client.clone();
    let m = method.to_string();
    match bounded(timeout, move || c.call(&m, params)) {
        Some(r) => r.map_err(CallError::Service),
        None => {
            client.abort();
            Err(CallError::TimedOut)
        }
    }
}

fn connect_bounded(suite: &Suite, service: &str, timeout: Duration) -> Result<Client> {
    let (s, sv) = (suite.clone(), service.to_string());
    match bounded(timeout, move || Client::connect(&s, &sv, APP)) {
        Some(r) => r.map_err(svc_err),
        None => err(format!(
            "connecting to {service} did not finish within {}ms",
            timeout.as_millis()
        )),
    }
}

fn bounded_connect_health(suite: &Suite, service: &str, timeout: Duration) -> Option<Arc<Client>> {
    let (s, sv) = (suite.clone(), service.to_string());
    bounded(timeout, move || {
        let c = Client::connect(&s, &sv, APP).ok()?;
        c.call("health", json!({})).ok()?;
        Some(Arc::new(c))
    })
    .flatten()
}

fn endpoint_open(endpoint: &str) -> bool {
    rightkit_service::probe_endpoint(endpoint)
}

/// Foreground app as `name` (macOS) or `pid:hwnd` (Windows). `None` means the
/// foreground could NOT be measured; it is never treated as "unchanged".
pub fn frontmost() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        rightkit_control::mac::frontmost_name()
    }
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        unsafe {
            let h = GetForegroundWindow();
            if h.0.is_null() {
                return None;
            }
            let mut pid = 0u32;
            if GetWindowThreadProcessId(h, Some(&mut pid)) == 0 || pid == 0 {
                return None;
            }
            Some(format!("{pid}:{:x}", h.0 as usize))
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

/// Samples the foreground on its own thread for the whole life of a control session, so
/// a transient steal (focus leaves and returns) is caught, and an unmeasurable
/// foreground is reported as such instead of comparing equal.
struct FocusMonitor {
    stop: Arc<std::sync::atomic::AtomicBool>,
    seen: Arc<Mutex<(usize, usize, Vec<String>)>>, // (measured, unavailable, distinct states in order)
    owned: Arc<Mutex<(Vec<u32>, Option<String>)>>, // primary PID(s), exact executable prefix
    enforce_owned: bool,
    owned_frontmost: Arc<Mutex<Vec<u32>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FocusMonitor {
    fn start(enforce_owned: bool) -> FocusMonitor {
        let seen: Arc<Mutex<(usize, usize, Vec<String>)>> = Arc::new(Mutex::new((0, 0, vec![])));
        let owned: Arc<Mutex<(Vec<u32>, Option<String>)>> = Arc::new(Mutex::new((vec![], None)));
        let owned_frontmost = Arc::new(Mutex::new(vec![]));
        let record = move |seen: &Mutex<(usize, usize, Vec<String>)>,
                           _owned: &Mutex<(Vec<u32>, Option<String>)>,
                           _owned_frontmost: &Mutex<Vec<u32>>| {
            {
                let mut g = seen.lock().unwrap_or_else(|e| e.into_inner());
                match frontmost() {
                    Some(f) => {
                        g.0 += 1;
                        if g.2.last() != Some(&f) {
                            g.2.push(f);
                        }
                    }
                    None => g.1 += 1,
                }
            }
            #[cfg(target_os = "macos")]
            if enforce_owned {
                match rightkit_control::mac::frontmost_pid() {
                    Some(pid) => {
                        let (primary, prefix) =
                            _owned.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        let mut candidates = primary;
                        let mut inventory_ok = true;
                        if let Some(prefix) = prefix {
                            match pids_with_command_prefix(&prefix) {
                                Ok(pids) => candidates.extend(pids),
                                Err(_) => inventory_ok = false,
                            }
                        }
                        if !inventory_ok {
                            let mut g = seen.lock().unwrap_or_else(|e| e.into_inner());
                            g.1 += 1;
                        }
                        if candidates.into_iter().any(|p| p == pid as u32) {
                            let mut observed =
                                _owned_frontmost.lock().unwrap_or_else(|e| e.into_inner());
                            if !observed.contains(&(pid as u32)) {
                                observed.push(pid as u32);
                            }
                        }
                    }
                    None => {
                        // An unmeasurable PID makes the strict background proof fail.
                        let mut g = seen.lock().unwrap_or_else(|e| e.into_inner());
                        g.1 += 1;
                    }
                }
            }
        };
        record(&seen, &owned, &owned_frontmost);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s2, seen2, owned2, observed2) = (
            stop.clone(),
            seen.clone(),
            owned.clone(),
            owned_frontmost.clone(),
        );
        let thread = std::thread::spawn(move || {
            while !s2.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
                record(&seen2, &owned2, &observed2);
            }
        });
        FocusMonitor {
            stop,
            seen,
            owned,
            enforce_owned,
            owned_frontmost,
            thread: Some(thread),
        }
    }
    fn track_primary(&self, pid: u32) {
        self.owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
            .push(pid);
    }
    #[cfg(target_os = "macos")]
    fn track_prefix(&self, prefix: String) {
        self.owned.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(prefix);
    }
    /// `(unchanged, states, owned_never_frontmost, owned_frontmost_pids)`.
    fn finish(&mut self) -> (bool, Vec<String>, bool, Vec<u32>) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let g = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let unchanged = g.0 > 0 && g.1 == 0 && g.2.len() == 1;
        let observed = self
            .owned_frontmost
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        #[cfg(target_os = "macos")]
        // Unverified launch interval: if the executable identity was never registered
        // before launch, startup samples could not be attributed, so never claim success.
        let identity_registered = self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .is_some();
        #[cfg(target_os = "macos")]
        let owned_never_frontmost =
            !self.enforce_owned || (identity_registered && g.1 == 0 && observed.is_empty());
        #[cfg(not(target_os = "macos"))]
        // No native foreground-PID sampler exists on these targets yet; background
        // claims fail closed instead of treating name-only sampling as proof.
        let owned_never_frontmost = !self.enforce_owned;
        (unchanged, g.2.clone(), owned_never_frontmost, observed)
    }
}

/// Environment passed to the app must not put secrets in a process listing: on macOS
/// `open --env K=V` exposes values in argv. Only `RIGHTKIT_*` keys travel that way, and
/// no key that names a credential is accepted at all.
fn check_env(env: &[(String, String)]) -> Result<()> {
    for (k, _) in env {
        let up = k.to_ascii_uppercase();
        if [
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "PASSWD",
            "API_KEY",
            "APIKEY",
            "PRIVATE_KEY",
            "CREDENTIAL",
        ]
        .iter()
        .any(|w| up.contains(w))
        {
            return err(format!("environment key {k} names a credential; secrets are never passed to a launched app through its command line or environment"));
        }
        #[cfg(target_os = "macos")]
        if !up.starts_with("RIGHTKIT_") {
            return err(format!("environment key {k} would be exposed in `open` arguments; macOS launches carry only RIGHTKIT_* keys"));
        }
    }
    Ok(())
}

/// Absolute path of the executable that `open` will actually run for `bundle`, read from
/// `Contents/Info.plist` `CFBundleExecutable`. `None` (fail closed) when the key is absent,
/// names a path component trick, or the file is not a regular file inside `Contents/MacOS`.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub fn resolve_bundle_executable(bundle: &Path) -> Option<String> {
    let plist = bundle.join("Contents").join("Info.plist");
    let out = Command::new("plutil")
        .args(["-extract", "CFBundleExecutable", "raw", "-o", "-"])
        .arg(&plist)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8(out.stdout).ok()?;
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return None;
    }
    let exe = bundle.join("Contents").join("MacOS").join(name);
    let meta = fs::metadata(&exe).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(exe.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
fn make_bundle(dir: &Path, binary: &Path, id: &str, background: bool) -> Result<PathBuf> {
    let exe = binary
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error("binary has no file name".into()))?;
    let app = dir.join(format!("{exe}.app"));
    let macos = app.join("Contents").join("MacOS");
    fs::create_dir_all(&macos)?;
    let target = macos.join(exe);
    let _ = fs::remove_file(&target);
    let abs = fs::canonicalize(binary)
        .map_err(|e| Error(format!("app binary not found: {}: {e}", binary.display())))?;
    // The process must show the bundle path (not a resolved symlink) so the harness can
    // find exactly the instance it started: hard link, else copy.
    if fs::hard_link(&abs, &target).is_err() {
        fs::copy(&abs, &target)?;
    }
    // Background fixtures must be non-activating before AppKit creates any window.
    let ui_element = if background {
        "<key>LSUIElement</key><true/>\n"
    } else {
        ""
    };
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>CFBundleExecutable</key><string>{exe}</string>\n<key>CFBundleIdentifier</key><string>app.rightkit.qa.{id}</string>\n<key>CFBundleName</key><string>{exe}</string>\n<key>CFBundlePackageType</key><string>APPL</string>\n<key>CFBundleVersion</key><string>1</string>\n{ui_element}</dict></plist>\n"
    );
    fs::write(app.join("Contents").join("Info.plist"), plist)?;
    Ok(app)
}

#[cfg(target_os = "macos")]
/// Pids whose command line STARTS with `prefix` (so `open`, which merely carries the
/// bundle path as an argument, is never mistaken for the app).
fn pids_with_command_prefix(prefix: &str) -> std::result::Result<Vec<u32>, String> {
    let out = Command::new("ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
        .map_err(|e| format!("ps PID inventory failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("ps PID inventory exited with {}", out.status));
    }
    let mut pids = vec![];
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim_start();
        let Some((pid, cmd)) = line.split_once(' ') else {
            continue;
        };
        if cmd.trim_start().starts_with(prefix) {
            pids.push(
                pid.trim()
                    .parse::<u32>()
                    .map_err(|e| format!("invalid PID inventory row: {e}"))?,
            );
        }
    }
    Ok(pids)
}

pub fn launch(spec: &LaunchSpec, ws: &QaWorkspace, tracker: &Tracker) -> Result<Control> {
    if !spec.binary.exists() {
        return err(format!(
            "UI app binary not found: {}",
            spec.binary.display()
        ));
    }
    let id = new_id().replace('-', "");
    let service = format!("control-{}", &id[..8]);
    // Isolated per-run suite root: the app's service.json never collides with an installed app's.
    let suite =
        Suite::at(ws.home.join(format!("suite-{}", &id[..8]))).map_err(|e| Error(e.to_string()))?;
    let log_path = ws.evidence_dir.join(format!("app-{}.log", &id[..8]));
    // launchd-started apps cannot open log files on external volumes (`open` fails with
    // -10810), so the live log goes to the system temp dir and is copied into the
    // evidence dir when the app stops.
    let raw_log = ws.home.join(format!("app-{}.log", &id[..8]));
    // macOS launches go through `open --env`, which exposes values in argv, so only the
    // workspace's RIGHTKIT_* keys travel; apps derive isolation from RIGHTKIT_QA_DATA_DIR
    // there. The XDG_*/WEBVIEW2_*/<APP>_DATA_DIR keys serve direct-exec platforms.
    // Caller-supplied `spec.env` is still checked strictly below.
    let mut env: Vec<(String, String)> = ws
        .env
        .iter()
        .filter(|(k, _)| cfg!(not(target_os = "macos")) || k.starts_with("RIGHTKIT_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    env.push(("RIGHTKIT_CONTROL_SERVICE".into(), service.clone()));
    env.push((
        "RIGHTKIT_SUITE_ROOT".into(),
        suite.root().to_string_lossy().into(),
    ));
    env.push((
        "RIGHTKIT_QA_HIDDEN".into(),
        if spec.mode == Mode::Hidden { "1" } else { "0" }.into(),
    ));
    // Hidden runs stay hidden until a test explicitly reveals a window; any such
    // reveal still uses the background window policy and target-pid input.
    env.push((
        "RIGHTKIT_QA_BACKGROUND".into(),
        if spec.mode != Mode::Visible { "1" } else { "0" }.into(),
    ));
    env.extend(spec.env.iter().cloned());
    check_env(&env)?;
    let frontmost_before = frontmost();
    let focus = FocusMonitor::start(spec.mode != Mode::Visible);
    let deadline = Instant::now() + spec.startup_timeout;

    // The executable identity is registered inside start_process BEFORE `open`, so
    // startup foreground samples are attributed; the pid follows once known.
    let (pid, child) = start_process(spec, ws, &env, &raw_log, &id, tracker, &focus, deadline)?;
    focus.track_primary(pid);
    tracker.register(pid, &spec.label);

    let mut control = Control {
        pid,
        service: service.clone(),
        log_path,
        raw_log,
        suite: suite.clone(),
        endpoint: String::new(),
        client: Mutex::new(None),
        child,
        tracker: tracker.clone(),
        frontmost_before,
        focus: Some(focus),
        request_timeout: Duration::from_secs(20),
        stopped: false,
    };
    loop {
        // Each attempt is bounded by the startup deadline: a hung host must not defeat it.
        let left = deadline.saturating_duration_since(Instant::now());
        if let Some(c) = bounded_connect_health(&suite, &service, left.min(Duration::from_secs(5)))
        {
            control.endpoint = c.record().endpoint.clone();
            *control.client.lock().unwrap() = Some(c);
            break;
        }
        if !is_alive(pid) {
            let tail = fs::read_to_string(&control.raw_log)
                .map(|t| crate::util::tail(&t, 1500))
                .unwrap_or_default();
            return err(format!(
                "app exited before its control server came up (pid {pid}); log tail: {tail}"
            ));
        }
        if Instant::now() >= deadline {
            let _ = control.stop();
            return err(format!(
                "control service did not answer within {}ms; was the app built with its control feature and launched with RIGHTKIT_CONTROL_SERVICE?",
                spec.startup_timeout.as_millis()
            ));
        }
        sleep_ms(100);
    }
    // The server comes up before the window exists (hidden launch): wait until the page
    // itself answers, so the first scenario step never races window creation.
    loop {
        if let Ok(Value::String(state)) = control.eval("return document.readyState;") {
            if state == "complete" || state == "interactive" {
                break;
            }
        }
        if Instant::now() >= deadline {
            let _ = control.stop();
            return err(format!(
                "app window never became ready within {}ms",
                spec.startup_timeout.as_millis()
            ));
        }
        sleep_ms(100);
    }
    Ok(control)
}

#[allow(clippy::too_many_arguments)]
fn start_process(
    spec: &LaunchSpec,
    ws: &QaWorkspace,
    env: &[(String, String)],
    log_path: &Path,
    id: &str,
    _tracker: &Tracker,
    focus: &FocusMonitor,
    deadline: Instant,
) -> Result<(u32, Option<OwnedChild>)> {
    #[cfg(target_os = "macos")]
    {
        let bundle = if spec.binary.extension().map(|e| e == "app").unwrap_or(false) {
            spec.binary.clone()
        } else {
            make_bundle(&ws.root, &spec.binary, &id[..8], spec.mode != Mode::Visible)?
        };
        let needle = format!("{}/Contents/MacOS/", bundle.display());
        // Register the REAL executable (CFBundleExecutable) before `open`. When it cannot be
        // resolved and validated, register nothing: the launch interval stays unverified and
        // `owned_never_frontmost` fails closed instead of matching a guessed name.
        if let Some(executable) = resolve_bundle_executable(&bundle) {
            focus.track_prefix(executable);
        }
        let mut cmd = Command::new("open");
        match spec.mode {
            Mode::Hidden => cmd.args(["-g", "-j", "-n"]),
            Mode::Background => cmd.args(["-g", "-n"]),
            Mode::Visible => cmd.args(["-n"]),
        };
        // `open --env NAME=VALUE` is the only form that reaches the app on current macOS
        // (bare `--env NAME` forwarding does not). The per-run secret is therefore briefly
        // visible in `ps`; it is minted per launch, loopback-only, and dies with the app.
        for (k, v) in env {
            cmd.arg("--env").arg(format!("{k}={v}"));
        }
        cmd.arg("--stdout")
            .arg(log_path)
            .arg("--stderr")
            .arg(log_path)
            .arg(&bundle);
        let st = cmd.status()?;
        if !st.success() {
            return err(format!("open exited {st}"));
        }
        loop {
            if let Some(pid) = pids_with_command_prefix(&needle)
                .ok()
                .and_then(|pids| pids.into_iter().next())
            {
                return Ok((pid, None));
            }
            if Instant::now() >= deadline {
                return err("launched app pid not found");
            }
            sleep_ms(100);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (ws, id, focus);
        let log = fs::File::create(log_path)?;
        let log2 = log.try_clone()?;
        let mut cmd = Command::new(&spec.binary);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log2));
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut owned = OwnedCommand::from_command(cmd);
        if spec.mode != Mode::Visible {
            owned.windows_hide();
        }
        let _ = deadline;
        let child = owned
            .spawn()
            .map_err(|e| Error(format!("failed to start {}: {e}", spec.binary.display())))?;
        Ok((child.id(), Some(child)))
    }
}

impl Control {
    pub fn set_request_timeout(&mut self, t: Duration) {
        self.request_timeout = t;
    }

    /// One bounded request. The deadline is real: when it passes the connection is
    /// aborted (unblocking the read) and the call fails. A request that may already have
    /// reached the app is replayed only when the method is a pure read; effects
    /// (click, key, type, command, eval, ...) are never silently executed twice.
    fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let replayable = matches!(method, "health" | "dom" | "ax" | "screenshot" | "move");
        let mut g = self.client.lock().unwrap();
        for attempt in 0..2 {
            let client = match g.as_ref() {
                Some(c) => c.clone(),
                None => {
                    // Not yet sent anything: reconnecting is always safe.
                    let c = Arc::new(connect_bounded(
                        &self.suite,
                        &self.service,
                        self.request_timeout,
                    )?);
                    *g = Some(c.clone());
                    c
                }
            };
            match call_bounded(&client, method, params.clone(), self.request_timeout) {
                Ok(v) => return Ok(v),
                Err(CallError::TimedOut) => {
                    *g = None;
                    return err(format!(
                        "{method}: no reply within {}ms; connection aborted",
                        self.request_timeout.as_millis()
                    ));
                }
                Err(CallError::Service(e)) if e.retryable && replayable && attempt == 0 => {
                    *g = None
                }
                Err(CallError::Service(e)) => {
                    if e.retryable {
                        *g = None;
                    }
                    let mut m = svc_err(e);
                    if !replayable {
                        m = Error(format!("{} (not replayed: {method} has effects and the first attempt may have been applied)", m.0));
                    }
                    return Err(m);
                }
            }
        }
        err("control rpc failed")
    }

    fn json(&self, method: &str, params: Value) -> Result<Value> {
        let v = self.rpc(method, params)?;
        if v.get("ok") == Some(&Value::Bool(false)) {
            return err(format!(
                "{method}: {}",
                v.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
            ));
        }
        Ok(v)
    }

    pub fn health(&self) -> Result<Value> {
        self.rpc("health", json!({}))
    }
    /// Prove the allowlist: an app that is not on it is refused at hello. Returns the error code.
    pub fn unlisted_app_probe(&self) -> Result<String> {
        match Client::connect(&self.suite, &self.service, "not-allowlisted-probe") {
            Ok(_) => err("an unlisted app was accepted by the control service"),
            Err(e) => Ok(e.code),
        }
    }
    pub fn click(&self, x: f64, y: f64, button: &str, count: u32) -> Result<()> {
        self.json(
            "click",
            json!({"x": x, "y": y, "button": button, "count": count}),
        )
        .map(|_| ())
    }
    /// [`click`](Self::click) holding modifiers (`"shift"`, `"cmd"`, `"alt"`, `"ctrl"`). They
    /// travel on the native mouse events, so the page sees `e.shiftKey` etc. as real input.
    pub fn click_with(
        &self,
        x: f64,
        y: f64,
        button: &str,
        count: u32,
        modifiers: &[&str],
    ) -> Result<()> {
        self.json(
            "click",
            json!({"x": x, "y": y, "button": button, "count": count, "modifiers": modifiers}),
        )
        .map(|_| ())
    }
    pub fn move_to(&self, x: f64, y: f64) -> Result<()> {
        self.json("move", json!({"x": x, "y": y})).map(|_| ())
    }
    pub fn drag(&self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
        self.json(
            "drag",
            json!({"x1": from.0, "y1": from.1, "x2": to.0, "y2": to.1, "steps": steps}),
        )
        .map(|_| ())
    }
    pub fn wheel(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<()> {
        self.json("wheel", json!({"x": x, "y": y, "dx": dx, "dy": dy}))
            .map(|_| ())
    }
    pub fn key(&self, spec: &str) -> Result<()> {
        self.json("key", json!({"key": spec})).map(|_| ())
    }
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.json("type", json!({"text": text})).map(|_| ())
    }
    /// Evaluate a JS function body (`return ...`) in the page; returns its JSON value.
    pub fn eval(&self, js: &str) -> Result<Value> {
        Ok(self
            .json("eval", json!({"js": js}))?
            .get("value")
            .cloned()
            .unwrap_or(Value::Null))
    }
    pub fn dom(&self, selector: &str) -> Result<Vec<Value>> {
        let v = self.json("dom", json!({"selector": selector}))?;
        Ok(v.get("elements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
    pub fn ax(&self) -> Result<Vec<Value>> {
        let v = self.json("ax", json!({}))?;
        Ok(v.get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
    pub fn screenshot_png(&self) -> Result<Vec<u8>> {
        let v = self.json("screenshot", json!({}))?;
        // The server masks credential elements before writing; a server that does not
        // report `masked` predates redaction and its captures are refused.
        if v.get("masked").and_then(Value::as_u64).is_none() {
            return err("control server does not redact screenshots; capture refused");
        }
        let path = PathBuf::from(
            v.get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| Error("screenshot returned no path".into()))?,
        );
        let body = fs::read(&path)?;
        let _ = fs::remove_file(&path);
        if body.len() < 8 || &body[1..4] != b"PNG" {
            return err("screenshot method did not return a PNG");
        }
        Ok(body)
    }
    pub fn screenshot_to(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(path, self.screenshot_png()?)?;
        Ok(())
    }
    /// A typed app command registered with `Control::command(name, f)`. The server
    /// hands the command its `args` string verbatim.
    pub fn command(&self, name: &str, args: &Value) -> Result<Value> {
        let args = match args {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let v = self.json("command", json!({"name": name, "args": args}))?;
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Centre of the first element matching `selector`, in viewport CSS px.
    pub fn center_of(&self, selector: &str) -> Result<(f64, f64)> {
        let els = self.dom(selector)?;
        let first = els
            .first()
            .ok_or_else(|| Error(format!("selector matched no element: {selector}")))?;
        let r: Vec<f64> = first
            .get("rect")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        if r.len() < 4 || r[2] <= 0.0 || r[3] <= 0.0 {
            return err(format!("{selector} has no visible box"));
        }
        Ok((r[0] + r[2] / 2.0, r[1] + r[3] / 2.0))
    }
    pub fn click_selector(&self, selector: &str) -> Result<()> {
        let (x, y) = self.center_of(selector)?;
        self.click(x, y, "left", 1)
    }

    /// Poll a JS function body until it returns something truthy; bounded.
    pub fn wait_eval(&self, js: &str, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        let mut last;
        loop {
            match self.eval(js) {
                Ok(v) => {
                    let truthy = !matches!(v, Value::Null | Value::Bool(false))
                        && v != json!(0)
                        && v != json!("");
                    if truthy {
                        return Ok(v);
                    }
                    last = v;
                }
                Err(e) => last = json!(e.0),
            }
            if Instant::now() >= deadline {
                return err(format!("timeout waiting for `{js}` (last: {last})"));
            }
            sleep_ms(100);
        }
    }

    /// Wait until `text` appears in the visible text (`innerText`) of the first element
    /// matching `selector` (`None` = the whole body). Polls every 100 ms until `timeout`.
    pub fn wait_for_text(
        &self,
        selector: Option<&str>,
        text: &str,
        timeout: Duration,
    ) -> Result<()> {
        let sel = serde_json::to_string(selector.unwrap_or("body")).unwrap_or_default();
        let needle = serde_json::to_string(text).unwrap_or_default();
        let js = format!(
            "return ((document.querySelector({sel}) || {{}}).innerText || '').includes({needle});"
        );
        self.wait_eval(&js, timeout).map(|_| ())
    }

    /// Stop the app: kill the recorded pid tree, prove the port closed, and check
    /// we did not change the frontmost app.
    pub fn stop(&mut self) -> Result<StopReport> {
        if self.stopped {
            return err("control session already stopped");
        }
        self.stopped = true;
        if let Some(mut c) = self.child.take() {
            let _ = c.terminate_tree();
        }
        kill_tree(self.pid);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && (is_alive(self.pid) || endpoint_open(&self.endpoint)) {
            sleep_ms(50);
        }
        let _ = fs::copy(&self.raw_log, &self.log_path);
        let _ = fs::remove_file(&self.raw_log);
        let after = frontmost();
        let (steady, states, owned_never_frontmost, owned_frontmost_pids) = self
            .focus
            .take()
            .map(|mut f| f.finish())
            .unwrap_or((false, vec![], false, vec![]));
        let process_gone = !is_alive(self.pid);
        let endpoint_closed = self.endpoint.is_empty() || !endpoint_open(&self.endpoint);
        if process_gone {
            self.tracker.forget(self.pid);
        }
        Ok(StopReport {
            pid: self.pid,
            endpoint_closed,
            process_gone,
            frontmost_unchanged: steady
                && self.frontmost_before.is_some()
                && self.frontmost_before == after,
            frontmost_states: states,
            frontmost_before: self.frontmost_before.clone(),
            frontmost_after: after,
            owned_never_frontmost,
            owned_frontmost_pids,
        })
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stop();
        }
    }
}
