//! Android journeys: an `adb`-backed launcher for an emulator or a USB device (feature `android`).
//!
//! One `AndroidSession` installs the app's dev APK, starts it, records
//! logcat for the app's pid into the run directory, optionally forwards the app's in-app control
//! server (dev builds only) to a loopback port, and at teardown force-stops the package (and removes it
//! only when this session installed it fresh and the config asks). Every `adb` call is bounded (an overrunning call is killed, the same rule as the
//! kill helpers in [`crate::process`], #134), so a stalled `adb` is a failed step, never a hang.
//!
//! ## Isolation
//!
//! * The device is chosen by serial and the serial is required: there is no "the only attached
//!   device" shortcut, so a developer's personal phone is never picked by accident. Every call
//!   after `adb devices` is `adb -s <serial> ...`.
//! * The package must be a `TestPackage`: a valid package name whose last segment is `dev`, `qa`,
//!   `test` or `debug` (or a segment the caller names with `TestPackage::with_segments`). Install,
//!   `am start`, force-stop, uninstall, `run-as` and the pid lookup are only ever built for that one
//!   name, so no command of this module can name another package.
//! * `run-as` (the app's private data dir) works only for a debuggable build, and the control
//!   forward is refused unless a `run-as` probe succeeds first. A release build cannot be reached
//!   through either.
//! * Teardown uninstalls the package only when this session installed it, and removes only the
//!   forward it created. Logcat is read filtered by pid (`--pid`) and never cleared (`logcat -c`
//!   is device-wide).
//!
//! The command builders (`args_*`) and the output parsers (`parse_*`) are pure and tested against
//! recorded `adb` output; the session logic is tested through the `AdbRunner` seam with a
//! scripted runner. No real `adb` and no device were used to write them.

use crate::process::{output_bounded, Tracker};
use crate::util::{err, Error, Result};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Longest an ordinary `adb` call (`shell`, `forward`, `uninstall`, `devices`) may run.
pub const ADB_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest `adb install` may run (a large APK over USB 2).
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(180);
/// Longest `am start -W` may wait for the activity.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait for the app's process to appear after `am start`.
pub const PID_WAIT: Duration = Duration::from_secs(15);
/// Longest each teardown call may run; teardown never takes more than a few of these in total.
pub const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for a killed `adb logcat` to be reaped.
const LOGCAT_REAP: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(250);

/// Last package-name segments that mark a package as a test build.
pub const TEST_PACKAGE_SEGMENTS: [&str; 4] = ["dev", "qa", "test", "debug"];

// ---------------------------------------------------------------------------------------------
// adb path

/// Find `adb`: `$ANDROID_HOME/platform-tools/adb`, then `$ANDROID_SDK_ROOT/platform-tools/adb`
/// (`adb.exe` on Windows). A root that is unset, empty, relative or lacks the file is skipped and
/// named in the error. `env` and `exists` are injected so the lookup is testable anywhere.
pub fn resolve_adb(
    env: &dyn Fn(&str) -> Option<OsString>,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<PathBuf> {
    let exe = if cfg!(windows) { "adb.exe" } else { "adb" };
    let mut tried: Vec<String> = Vec::new();
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        let Some(value) = env(var).filter(|v| !v.is_empty()) else {
            tried.push(format!("{var} is not set"));
            continue;
        };
        let root = PathBuf::from(value);
        if !root.is_absolute() {
            tried.push(format!("{var} is not an absolute path"));
            continue;
        }
        let adb = root.join("platform-tools").join(exe);
        if exists(&adb) {
            return Ok(adb);
        }
        tried.push(format!("{var}: {} does not exist", adb.display()));
    }
    err(format!(
        "adb not found ({}); install the Android SDK platform-tools and set ANDROID_HOME",
        tried.join("; ")
    ))
}

/// [`resolve_adb`] over this process's environment and file system.
pub fn resolve_adb_from_env() -> Result<PathBuf> {
    resolve_adb(&|k| std::env::var_os(k), &|p| p.is_file())
}

// ---------------------------------------------------------------------------------------------
// device list and selection

/// One row of `adb devices -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    /// `device` (ready), `offline`, `unauthorized`, `no` (permissions), ...
    pub state: String,
    /// The `key:value` columns (`model`, `product`, `device`, `usb`, `transport_id`).
    pub props: BTreeMap<String, String>,
}

impl Device {
    /// An emulator (`emulator-5554`) rather than a USB or network device.
    pub fn is_emulator(&self) -> bool {
        self.serial.starts_with("emulator-")
    }

    /// Authorized and booted enough for `adb` to talk to it.
    pub fn is_ready(&self) -> bool {
        self.state == "device"
    }
}

/// Parse `adb devices -l`. The header, daemon start-up lines (`* daemon ...`) and blank lines are
/// skipped.
pub fn parse_devices(output: &str) -> Vec<Device> {
    let mut devices = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('*') || line.starts_with("List of devices") {
            continue;
        }
        let mut cols = line.split_whitespace();
        let (Some(serial), Some(state)) = (cols.next(), cols.next()) else {
            continue;
        };
        let props = cols
            .filter_map(|c| c.split_once(':'))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        devices.push(Device {
            serial: serial.to_string(),
            state: state.to_string(),
            props,
        });
    }
    devices
}

fn valid_serial(serial: &str) -> bool {
    !serial.is_empty()
        && serial.len() <= 128
        && serial
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// The device named by `serial`, which must be attached and ready. An absent, offline or
/// unauthorized device is an error that lists what is attached; nothing is ever chosen for the
/// caller.
pub fn select_device(devices: &[Device], serial: &str) -> Result<Device> {
    if !valid_serial(serial) {
        return err(format!("not a device serial: {serial:?}"));
    }
    let attached = || {
        if devices.is_empty() {
            "none attached".to_string()
        } else {
            devices
                .iter()
                .map(|d| format!("{} ({})", d.serial, d.state))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    match devices.iter().find(|d| d.serial == serial) {
        None => err(format!(
            "device {serial} is not attached; attached: {}",
            attached()
        )),
        Some(d) if !d.is_ready() => err(format!(
            "device {serial} is {}, not ready (unauthorized: accept the USB debugging prompt; \
             offline: reconnect or restart the emulator)",
            d.state
        )),
        Some(d) => Ok(d.clone()),
    }
}

// ---------------------------------------------------------------------------------------------
// package, extras, endpoints

/// A package this module may touch: a valid Android package name that is marked as a test build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestPackage(String);

impl TestPackage {
    /// Valid name whose last segment is one of [`TEST_PACKAGE_SEGMENTS`]
    /// (`com.orthiclabs.heardright.dev`).
    pub fn new(name: &str) -> Result<TestPackage> {
        TestPackage::with_segments(name, &TEST_PACKAGE_SEGMENTS)
    }

    /// Valid name whose last segment is one of `segments` (an app whose dev id ends differently).
    pub fn with_segments(name: &str, segments: &[&str]) -> Result<TestPackage> {
        let parts: Vec<&str> = name.split('.').collect();
        let part_ok = |p: &&str| {
            let mut cs = p.chars();
            cs.next().is_some_and(|c| c.is_ascii_alphabetic())
                && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        if parts.len() < 2 || !parts.iter().all(part_ok) {
            return err(format!("not an Android package name: {name:?}"));
        }
        let last = parts[parts.len() - 1];
        if !segments.iter().any(|s| s.eq_ignore_ascii_case(last)) {
            return err(format!(
                "refusing package {name}: only a test build may be installed, started or removed \
                 (its last segment must be one of {})",
                segments.join(", ")
            ));
        }
        Ok(TestPackage(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One `am start` extra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extra {
    Bool(bool),
    Int(i64),
    Str(String),
}

fn valid_extra_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// POSIX single-quote `s`. `adb shell` joins its arguments into one string for the device shell,
/// so every value that is not a fixed word is quoted.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r#"'\''"#))
}

/// Where the in-app control server listens on the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteEndpoint {
    /// `tcp:<port>` on the device loopback.
    Tcp(u16),
    /// `localabstract:<name>`, an abstract unix socket.
    LocalAbstract(String),
}

impl RemoteEndpoint {
    /// The `adb forward` spec.
    pub fn spec(&self) -> Result<String> {
        match self {
            RemoteEndpoint::Tcp(0) => err("remote tcp port 0 is not an endpoint"),
            RemoteEndpoint::Tcp(p) => Ok(format!("tcp:{p}")),
            RemoteEndpoint::LocalAbstract(n) => {
                if !n.is_empty()
                    && n.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                {
                    Ok(format!("localabstract:{n}"))
                } else {
                    err(format!("not a socket name: {n:?}"))
                }
            }
        }
    }
}

/// What to run.
#[derive(Debug, Clone)]
pub struct AndroidConfig {
    /// Device serial (`emulator-5554`, a USB serial or `host:port`). Required.
    pub serial: String,
    pub package: TestPackage,
    /// The test APK to install.
    pub apk: PathBuf,
    /// Activity class: `.MainActivity` or fully qualified.
    pub activity: String,
    /// Launch extras; empty by default (there is no test mode, rightkit#1437).
    pub extras: Vec<(String, Extra)>,
    /// `install -g`: grant every runtime permission the manifest requests (microphone).
    pub grant_permissions: bool,
    /// Uninstall the package at teardown, only when this session installed it fresh (the package was
    /// not on the device before). Never when it was already installed: an uninstall deletes the app's
    /// data, which is the owner's real data.
    pub uninstall_at_teardown: bool,
    /// The in-app control server to forward, for a dev build that serves one.
    pub control: Option<RemoteEndpoint>,
    /// `adb` to use; `None` resolves it from `ANDROID_HOME` / `ANDROID_SDK_ROOT`.
    pub adb: Option<PathBuf>,
}

impl AndroidConfig {
    pub fn new(
        serial: &str,
        package: TestPackage,
        apk: impl Into<PathBuf>,
        activity: &str,
    ) -> Result<AndroidConfig> {
        if !valid_serial(serial) {
            return err(format!("not a device serial: {serial:?}"));
        }
        let ok = !activity.is_empty()
            && activity
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '$'));
        if !ok {
            return err(format!("not an activity class name: {activity:?}"));
        }
        Ok(AndroidConfig {
            serial: serial.to_string(),
            package,
            apk: apk.into(),
            activity: activity.to_string(),
            extras: vec![],
            grant_permissions: true,
            // Uninstalling deletes the app's data, which is the owner's real data (rightkit#1437).
            uninstall_at_teardown: false,
            control: None,
            adb: None,
        })
    }

    /// Add a launch extra.
    pub fn extra(mut self, key: &str, value: Extra) -> Result<AndroidConfig> {
        if !valid_extra_key(key) {
            return err(format!("not an extra key: {key:?}"));
        }
        self.extras.push((key.to_string(), value));
        Ok(self)
    }

    /// Forward this in-app control endpoint to a local loopback port (dev builds only).
    pub fn control(mut self, remote: RemoteEndpoint) -> AndroidConfig {
        self.control = Some(remote);
        self
    }
}

// ---------------------------------------------------------------------------------------------
// command builders (arguments after `adb -s <serial>`)

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

pub fn args_devices() -> Vec<String> {
    s(&["devices", "-l"])
}

pub fn args_install(apk: &Path, grant_permissions: bool) -> Vec<String> {
    let mut a = s(&["install", "-r", "-t"]);
    if grant_permissions {
        a.push("-g".to_string());
    }
    a.push(apk.to_string_lossy().into_owned());
    a
}

pub fn args_uninstall(package: &TestPackage) -> Vec<String> {
    vec!["uninstall".to_string(), package.as_str().to_string()]
}

pub fn args_list_package(package: &TestPackage) -> Vec<String> {
    let mut a = s(&["shell", "pm", "list", "packages"]);
    a.push(package.as_str().to_string());
    a
}

/// `am start -W` of `<package>/<activity>` with the launcher intent and the extras.
pub fn args_start(
    package: &TestPackage,
    activity: &str,
    extras: &[(String, Extra)],
) -> Vec<String> {
    let component = format!("{}/{}", package.as_str(), activity);
    let mut a = s(&["shell", "am", "start", "-W", "-n"]);
    a.push(sh_quote(&component));
    a.extend(s(&[
        "-a",
        "android.intent.action.MAIN",
        "-c",
        "android.intent.category.LAUNCHER",
    ]));
    for (key, value) in extras {
        match value {
            Extra::Bool(b) => a.extend(["--ez".to_string(), sh_quote(key), b.to_string()]),
            Extra::Int(i) => a.extend(["--ei".to_string(), sh_quote(key), i.to_string()]),
            Extra::Str(v) => a.extend(["--es".to_string(), sh_quote(key), sh_quote(v)]),
        }
    }
    a
}

pub fn args_force_stop(package: &TestPackage) -> Vec<String> {
    let mut a = s(&["shell", "am", "force-stop"]);
    a.push(package.as_str().to_string());
    a
}

pub fn args_pidof(package: &TestPackage) -> Vec<String> {
    let mut a = s(&["shell", "pidof"]);
    a.push(package.as_str().to_string());
    a
}

/// Logcat for one pid, with the thread-time format. No `-c`: the buffer is shared.
pub fn args_logcat(pid: u32) -> Vec<String> {
    vec![
        "logcat".to_string(),
        format!("--pid={pid}"),
        "-v".to_string(),
        "threadtime".to_string(),
    ]
}

/// Forward a free local tcp port (`tcp:0`; adb prints the one it chose) to `remote`.
pub fn args_forward(remote: &RemoteEndpoint) -> Result<Vec<String>> {
    Ok(vec![
        "forward".to_string(),
        "tcp:0".to_string(),
        remote.spec()?,
    ])
}

pub fn args_forward_remove(local_port: u16) -> Vec<String> {
    vec![
        "forward".to_string(),
        "--remove".to_string(),
        format!("tcp:{local_port}"),
    ]
}

/// `run-as <package> id`: succeeds only for a debuggable build of an installed package.
pub fn args_run_as_probe(package: &TestPackage) -> Vec<String> {
    let mut a = s(&["shell", "run-as"]);
    a.push(package.as_str().to_string());
    a.push("id".to_string());
    a
}

/// Read one file of the app's private data dir (relative to it) as raw bytes.
pub fn args_run_as_cat(package: &TestPackage, relative: &str) -> Result<Vec<String>> {
    let path = Path::new(relative);
    let bad = relative.is_empty()
        || relative.contains('\0')
        || relative.starts_with('/')
        || relative.starts_with('\\')
        || path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)));
    if bad {
        return err(format!(
            "not a path inside the app data dir: {relative:?} (relative, no `..`)"
        ));
    }
    let mut a = s(&["exec-out", "run-as"]);
    a.push(package.as_str().to_string());
    a.push("cat".to_string());
    a.push(sh_quote(relative));
    Ok(a)
}

// ---------------------------------------------------------------------------------------------
// output parsers

/// `adb install`: `Ok` on `Success`, else the `Failure [REASON]`.
pub fn parse_install(output: &str) -> Result<()> {
    if let Some(at) = output.find("Failure [") {
        let rest = &output[at + "Failure [".len()..];
        let reason = rest.split(']').next().unwrap_or(rest).trim();
        return err(format!("install failed: {reason}"));
    }
    if output.lines().any(|l| l.trim() == "Success") {
        return Ok(());
    }
    err(format!(
        "install did not report Success: {}",
        first_line(output)
    ))
}

/// What `am start -W` reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartOutcome {
    /// `COLD`, `WARM` or `HOT`.
    pub launch_state: Option<String>,
    pub total_ms: Option<u64>,
    pub activity: Option<String>,
    /// A `Warning:` line (for example, the task was brought to the front instead).
    pub warning: Option<String>,
}

/// `am start -W`: an `Error` line or a `Status:` other than `ok` is a failure carrying the text.
pub fn parse_start(output: &str) -> Result<StartOutcome> {
    let mut out = StartOutcome::default();
    let mut errors: Vec<String> = Vec::new();
    for line in output.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("Status:") {
            if v.trim() != "ok" {
                errors.push(format!("Status: {}", v.trim()));
            }
        } else if let Some(v) = line.strip_prefix("LaunchState:") {
            out.launch_state = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("TotalTime:") {
            out.total_ms = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("Activity:") {
            out.activity = Some(v.trim().to_string());
        } else if line.starts_with("Warning:") {
            out.warning = Some(line.to_string());
        } else if line.starts_with("Error") || line.contains("Exception") {
            errors.push(line.to_string());
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        err(format!("am start failed: {}", errors.join("; ")))
    }
}

/// `pidof` output: every pid, in order. Empty when the process is not running.
pub fn parse_pids(output: &str) -> Vec<u32> {
    output
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect()
}

/// The local port `adb forward tcp:0 ...` printed.
pub fn parse_forward_port(output: &str) -> Option<u16> {
    output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .and_then(|l| l.parse::<u16>().ok())
        .filter(|p| *p != 0)
}

/// Whether `pm list packages <name>` lists exactly `name` (it matches by substring).
pub fn parse_package_listed(output: &str, package: &TestPackage) -> bool {
    let want = format!("package:{}", package.as_str());
    output.lines().any(|l| l.trim() == want)
}

/// `run-as <pkg> id`: success prints the uid; a release build prints `package not debuggable`.
pub fn parse_run_as(output: &str) -> Result<()> {
    if output.contains("uid=") {
        Ok(())
    } else {
        err(format!(
            "run-as refused ({}): the build is not debuggable or the package is not installed",
            first_line(output)
        ))
    }
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(no output)");
    line.chars().take(200).collect()
}

// ---------------------------------------------------------------------------------------------
// runner seam

/// What one `adb` call produced.
#[derive(Debug, Clone, Default)]
pub struct AdbOutput {
    /// The process exited with status 0. Old `adb` does not carry the device command's status
    /// through `shell`, so the parsers read the text, not this.
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl AdbOutput {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Standard output then standard error, as text.
    pub fn text(&self) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&self.stdout),
            String::from_utf8_lossy(&self.stderr)
        )
    }
}

/// A running `adb logcat`.
pub trait LogcatProc: Send {
    /// Kill it and wait (bounded) for it to be reaped.
    fn stop(&mut self);
    fn id(&self) -> Option<u32>;
}

/// Runs `adb`. The real one is [`ProcessRunner`]; tests script it.
pub trait AdbRunner: Send + Sync {
    /// One bounded call. A call that overruns `timeout` is killed and is an `Err`.
    fn run(&self, args: &[String], timeout: Duration) -> Result<AdbOutput>;
    /// Start `adb <args>` with its standard output written to `out` (logcat).
    fn spawn_to_file(&self, args: &[String], out: File) -> Result<Box<dyn LogcatProc>>;
    /// One bounded call whose standard output is written to `out` (a binary stream such as
    /// `exec-out run-as <pkg> tar -cf -`), with no copy held in memory. The returned
    /// [`AdbOutput::stdout`] is empty. A runner that cannot do it says so.
    fn run_to_file(&self, args: &[String], out: File, timeout: Duration) -> Result<AdbOutput> {
        let _ = (args, out, timeout);
        err("this adb runner cannot stream a command's output to a file")
    }
    /// One bounded call whose standard input is read from `input` (a binary stream such as
    /// `shell run-as <pkg> tar -xf -`). A runner that cannot do it says so.
    fn run_from_file(&self, args: &[String], input: File, timeout: Duration) -> Result<AdbOutput> {
        let _ = (args, input, timeout);
        err("this adb runner cannot stream a file to a command's input")
    }
}

/// The real runner: one `adb` executable.
pub struct ProcessRunner {
    adb: PathBuf,
}

impl ProcessRunner {
    pub fn new(adb: PathBuf) -> ProcessRunner {
        ProcessRunner { adb }
    }
}

struct ChildLog {
    child: Child,
    done: bool,
}

impl LogcatProc for ChildLog {
    fn stop(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        let _ = self.child.kill();
        let deadline = Instant::now() + LOGCAT_REAP;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            }
        }
    }

    fn id(&self) -> Option<u32> {
        Some(self.child.id())
    }
}

impl AdbRunner for ProcessRunner {
    fn run(&self, args: &[String], timeout: Duration) -> Result<AdbOutput> {
        let mut cmd = Command::new(&self.adb);
        cmd.args(args);
        match output_bounded(&mut cmd, timeout) {
            Ok(Some(o)) => Ok(AdbOutput {
                success: o.status.success(),
                stdout: o.stdout,
                stderr: o.stderr,
            }),
            Ok(None) => err(format!(
                "adb {} did not finish within {}s and was killed",
                args.join(" "),
                timeout.as_secs()
            )),
            Err(e) => err(format!("could not run {}: {e}", self.adb.display())),
        }
    }

    fn run_to_file(&self, args: &[String], out: File, timeout: Duration) -> Result<AdbOutput> {
        run_streamed(&self.adb, args, None, Some(out), timeout)
    }

    fn run_from_file(&self, args: &[String], input: File, timeout: Duration) -> Result<AdbOutput> {
        run_streamed(&self.adb, args, Some(input), None, timeout)
    }

    fn spawn_to_file(&self, args: &[String], out: File) -> Result<Box<dyn LogcatProc>> {
        let mut cmd = Command::new(&self.adb);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: adb logcat runs in the background
        }
        let child = cmd
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error(format!("could not start {}: {e}", self.adb.display())))?;
        Ok(Box::new(ChildLog { child, done: false }))
    }
}

fn drain<R: Read + Send + 'static>(stream: Option<R>) -> Option<std::thread::JoinHandle<Vec<u8>>> {
    stream.map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    })
}

/// One `adb` call with a file as its standard input or output. Standard error (and standard
/// output when no file takes it) is read into memory. An overrunning call is killed.
fn run_streamed(
    adb: &Path,
    args: &[String],
    stdin: Option<File>,
    stdout: Option<File>,
    timeout: Duration,
) -> Result<AdbOutput> {
    let mut cmd = Command::new(adb);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.stdin(match stdin {
        Some(f) => Stdio::from(f),
        None => Stdio::null(),
    });
    cmd.stdout(match stdout {
        Some(f) => Stdio::from(f),
        None => Stdio::piped(),
    });
    cmd.stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| Error(format!("could not run {}: {e}", adb.display())))?;
    let out_h = drain(child.stdout.take());
    let err_h = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return err(format!(
                        "adb {} did not finish within {}s and was killed",
                        args.join(" "),
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return err(format!("could not wait for {}: {e}", adb.display())),
        }
    };
    Ok(AdbOutput {
        success: status.success(),
        stdout: out_h
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default(),
        stderr: err_h
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default(),
    })
}

/// `adb devices -l` through `runner`.
pub fn list_devices(runner: &dyn AdbRunner) -> Result<Vec<Device>> {
    let out = runner.run(&args_devices(), ADB_TIMEOUT)?;
    Ok(parse_devices(&out.stdout_text()))
}

// ---------------------------------------------------------------------------------------------
// session

/// A loopback port forwarded to the app's in-app control server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forward {
    pub local_port: u16,
}

impl Forward {
    /// `127.0.0.1:<port>`, what a journey connects to (loopback only, like [`crate::http`]).
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.local_port)
    }
}

/// An installed, running test build on one device. Dropping it tears it down.
pub struct AndroidSession {
    runner: Box<dyn AdbRunner>,
    cfg: AndroidConfig,
    device: Device,
    start: StartOutcome,
    pid: Option<u32>,
    logcat: Option<Box<dyn LogcatProc>>,
    logcat_pid: Option<u32>,
    logcat_path: Option<PathBuf>,
    forward: Option<Forward>,
    installed: bool,
    preinstalled: bool,
    tracker: Option<Tracker>,
    torn_down: bool,
}

impl AndroidSession {
    /// Resolve `adb`, then [`AndroidSession::start_with`] the real runner.
    pub fn start(
        cfg: AndroidConfig,
        run_dir: &Path,
        tracker: Option<&Tracker>,
    ) -> Result<AndroidSession> {
        let adb = match &cfg.adb {
            Some(p) => p.clone(),
            None => resolve_adb_from_env()?,
        };
        AndroidSession::start_with(Box::new(ProcessRunner::new(adb)), cfg, run_dir, tracker)
    }

    /// Select the device, install, launch, find the pid, start logcat into
    /// `<run_dir>/logcat.txt`, and forward the control server when configured. A failure after the
    /// install tears down what was done before returning the error.
    pub fn start_with(
        runner: Box<dyn AdbRunner>,
        cfg: AndroidConfig,
        run_dir: &Path,
        tracker: Option<&Tracker>,
    ) -> Result<AndroidSession> {
        let devices = list_devices(runner.as_ref())?;
        let device = select_device(&devices, &cfg.serial)?;
        if !cfg.apk.is_file() {
            return err(format!("APK not found: {}", cfg.apk.display()));
        }
        let mut session = AndroidSession {
            runner,
            cfg,
            device,
            start: StartOutcome::default(),
            pid: None,
            logcat: None,
            logcat_pid: None,
            logcat_path: None,
            forward: None,
            installed: false,
            preinstalled: false,
            tracker: tracker.cloned(),
            torn_down: false,
        };
        if let Err(e) = session.bring_up(run_dir) {
            let notes = session.teardown();
            return err(if notes.is_empty() {
                e.0
            } else {
                format!("{}; teardown: {}", e.0, notes.join("; "))
            });
        }
        Ok(session)
    }

    fn call(&self, args: Vec<String>, timeout: Duration) -> Result<AdbOutput> {
        let mut full = vec!["-s".to_string(), self.cfg.serial.clone()];
        full.extend(args);
        self.runner.run(&full, timeout)
    }

    fn bring_up(&mut self, run_dir: &Path) -> Result<()> {
        let listed = self.call(args_list_package(&self.cfg.package), ADB_TIMEOUT)?;
        self.preinstalled = parse_package_listed(&listed.stdout_text(), &self.cfg.package);

        let out = self.call(
            args_install(&self.cfg.apk, self.cfg.grant_permissions),
            INSTALL_TIMEOUT,
        )?;
        parse_install(&out.text())?;
        // From here the package is this session's to stop and remove; a failed install is not.
        self.installed = true;

        let out = self.call(
            args_start(&self.cfg.package, &self.cfg.activity, &self.cfg.extras),
            LAUNCH_TIMEOUT,
        )?;
        self.start = parse_start(&out.text())?;

        let pid = self.wait_for_pid(PID_WAIT)?;
        self.pid = Some(pid);

        std::fs::create_dir_all(run_dir)?;
        let path = run_dir.join("logcat.txt");
        let file = File::create(&path)?;
        let mut full = vec!["-s".to_string(), self.cfg.serial.clone()];
        full.extend(args_logcat(pid));
        let proc = self.runner.spawn_to_file(&full, file)?;
        self.logcat_pid = proc.id();
        if let (Some(t), Some(p)) = (&self.tracker, self.logcat_pid) {
            t.register(p, "adb-logcat");
        }
        self.logcat = Some(proc);
        self.logcat_path = Some(path);

        if let Some(remote) = self.cfg.control.clone() {
            // The control server exists only in a dev build, and only a debuggable build answers
            // `run-as`, so a build that is not debuggable is never forwarded.
            let probe = self.call(args_run_as_probe(&self.cfg.package), ADB_TIMEOUT)?;
            parse_run_as(&probe.text())?;
            let out = self.call(args_forward(&remote)?, ADB_TIMEOUT)?;
            let text = out.text();
            let Some(port) = parse_forward_port(&out.stdout_text()) else {
                return err(format!(
                    "adb forward gave no local port: {}",
                    first_line(&text)
                ));
            };
            self.forward = Some(Forward { local_port: port });
        }
        Ok(())
    }

    /// Poll `pidof` until the package's process exists or `within` passes. The first pid is the
    /// main process (a bare package name is the main process; its services are `pkg:name`).
    fn wait_for_pid(&self, within: Duration) -> Result<u32> {
        let deadline = Instant::now() + within;
        loop {
            let out = self.call(args_pidof(&self.cfg.package), ADB_TIMEOUT)?;
            if let Some(pid) = parse_pids(&out.stdout_text()).first() {
                return Ok(*pid);
            }
            if Instant::now() >= deadline {
                return err(format!(
                    "{} has no process {}s after am start (it crashed or exited; see logcat)",
                    self.cfg.package.as_str(),
                    within.as_secs()
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn package(&self) -> &TestPackage {
        &self.cfg.package
    }

    /// The app's main-process pid on the device.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// What `am start -W` reported (cold/warm start and the launch time).
    pub fn start_outcome(&self) -> &StartOutcome {
        &self.start
    }

    /// The file logcat is being written to.
    pub fn logcat_path(&self) -> Option<&Path> {
        self.logcat_path.as_deref()
    }

    /// The forwarded control endpoint, when the config named one.
    pub fn control_addr(&self) -> Option<SocketAddr> {
        self.forward.map(|f| f.addr())
    }

    /// Whether the package was already on the device before this session installed over it.
    pub fn was_preinstalled(&self) -> bool {
        self.preinstalled
    }

    /// Read one file of the app's private data dir (relative path, no `..`) through `run-as`. Only
    /// this session's test package, only a debuggable build.
    pub fn read_app_file(&self, relative: &str) -> Result<Vec<u8>> {
        let args = args_run_as_cat(&self.cfg.package, relative)?;
        let out = self.call(args, ADB_TIMEOUT)?;
        if out.stdout.is_empty() {
            let text = out.text();
            if text.contains("run-as:") || text.contains("No such file") {
                return err(format!("read {relative}: {}", first_line(&text)));
            }
        }
        Ok(out.stdout)
    }

    /// Tear down, once: force-stop, stop logcat, drop the forward, uninstall (when configured and
    /// this session installed it). Every step runs even if an earlier one failed; the returned
    /// notes name the failures. Also run by `Drop`.
    pub fn teardown(&mut self) -> Vec<String> {
        if self.torn_down {
            return Vec::new();
        }
        self.torn_down = true;
        let mut notes = Vec::new();
        let package = self.cfg.package.clone();
        if self.installed {
            if let Err(e) = self.call(args_force_stop(&package), TEARDOWN_TIMEOUT) {
                notes.push(format!("force-stop: {e}"));
            }
        }
        if let Some(mut log) = self.logcat.take() {
            log.stop();
            if let (Some(t), Some(p)) = (&self.tracker, self.logcat_pid) {
                t.forget(p);
            }
        }
        if let Some(f) = self.forward.take() {
            if let Err(e) = self.call(args_forward_remove(f.local_port), TEARDOWN_TIMEOUT) {
                notes.push(format!("forward --remove: {e}"));
            }
        }
        // Never over a package that was on the device before this session: its data is the owner's.
        if self.installed && !self.preinstalled && self.cfg.uninstall_at_teardown {
            match self.call(args_uninstall(&package), TEARDOWN_TIMEOUT) {
                Ok(out) => {
                    if !out.text().contains("Success") {
                        notes.push(format!("uninstall: {}", first_line(&out.text())));
                    }
                }
                Err(e) => notes.push(format!("uninstall: {e}")),
            }
        }
        notes
    }
}

impl Drop for AndroidSession {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}
