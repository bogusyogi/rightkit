//! Claude Desktop account switcher: every account in Claude's one default data folder, one
//! signed in at a time.
//!
//! A switch quits that folder's Claude, saves the signed-in account's login into
//! `<the host app state>/claude-desktop-logins/<slot>`, puts the target's login back, merges the chats
//! across account folders and starts the same Claude with the account's real home and no
//! `CLAUDE_CONFIG_DIR` (Adrian 2026-10-19: "point claude a to config a and config b? so the chats
//! stay as is and only the credentials change?", #425). A named account is first signed in on its
//! own folder under `<the host app state>/claude-desktop/<name>` started with `--user-data-dir`; a switch
//! reads that folder's login once and never starts it again. Instances whose arguments or
//! environment point under `ClaudeProfiles` (Claude B) are never touched by a switch and are only
//! quit and restarted, with their exact launch arguments and environment, around the one-time
//! sign-in of a new account. Pipeline and rules: docs/claude-account-switch.md, "Desktop switcher".
//!
//! A login is moved as opaque bytes and is never decrypted, printed, logged or sent: the
//! `config.json` keys in `LOGIN_KEYS` and the files in `LOGIN_FILES`. The only value the host app
//! reads is `lastKnownAccountUuid`, and nothing is written under `ClaudeProfiles`. A swap is one
//! recoverable operation (`swap_login`): an unreadable file stops it, a target login that is not
//! whole is refused, and a failure or crash part way is rolled back to the journalled snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", windows))]
use std::process::Command;
use std::time::{Duration, Instant};

use rightkit_fs::{FileLock, LockKind, LockOptions, ensure_private_dir};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::cli_accounts::valid_name;
use crate::sync;

/// A failure: message and a stable machine code.
pub type Fail = (String, &'static str);

fn fail<T>(message: impl Into<String>, code: &'static str) -> Result<T, Fail> {
    Err((message.into(), code))
}

/// How long a quit may take before the switch gives up. Windows Claude hides to the tray on
/// `WM_CLOSE` instead of exiting, so there the wait is short and the leftover instance is ended.
/// On the Mac, Claude asks "Quit Claude?" while chats run, so the wait leaves time to answer it (#425).
const QUIT_WAIT: Duration = if cfg!(windows) {
    Duration::from_secs(10)
} else {
    Duration::from_secs(120)
};
/// Backups of switched-into folders kept.
const KEEP_SWITCH_BACKUPS: usize = 10;
/// Total size the switch backups may take (2 GB, as the sync backups); past it the oldest go.
/// One switch copies up to about 1 GB, so this keeps about two at the Dell's largest size.
const SWITCH_BACKUPS_MAX_BYTES: u64 = sync::BACKUPS_MAX_BYTES;
/// The environment kept for a restart of an instance that was quit around a sign-in.
const KEPT_ENV: [&str; 7] = [
    "HOME",
    "CFFIXED_USER_HOME",
    "CLAUDE_CONFIG_DIR",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_CONFIG_HOME",
];
/// The `config.json` keys that hold a Claude Desktop sign-in (encrypted by Claude) and its account.
const LOGIN_KEYS: [&str; 3] = [
    "oauth:tokenCache",
    "oauth:tokenCacheV2",
    "lastKnownAccountUuid",
];
/// The files that hold the claude.ai session cookie, and on Windows the key Claude encrypts the
/// cookie and tokens with (`Local State`).
const LOGIN_FILES: [&str; 3] = ["Cookies", "Cookies-journal", "Local State"];

// ---------------------------------------------------------------- locations

/// The account's real home: the password database on Unix (a shell inside Claude B has `HOME`
/// pointed at its profile), `USERPROFILE` on Windows.
fn real_home() -> PathBuf {
    rightkit_app_host::account_home()
}

/// The host app's state folder for this platform.
pub(crate) fn state_base() -> Result<PathBuf, Fail> {
    // The host's state root resolves through its profile-aware environment: a Claude B shell's LOCALAPPDATA points
    // into ClaudeProfiles (#420).
    #[cfg(any(target_os = "macos", windows))]
    let base = rightkit_app_host::state_root();
    #[cfg(not(any(target_os = "macos", windows)))]
    let base: Option<PathBuf> = None;
    match base {
        Some(base) => Ok(base),
        None => fail(
            "Claude Desktop is not supported on this platform",
            "unsupported",
        ),
    }
}

fn accounts_dir() -> Result<PathBuf, Fail> {
    Ok(state_base()?.join("claude-desktop"))
}

fn registry_file() -> Result<PathBuf, Fail> {
    Ok(state_base()?.join("claude-desktop-accounts.json"))
}

fn pending_file() -> Result<PathBuf, Fail> {
    Ok(state_base()?.join("claude-desktop-pending.json"))
}

fn backups_dir() -> Result<PathBuf, Fail> {
    #[cfg(target_os = "macos")]
    {
        Ok(state_base()?.join("claude-sync-backups"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        sync::default_backups().map_err(|e| (e.to_string(), "unsupported"))
    }
}

/// Saved logins, one folder per slot.
fn logins_dir() -> Result<PathBuf, Fail> {
    Ok(state_base()?.join("claude-desktop-logins"))
}

/// Claude's default data folder.
fn main_root() -> Result<PathBuf, Fail> {
    #[cfg(target_os = "macos")]
    let root = Some(
        real_home()
            .join("Library")
            .join("Application Support")
            .join("Claude"),
    );
    #[cfg(windows)]
    // `home::env_var`, not the raw variable: from a Claude B shell APPDATA points into
    // ClaudeProfiles, and B's folder must never be taken for Claude's own (#368).
    let root = rightkit_app_host::env_var("APPDATA").map(|a| PathBuf::from(a).join("Claude"));
    #[cfg(not(any(target_os = "macos", windows)))]
    let root: Option<PathBuf> = None;
    match root {
        Some(root) => Ok(root),
        None => fail(
            "Claude Desktop is not supported on this platform",
            "unsupported",
        ),
    }
}

// ----------------------------------------------------------------- registry

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Account {
    name: String,
    /// The account signed in at this folder (`lastKnownAccountUuid`), when known.
    #[serde(default)]
    account_uuid: Option<String>,
    #[serde(default)]
    added_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    accounts: Vec<Account>,
}

fn load_registry() -> Registry {
    registry_file()
        .ok()
        .and_then(|p| fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Replaces `path` atomically: owner-only from creation on Unix (Windows keeps the parent's
/// inherited access), flushed to disk when `durable`.
pub(crate) fn write_private(path: &Path, bytes: &[u8], durable: bool) -> std::io::Result<()> {
    rightkit_fs::atomic_write_with_options(
        path,
        rightkit_fs::AtomicWriteOptions {
            durable,
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| std::io::Write::write_all(file, bytes),
    )
    .map_err(std::io::Error::from)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Fail> {
    if let Some(dir) = path.parent() {
        ensure_private_dir(dir).map_err(|e| (e.to_string(), "io"))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| (e.to_string(), "io"))?;
    write_private(path, &bytes, true).map_err(|e| (format!("{}: {e}", path.display()), "io"))
}

fn save_registry(registry: &Registry) -> Result<(), Fail> {
    write_json(&registry_file()?, registry)
}

/// One slot: "main" or a named account folder.
#[derive(Clone, Debug)]
struct Slot {
    name: String,
    dir: PathBuf,
}

/// "main" first, then every named folder on disk or in the registry.
fn slots(registry: &Registry) -> Result<Vec<Slot>, Fail> {
    let mut out = vec![Slot {
        name: "main".into(),
        dir: main_root()?,
    }];
    let base = accounts_dir()?;
    let mut names: BTreeSet<String> = registry.accounts.iter().map(|a| a.name.clone()).collect();
    if let Ok(entries) = fs::read_dir(&base) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if is_dir && valid_name(&name) {
                names.insert(name);
            }
        }
    }
    for name in names {
        if name == "main" {
            continue;
        }
        let dir = base.join(&name);
        out.push(Slot { name, dir });
    }
    Ok(out)
}

fn short(uuid: &str) -> String {
    uuid.split('-').next().unwrap_or(uuid).to_string()
}

// ---------------------------------------------------------------- instances

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    /// The main slot's or a named account's instance.
    Slot(String),
    /// Under `ClaudeProfiles`: never touched by a switch.
    Protected,
    /// A `--user-data-dir` that is not one of the host app's folders: not ours.
    Foreign,
    /// Its environment could not be read, so it cannot be told from the main instance.
    Unknown,
}

#[derive(Clone, Debug)]
struct Instance {
    pid: u32,
    start_time: u64,
    kind: Kind,
    exe: PathBuf,
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

fn text(os: &OsString) -> String {
    os.to_string_lossy().into_owned()
}

fn is_claude_main(exe: &Path, cmd: &[OsString]) -> bool {
    let name = exe
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let named = if cfg!(windows) {
        name == "claude.exe"
    } else {
        name == "claude"
    };
    named && sync::is_claude_exe(&exe.to_string_lossy()) && !sync::is_helper(cmd)
}

/// Every running Claude Desktop main process, classified by its own arguments and environment.
fn instances(slots: &[Slot]) -> Vec<Instance> {
    instances_with(slots, true)
}

/// `instances` without reading any process environment (kinds read `Unknown`): for the account
/// watch's periodic look, which needs only the command lines.
fn instances_light(slots: &[Slot]) -> Vec<Instance> {
    instances_with(slots, false)
}

fn instances_with(slots: &[Slot], environ: bool) -> Vec<Instance> {
    let Ok(mut system) = System::new() else {
        return Vec::new();
    };
    let mut kind = ProcessRefreshKind::nothing()
        .with_cmd(UpdateKind::Always)
        .with_exe(UpdateKind::Always);
    if environ {
        kind = kind.with_environ(UpdateKind::Always);
    }
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    let keys: Vec<(String, String)> = slots
        .iter()
        .map(|s| (s.name.clone(), sync::data_dir_key(&s.dir)))
        .collect();
    let mut out = Vec::new();
    for (pid, p) in system.processes() {
        let Some(exe) = p.exe() else { continue };
        if !is_claude_main(exe, p.cmd()) {
            continue;
        }
        // A helper whose arguments cannot be read (another session's, #426) still has a Claude for
        // its parent; it is not a main instance and must not make the switch refuse.
        let parent_is_claude = p
            .parent()
            .and_then(|pp| system.process(pp))
            .and_then(|pp| pp.exe())
            .is_some_and(|pe| pe.file_name() == exe.file_name());
        if parent_is_claude {
            continue;
        }
        let args: Vec<String> = p.cmd().iter().skip(1).map(text).collect();
        let mut env = BTreeMap::new();
        for var in p.environ() {
            let var = text(var);
            if let Some((k, v)) = var.split_once('=')
                && KEPT_ENV.contains(&k)
            {
                env.insert(k.to_string(), v.to_string());
            }
        }
        let env_known = !p.environ().is_empty();
        let under_profiles = args.iter().any(|a| a.contains("ClaudeProfiles"))
            || env.values().any(|v| v.contains("ClaudeProfiles"));
        let data_dir = args
            .iter()
            .find_map(|a| a.strip_prefix(sync::USER_DATA_DIR))
            .map(|d| sync::data_dir_key(Path::new(d.trim_matches('"'))));
        let kind = if under_profiles {
            Kind::Protected
        } else if !env_known {
            Kind::Unknown
        } else if let Some(dir) = data_dir {
            match keys.iter().find(|(_, key)| *key == dir) {
                Some((name, _)) => Kind::Slot(name.clone()),
                None => Kind::Foreign,
            }
        } else {
            Kind::Slot("main".into())
        };
        out.push(Instance {
            pid: pid.as_u32(),
            start_time: p.start_time(),
            kind,
            exe: exe.to_path_buf(),
            args,
            env,
        });
    }
    out.sort_by_key(|i| i.pid);
    out
}

fn alive(pid: u32, start_time: u64) -> bool {
    let Ok(mut system) = System::new() else {
        return false;
    };
    let pid = Pid::from_u32(pid);
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .is_some_and(|p| p.start_time() == start_time)
}

/// Runs a program with no environment or working directory of its own. A command that sets
/// either goes through `run_quiet_env`.
#[cfg(any(target_os = "macos", windows))]
fn run_quiet(program: &str, args: &[&str]) -> Result<bool, String> {
    let options = rightkit_child_process::RunOptions::new(Duration::from_secs(20))
        .stdout_cap(0)
        .stderr_cap(4096);
    match rightkit_child_process::run(program, args, &options) {
        Ok(done) if done.timed_out => Err("the command did not finish".into()),
        Ok(done) => Ok(done.status.success()),
        Err(e) => Err(e.to_string()),
    }
}

/// `run_quiet` for a command that carries environment changes: RightKit's `run_command_capture`
/// takes the configured command as given.
#[cfg(target_os = "macos")]
fn run_quiet_env(command: Command) -> Result<bool, String> {
    let options = rightkit_child_process::CaptureOptions::new()
        .timeout(Duration::from_secs(20))
        .stdout_cap(0)
        .stderr_cap(4096)
        .drain_grace(Duration::from_millis(200));
    let owned = rightkit_child_process::OwnedCommand::from_command(command);
    match rightkit_child_process::run_command_capture(owned, None, &options) {
        Ok(done) if done.timed_out => Err("the command did not finish".into()),
        Ok(done) => Ok(done.status.success()),
        Err(e)
            if rightkit_child_process::RunnerFailure::from_io(&e)
                .is_some_and(|f| f.phase() == rightkit_child_process::RunnerPhase::Spawn) =>
        {
            Err(e.to_string())
        }
        Err(_) => Err("the command did not finish".into()),
    }
}

/// Ask the instance to quit the way Quit does. Never forces.
fn request_quit(pid: u32) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "ObjC.import('AppKit'); $.NSRunningApplication.runningApplicationWithProcessIdentifier({pid}).terminate"
        );
        run_quiet("/usr/bin/osascript", &["-l", "JavaScript", "-e", &script]).map(|_| ())
    }
    #[cfg(windows)]
    {
        // Without /F, taskkill posts WM_CLOSE to the process's top windows. RightKit's
        // `AdoptedSignal::Graceful` is `TerminateProcess` on Windows, so it cannot ask this way.
        let pid = pid.to_string();
        run_quiet("taskkill", &["/PID", &pid]).map(|_| ())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = pid;
        Err("unsupported".into())
    }
}

/// Force the instance to end (on Windows with everything it started), only while the pid still
/// names the process that was read: RightKit's owner captures the pid and re-checks its identity
/// inside the signal, and the start time is read again around the capture.
#[cfg(any(unix, windows))]
fn force_quit(pid: u32, start_time: u64) {
    use rightkit_child_process::AdoptedIdentity;
    let Some(pid) = std::num::NonZeroU32::new(pid) else {
        return;
    };
    let Ok(Some(target)) = AdoptedIdentity::capture(pid) else {
        return;
    };
    if !alive(pid.get(), start_time) {
        return;
    }
    #[cfg(unix)]
    {
        let _ = target.signal(rightkit_child_process::AdoptedSignal::Force);
    }
    #[cfg(windows)]
    {
        let mut targets = target.descendants().unwrap_or_default();
        targets.reverse();
        targets.push(target);
        let _ = rightkit_child_process::sweep(&targets, Duration::ZERO);
    }
}

/// Quit these instances and wait for each to exit. `force` kills what is left after the wait.
/// Returns the ones still running.
fn quit_all(targets: &[Instance], force: bool) -> Vec<Instance> {
    // Windows: `taskkill /PID` first, then `/F` on that instance only, after the wait.
    let force = force || cfg!(windows);
    for t in targets {
        let _ = request_quit(t.pid);
    }
    let start = Instant::now();
    let mut left: Vec<Instance> = targets.to_vec();
    while !left.is_empty() && start.elapsed() < QUIT_WAIT {
        left.retain(|t| alive(t.pid, t.start_time));
        if !left.is_empty() {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    if force && !left.is_empty() {
        for t in &left {
            force_quit(t.pid, t.start_time);
        }
        std::thread::sleep(Duration::from_secs(2));
        left.retain(|t| alive(t.pid, t.start_time));
    }
    left
}

// ------------------------------------------------------------------- launch

/// What to start. `env` is applied over the inherited environment; `CLAUDE_CONFIG_DIR` is
/// removed unless `env` carries it.
struct Launch {
    exe: Option<PathBuf>,
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

impl Launch {
    /// A fresh start of a slot: the account's real home, no `CLAUDE_CONFIG_DIR`.
    fn slot(slot: &Slot) -> Self {
        let mut env = BTreeMap::new();
        let home = real_home().to_string_lossy().into_owned();
        #[cfg(windows)]
        {
            env.insert("USERPROFILE".into(), home);
            for key in ["APPDATA", "LOCALAPPDATA"] {
                if let Some(v) = std::env::var_os(key) {
                    env.insert(key.into(), v.to_string_lossy().into_owned());
                }
            }
        }
        #[cfg(not(windows))]
        env.insert("HOME".into(), home);
        let args = if slot.name == "main" {
            Vec::new()
        } else {
            vec![format!("{}{}", sync::USER_DATA_DIR, slot.dir.display())]
        };
        Self {
            exe: None,
            args,
            env,
        }
    }

    fn restore(instance: &Instance) -> Self {
        Self {
            exe: Some(instance.exe.clone()),
            args: instance.args.clone(),
            env: instance.env.clone(),
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    fn apply_env(&self, command: &mut Command) {
        command.env_remove("CLAUDE_CONFIG_DIR");
        // Foundation finds an app's folders through CFFIXED_USER_HOME before HOME: inherited from a
        // Claude profile's shell it put account A on Claude B's data folder (2026-10-19, #425).
        command.env_remove("CFFIXED_USER_HOME");
        for (k, v) in &self.env {
            command.env(k, v);
        }
        // A restored instance keeps the HOME it was quit with, which may be a profile's (#364).
        #[cfg(unix)]
        command.env("HOME", real_home());
    }

    #[cfg(target_os = "macos")]
    fn start(&self) -> Result<(), String> {
        let bundle = self
            .exe
            .as_ref()
            .and_then(|e| {
                e.ancestors()
                    .find(|a| a.extension().is_some_and(|x| x == "app"))
            })
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/Applications/Claude.app"));
        // Starts a new instance with the account's environment, which `launch_app` cannot.
        let mut command = rightkit_child_process::command_std("/usr/bin/open");
        command.arg("-n").arg(&bundle);
        if !self.args.is_empty() {
            command.arg("--args").args(&self.args);
        }
        self.apply_env(&mut command);
        if run_quiet_env(command)? {
            Ok(())
        } else {
            Err(format!("could not open {}", bundle.display()))
        }
    }

    #[cfg(windows)]
    fn start(&self) -> Result<(), String> {
        // The main slot: the Microsoft Store app while it is installed, else the website install
        // with no `--user-data-dir`.
        if self.exe.is_none()
            && self.args.is_empty()
            && let Some(family) = store_family()
        {
            return rightkit_child_process::launch_app(&format!("{family}!Claude"))
                .map_err(|e| e.to_string());
        }
        let exe = match &self.exe {
            Some(exe) if exe.is_file() => exe.clone(),
            _ => windows_exe().ok_or("Claude Desktop is not installed")?,
        };
        let mut command = rightkit_child_process::command_std(exe);
        command.args(&self.args);
        self.apply_env(&mut command);
        spawn_detached(command).map_err(|e| e.to_string())
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn start(&self) -> Result<(), String> {
        // No launcher on this platform; the request is still fully formed.
        let _ = (&self.exe, &self.args, &self.env);
        Err("unsupported".into())
    }
}

/// The Microsoft Store Claude's package family name, when it is installed.
#[cfg(windows)]
fn store_family() -> Option<String> {
    let options = rightkit_child_process::RunOptions::new(Duration::from_secs(20))
        .stdout_cap(4096)
        .stderr_cap(0);
    let args = [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "(Get-AppxPackage -Name 'Claude*' | Select-Object -First 1).PackageFamilyName",
    ];
    let done = rightkit_child_process::run("powershell.exe", args, &options)
        .ok()
        .filter(|done| !done.timed_out)?;
    let text = String::from_utf8_lossy(&done.stdout.bytes)
        .trim()
        .to_string();
    (done.status.success() && !text.is_empty()).then_some(text)
}

/// The newest `%LOCALAPPDATA%\AnthropicClaude\app-*\claude.exe`.
#[cfg(windows)]
fn windows_exe() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("AnthropicClaude");
    let version = |name: &str| -> Vec<u64> {
        name.trim_start_matches("app-")
            .split(['.', '-'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    fs::read_dir(&base)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let exe = e.path().join("claude.exe");
            (name.starts_with("app-") && exe.is_file()).then(|| (version(&name), exe))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, exe)| exe)
}

/// Starts a program that outlives this one (Claude Desktop, the sign-in catcher) with no standard
/// handles of ours: inheriting them, it held the output pipe of `host claude desktop switch`
/// open, so the notch reading that output waited until Claude quit and the switch looked hung,
/// holding its lock (#426).
#[cfg(windows)]
fn spawn_detached(mut command: Command) -> std::io::Result<()> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    rightkit_child_process::OwnedCommand::from_command(command)
        .spawn_uncontained_detached()
        .map(drop)
}

fn start(launch: &Launch) -> Result<(), Fail> {
    launch.start().map_err(|e| (e, "launch_failed"))
}

// -------------------------------------------------------------------- state

fn lock() -> Result<FileLock, Fail> {
    let path = state_base()?.join("claude-desktop.lock");
    if let Some(dir) = path.parent() {
        ensure_private_dir(dir).map_err(|e| (e.to_string(), "io"))?;
    }
    FileLock::acquire_kind(
        &path,
        LockKind::Flock,
        &LockOptions::wait(Duration::ZERO).labeled("rightkit claude desktop"),
    )
    .map_err(|_| {
        (
            "another account switch or add is running".to_string(),
            "busy",
        )
    })
}

fn uuid_of(dir: &Path) -> Option<String> {
    sync::read_active_account(dir)
}

/// Remember each named folder's account.
fn refresh_registry(slots: &[Slot]) -> Registry {
    let mut registry = load_registry();
    let mut changed = false;
    for slot in slots.iter().filter(|s| s.name != "main") {
        let uuid = uuid_of(&slot.dir);
        match registry.accounts.iter_mut().find(|a| a.name == slot.name) {
            Some(a) => {
                if uuid.is_some() && a.account_uuid != uuid {
                    a.account_uuid = uuid;
                    changed = true;
                }
            }
            None => {
                registry.accounts.push(Account {
                    name: slot.name.clone(),
                    account_uuid: uuid,
                    added_ms: sync::now_ms(),
                });
                changed = true;
            }
        }
    }
    if changed {
        let _ = save_registry(&registry);
    }
    registry
}

// --------------------------------------------------------------------- list

/// The name the user gave an account (or its sign-in address's first part) in the notch's account
/// book, `claude-account-usage.json` in the host app's state folder, by account id. The book holds no
/// secrets; nothing is read from the login itself (#425: the card's buttons showed no names).
fn account_name(uuid: &str) -> Option<String> {
    let bytes = fs::read(state_base().ok()?.join("claude-account-usage.json")).ok()?;
    let book: Value = serde_json::from_slice(&bytes).ok()?;
    let entry = book
        .get("accounts")?
        .as_array()?
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(uuid))?;
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    text("customName")
        .map(str::to_string)
        .or_else(|| text("email").map(|e| e.split('@').next().unwrap_or(e).to_string()))
}

fn slot_json(slot: &Slot, running: &[Instance], current: &str) -> Value {
    let account = slot_account(slot, current);
    let label = account
        .as_deref()
        .and_then(account_name)
        .unwrap_or_else(|| {
            if slot.name == "main" {
                "Main".to_string()
            } else {
                slot.name.clone()
            }
        });
    // One folder: the Claude in Claude's own folder runs whichever login is in it, so it counts
    // only for that slot (counting it for "main" too left Main disabled while another login ran).
    // A slot's own folder still running from before the one-folder switch also counts.
    let is_running = running.iter().any(|i| {
        if i.kind == Kind::Slot("main".into()) {
            slot.name == current
        } else {
            i.kind == Kind::Slot(slot.name.clone())
        }
    });
    json!({
        "slot": slot.name,
        "label": label,
        "dir": slot.dir.to_string_lossy(),
        // The login in Claude's folder, running or not: the card ticks it.
        "current": slot.name == current,
        "account": account,
        "account_short": account.as_deref().map(short),
        "signed_in": account.is_some(),
        "running": is_running,
        "active": is_running,
    })
}

/// The accounts signed in to the Claude Desktop windows running now: one id per running main
/// process (the default instance reads Claude's own folder, any other its `--user-data-dir`,
/// `ClaudeProfiles` included). An account whose Desktop is closed is not here, so the notch
/// draws a ring only for an account with a window (#470).
fn running_accounts(running: &[Instance]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for instance in running {
        let dir = instance
            .args
            .iter()
            .find_map(|a| a.strip_prefix(sync::USER_DATA_DIR))
            .map(|d| PathBuf::from(d.trim_matches('"')))
            .or_else(|| main_root().ok());
        if let Some(id) = dir.and_then(|d| uuid_of(&d))
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids.sort();
    ids
}

/// What a scan finds (#353), with nothing entered by hand: every Claude Desktop data folder
/// there is, each with the account signed in and the chats per account. The folders are Claude's
/// default one, each the host app account folder, each second profile under `ClaudeProfiles`
/// (`chat::deliver_claude::desktop_chat_roots`) and the `--user-data-dir` of every running
/// Claude Desktop. `warnings` names a running Claude Desktop whose data folder cannot be read,
/// so a missing account is never silent. Only counts and ids are read.
fn found(running: &[Instance]) -> Value {
    let mut roots: Vec<(PathBuf, Option<String>)> = crate::desktop_roots::desktop_chat_roots()
        .into_iter()
        .map(|(root, label, _)| (root, label))
        .collect();
    let mut warnings: Vec<String> = Vec::new();
    for instance in running {
        if instance.kind == Kind::Unknown {
            warnings.push(format!(
                "Claude Desktop (pid {}) is running and its data folder cannot be read, so its accounts are not listed",
                instance.pid
            ));
            continue;
        }
        let dir = instance
            .args
            .iter()
            .find_map(|a| a.strip_prefix(sync::USER_DATA_DIR))
            .map(|d| PathBuf::from(d.trim_matches('"')));
        // A second data folder on the Mac is a Claude run with CFFIXED_USER_HOME (#353, #352).
        let dir = dir.or_else(|| {
            instance.env.get("CFFIXED_USER_HOME").map(|h| {
                Path::new(h.trim_matches('"'))
                    .join("Library")
                    .join("Application Support")
                    .join("Claude")
            })
        });
        if let Some(dir) = dir {
            let key = sync::data_dir_key(&dir);
            if dir.is_dir() && !roots.iter().any(|(r, _)| sync::data_dir_key(r) == key) {
                let label = dir.file_name().map(|n| n.to_string_lossy().into_owned());
                roots.push((dir, label));
            }
        }
    }
    let out: Vec<Value> = roots
        .iter()
        .map(|(root, label)| {
            let mut per_account: BTreeMap<String, usize> = BTreeMap::new();
            for (account, org) in sync::org_folders(root) {
                let chats = fs::read_dir(&org)
                    .map(|l| {
                        l.flatten()
                            .filter(|e| {
                                let n = e.file_name().to_string_lossy().into_owned();
                                n.starts_with("local_") && n.ends_with(".json")
                            })
                            .count()
                    })
                    .unwrap_or(0);
                *per_account.entry(account).or_default() += chats;
            }
            let signed_in = uuid_of(root);
            if let Some(account) = &signed_in {
                per_account.entry(account.clone()).or_default();
            }
            json!({
                "dir": root.to_string_lossy(),
                "profile": label,
                "signed_in": signed_in,
                "accounts": per_account.iter().map(|(account, chats)| json!({
                    "account": account,
                    "account_short": short(account),
                    "chats": chats,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({"roots": out, "warnings": warnings, "running": running_accounts(running)})
}

/// `found` on its own, for `host claude accounts`: reads only, never finishes a pending
/// sign-in or starts anything.
pub fn scan() -> Value {
    let slot_list = slots(&load_registry()).unwrap_or_default();
    found(&instances(&slot_list))
}

/// `host claude desktop list`. A pending sign-in that has completed is finished here: the
/// instances quit for it are started again.
pub fn list() -> Result<Value, Fail> {
    let mut registry = load_registry();
    let mut slot_list = slots(&registry)?;
    let mut restored = 0usize;
    // A swap a crash left half done is put right whenever the lock is free (#425); when it cannot
    // be yet (Claude is open on it), the list says so instead of failing.
    let mut interrupted = None;
    let mut recovered = None;
    if swap_dir().is_ok_and(|d| d.exists())
        && let Ok(_guard) = lock()
    {
        match recover_swap(&slot_list) {
            Ok(done) => recovered = done,
            Err((message, _)) => interrupted = Some(message),
        }
    }
    if let Some(pending) = read_pending()
        && uuid_of(&pending.dir).is_some()
        && let Ok(_guard) = lock()
    {
        restored = finish_pending(&mut registry, false)?.restored;
        slot_list = slots(&registry)?;
    }
    let registry = refresh_registry(&slot_list);
    let running = instances(&slot_list);
    let current = current_login(&registry, &slot_list[0].dir);
    let pending = read_pending().map(|p| json!({"name": p.name, "waiting_for_sign_in": true}));
    Ok(json!({"desktop": {
        "slots": slot_list.iter().map(|s| slot_json(s, &running, &current)).collect::<Vec<_>>(),
        "signed_in_slot": current,
        "known": registry.accounts.len(),
        "pending": pending,
        "restored": restored,
        "protected_running": running.iter().filter(|i| i.kind == Kind::Protected).count(),
        "found": found(&running),
        "recovered": recovered,
        "interrupted_switch": interrupted,
    }}))
}

// ------------------------------------------------------------------ pending

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Restart {
    /// An instance of Claude A's slot (main or a named account): never started again by a
    /// finished sign-in, because the slot is one account at a time.
    #[serde(default)]
    slot: bool,
    exe: PathBuf,
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    name: String,
    dir: PathBuf,
    started_ms: u64,
    restart: Vec<Restart>,
}

fn read_pending() -> Option<Pending> {
    let bytes = fs::read(pending_file().ok()?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

struct Finished {
    restored: usize,
}

/// Record the account and start the instances that were quit for the sign-in again (skipping
/// any already running). Without `force` the account must have signed in.
fn finish_pending(registry: &mut Registry, force: bool) -> Result<Finished, Fail> {
    let Some(pending) = read_pending() else {
        return fail("no account is waiting for sign-in", "no_pending");
    };
    let uuid = uuid_of(&pending.dir);
    if uuid.is_none() && !force {
        return fail(
            format!("{} has not signed in yet", pending.name),
            "not_signed_in",
        );
    }
    match registry
        .accounts
        .iter_mut()
        .find(|a| a.name == pending.name)
    {
        Some(a) => a.account_uuid = uuid,
        None => registry.accounts.push(Account {
            name: pending.name.clone(),
            account_uuid: uuid,
            added_ms: pending.started_ms,
        }),
    }
    save_registry(registry)?;
    let slot_list = slots(registry)?;
    let running = instances(&slot_list);
    let mut restored = 0;
    for r in pending.restart.iter().filter(|r| !r.slot) {
        let same = running
            .iter()
            .any(|i| i.args == r.args && i.env.get("HOME") == r.env.get("HOME"));
        if same {
            continue;
        }
        start(&Launch {
            exe: Some(r.exe.clone()),
            args: r.args.clone(),
            env: r.env.clone(),
        })?;
        restored += 1;
    }
    if let Ok(path) = pending_file() {
        let _ = fs::remove_file(path);
    }
    Ok(Finished { restored })
}

// ---------------------------------------------------------------------- add

/// `host claude desktop add <name>`: quit every Claude Desktop (a sign-in callback goes to a
/// running one), start Claude on the new empty folder for the one-time sign-in.
pub fn add(name: &str) -> Result<Value, Fail> {
    if !valid_name(name) || name == "main" {
        return fail(
            "an account name is 1 to 32 letters, digits, '.', '_' or '-', not starting with '.' or '-', and not 'main'",
            "bad_name",
        );
    }
    let _guard = lock()?;
    let mut registry = load_registry();
    let slot_list = slots(&registry)?;
    recover_swap(&slot_list)?;
    if slot_list.iter().any(|s| s.name == name) {
        return fail(format!("{name} already exists"), "exists");
    }
    if read_pending().is_some() {
        return fail(
            "another account is waiting for sign-in; finish it first (add <name> --finish)",
            "pending",
        );
    }
    let running = instances(&slot_list);
    if let Some(unknown) = running.iter().find(|i| i.kind == Kind::Unknown) {
        return fail(
            format!(
                "cannot read the launch environment of Claude (pid {}), so it could not be started again; nothing was quit",
                unknown.pid
            ),
            "unreadable",
        );
    }
    let dir = accounts_dir()?.join(name);
    let restart: Vec<Restart> = running
        .iter()
        .map(|i| Restart {
            slot: matches!(i.kind, Kind::Slot(_)),
            exe: i.exe.clone(),
            args: i.args.clone(),
            env: i.env.clone(),
        })
        .collect();
    let pending = Pending {
        name: name.to_string(),
        dir: dir.clone(),
        started_ms: sync::now_ms(),
        restart,
    };
    // Written before anything is quit, so a crash never loses what to start again.
    write_json(&pending_file()?, &pending)?;
    let left = quit_all(&running, false);
    if !left.is_empty() {
        // Put back what did quit and leave nothing pending.
        for r in running
            .iter()
            .filter(|i| !left.iter().any(|l| l.pid == i.pid))
        {
            let _ = start(&Launch::restore(r));
        }
        let _ = fs::remove_file(pending_file()?);
        return fail(
            "Claude did not quit (unsaved work?); nothing was changed",
            "quit_timeout",
        );
    }
    ensure_private_dir(&dir).map_err(|e| (e.to_string(), "io"))?;
    registry.accounts.push(Account {
        name: name.to_string(),
        account_uuid: None,
        added_ms: sync::now_ms(),
    });
    save_registry(&registry)?;
    start(&Launch::slot(&Slot {
        name: name.to_string(),
        dir: dir.clone(),
    }))?;
    #[cfg(windows)]
    spawn_catcher(name);
    Ok(json!({"add": {
        "name": name,
        "dir": dir.to_string_lossy(),
        "waiting_for_sign_in": true,
        "quit": running.len(),
    }}))
}

/// Windows: the Store Claude claims every `claude://` link, so a first sign-in for the new
/// folder would land in it. A detached `host claude desktop catch-login <name>` watches for it.
#[cfg(windows)]
fn spawn_catcher(name: &str) {
    if let Ok(exe) = std::env::current_exe() {
        let mut command = rightkit_child_process::OwnedCommand::new(exe);
        command
            .command_mut()
            .args(["claude", "desktop", "catch-login", name])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _ = command.spawn_uncontained_detached();
    }
}

/// How long the sign-in link is watched for.
#[cfg(windows)]
const CATCH_WINDOW: Duration = Duration::from_secs(600);

/// The pid and link of a Claude started by a `claude://` link with no `--user-data-dir` (the
/// Store app taking the sign-in).
#[cfg(windows)]
fn find_login_link() -> Option<(u32, u64, String)> {
    let mut system = System::new().ok()?;
    let kind = ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always);
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    for (pid, p) in system.processes() {
        if !p
            .name()
            .to_string_lossy()
            .eq_ignore_ascii_case("claude.exe")
        {
            continue;
        }
        let args: Vec<String> = p.cmd().iter().map(text).collect();
        if args.iter().any(|a| a.contains(sync::USER_DATA_DIR)) {
            continue;
        }
        if let Some(arg) = args.iter().find(|a| a.contains("claude://")) {
            let start = arg.find("claude://")?;
            let link: String = arg[start..]
                .chars()
                .take_while(|c| *c != '"' && !c.is_whitespace())
                .collect();
            return Some((pid.as_u32(), p.start_time(), link));
        }
    }
    None
}

/// `host claude desktop catch-login <name>` (started by `add` on Windows): for up to ten
/// minutes, end the Claude that took the sign-in link and give the link to the new folder's
/// instance as its last argument.
#[cfg(windows)]
pub fn catch_login(name: &str) -> Result<Value, Fail> {
    let dir = accounts_dir()?.join(name);
    let deadline = Instant::now() + CATCH_WINDOW;
    while Instant::now() < deadline {
        if read_pending().is_none_or(|p| p.name != name) {
            return Ok(json!({"catch_login": {"caught": false, "reason": "finished"}}));
        }
        if let Some((pid, start_time, link)) = find_login_link() {
            force_quit(pid, start_time);
            std::thread::sleep(Duration::from_secs(1));
            let mut launch = Launch::slot(&Slot {
                name: name.to_string(),
                dir,
            });
            launch.args.push(link);
            start(&launch)?;
            return Ok(json!({"catch_login": {"caught": true}}));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(json!({"catch_login": {"caught": false, "reason": "no sign-in link in 10 minutes"}}))
}

#[cfg(not(windows))]
pub fn catch_login(_name: &str) -> Result<Value, Fail> {
    fail("catch-login is only used on Windows", "unsupported")
}

/// `host claude desktop add <name> --finish`.
pub fn add_finish(name: &str, force: bool) -> Result<Value, Fail> {
    let _guard = lock()?;
    recover_swap(&slots(&load_registry())?)?;
    match read_pending() {
        Some(p) if p.name == name => {}
        _ => return fail(format!("{name} is not waiting for sign-in"), "no_pending"),
    }
    let mut registry = load_registry();
    let finished = finish_pending(&mut registry, force)?;
    Ok(json!({"add": {
        "name": name,
        "waiting_for_sign_in": false,
        "restored": finished.restored,
    }}))
}

// ------------------------------------------------------------------- switch

/// `host claude desktop switch <slot>`: quit Claude's one folder, swap the login, merge the chats,
/// start it again.
pub fn switch(target: &str, force: bool) -> Result<Value, Fail> {
    let _guard = lock()?;
    let registry = load_registry();
    let slot_list = slots(&registry)?;
    // A swap a crash or failure left half done is put right before anything else (#425).
    let recovered = recover_swap(&slot_list)?;
    let Some(slot) = slot_list.iter().find(|s| s.name == target).cloned() else {
        return fail(format!("no account slot named {target}"), "no_slot");
    };
    if read_pending().is_some() {
        return fail(
            "an account is waiting for sign-in; finish it first (add <name> --finish)",
            "pending",
        );
    }
    let root = main_root()?;
    let running = instances(&slot_list);
    if let Some(unknown) = running.iter().find(|i| i.kind == Kind::Unknown) {
        return fail(
            format!(
                "cannot tell whether Claude (pid {}) is the main instance; nothing was changed",
                unknown.pid
            ),
            "unreadable",
        );
    }
    let current = current_login(&registry, &root);
    let main_running = running.iter().any(|i| i.kind == Kind::Slot("main".into()));
    if current == slot.name && main_running {
        return Ok(json!({"switch": {"slot": slot.name, "action": "already_running"}}));
    }
    // Read before anything is quit, and never an unreadable file taken for a missing one: the
    // exclusions, Claude's settings document, and the target's login (saved by an earlier switch,
    // else the account's own folder from its first sign-in), which must be whole.
    let excluded = exclusions()?;
    read_config(&root)?;
    let incoming = if current == slot.name {
        None
    } else {
        let found = match read_saved_login(&slot.name)? {
            Some(login) => Some(login),
            None if slot.name != "main" => read_folder_login(&slot.dir)?,
            None => None,
        };
        let Some(login) = found else {
            return fail(
                format!("{} has no saved sign-in; nothing was changed", slot.name),
                "no_login",
            );
        };
        let missing = login.missing();
        if !missing.is_empty() {
            return fail(
                format!(
                    "the saved sign-in of {} is incomplete (no {}); nothing was changed",
                    slot.name,
                    missing.join(", ")
                ),
                "incomplete_login",
            );
        }
        Some(login)
    };
    // Claude's folder and any account folder still running from before (Claude B never).
    let to_quit: Vec<Instance> = running
        .iter()
        .filter(|i| matches!(i.kind, Kind::Slot(_)))
        .cloned()
        .collect();
    let left = quit_all(&to_quit, force);
    if !left.is_empty() {
        return fail(
            "Claude did not quit (unsaved work?); nothing was opened. Use --force to end it",
            "quit_timeout",
        );
    }
    fs::create_dir_all(&root).map_err(|e| (format!("{}: {e}", root.display()), "io"))?;
    let mut saved = None;
    if let Some(login) = &incoming {
        // The outgoing login is saved before the incoming one replaces it, all in one journal.
        saved = swap_login(&root, &current, &slot.name, login)?;
    }
    // The account Claude must open with: the incoming one, else the one already in its folder.
    let expected = match &incoming {
        Some(login) => login.account().map(str::to_string),
        None => uuid_of(&root),
    };
    // Chats: the account folders' records from before the one-folder switch, then every account
    // folder in Claude's folder brings in the newest record of every chat.
    let sources: Vec<PathBuf> = slot_list
        .iter()
        .filter(|s| s.name != "main" && s.dir.is_dir())
        .map(|s| s.dir.clone())
        .collect();
    let backup = backup_folders(&root, &slot.name)?;
    let ids = transcript_ids(&root, &sources);
    let merge =
        sync::merge_into_root(&root, &sources, &excluded).map_err(|e| (e.to_string(), "merge"))?;
    let across = merge_accounts(&root);
    let repaired = repair(&root, &sources);
    let repointed = repoint(&root, &ids);
    start(&Launch::slot(&slot_list[0]))?;
    refresh_registry(&slot_list);
    // "Opened" is read back, not assumed: a start that returned but left no Claude running used to
    // look like a switch that worked ("c doesn't open", #427).
    if !wait_running(&slot_list) {
        return fail(
            "Claude was started but did not open; the sign-in was switched, so open Claude again",
            "not_opened",
        );
    }
    // So is the account it opened with (its id, never its sign-in).
    if let Some(expected) = &expected {
        match opened_account(&root) {
            Some(opened) if opened == *expected => {}
            Some(opened) => {
                return fail(
                    format!(
                        "Claude opened signed in as {} instead of {}",
                        short(&opened),
                        short(expected)
                    ),
                    "wrong_account",
                );
            }
            None => {
                return fail(
                    "Claude opened, but which account it is signed in as cannot be read",
                    "unverified",
                );
            }
        }
    }
    Ok(json!({"switch": {
        "slot": slot.name,
        "action": "switched",
        "from": current,
        "login": if incoming.is_some() { "swapped" } else { "kept" },
        "account": expected.as_deref().map(short),
        "saved": saved,
        "recovered": recovered,
        "quit": to_quit.len(),
        "backup": backup,
        "merge": {"folders": merge.folders, "copied": merge.copied, "skipped": merge.skipped},
        "accounts": across,
        "repaired": repaired,
        "repointed": repointed,
        "launched": true,
    }}))
}

/// The account in Claude's folder once it has opened, read for a few seconds while Claude may be
/// rewriting its settings.
fn opened_account(root: &Path) -> Option<String> {
    for _ in 0..6 {
        if let Some(uuid) = uuid_of(root) {
            return Some(uuid);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    None
}

/// Every account folder in `root` brings in the newest record of every chat (what `claude sync`
/// applies). A failure is reported, never fatal: Claude is closed and is started either way.
fn merge_accounts(root: &Path) -> Value {
    let result = (|| {
        let registry = sync::registry_path()?;
        let only = sync::read_registry(&registry)?.sync_set(&sync::accounts(root)?);
        let backups = backups_dir().map_err(|(m, _)| sync::SyncError::Io(m))?;
        sync::apply_for(root, &backups, &|| false, sync::now_ms(), Some(&only))
    })();
    match result {
        Ok(r) => json!({"files_changed": r.files_changed, "backup": r.backup}),
        Err(e) => json!({"error": e.to_string()}),
    }
}

// -------------------------------------------------------------------- logins

/// A sign-in as opaque bytes: its `config.json` values and its files (`None`: absent).
struct Login {
    keys: serde_json::Map<String, Value>,
    files: Vec<(&'static str, Option<Vec<u8>>)>,
}

/// The `config.json` keys one of which holds a usable sign-in's tokens.
const TOKEN_KEYS: [&str; 2] = ["oauth:tokenCache", "oauth:tokenCacheV2"];
/// The login files a usable sign-in cannot lack: the session cookie, and on Windows the key Claude
/// encrypts the cookie and tokens with.
const REQUIRED_FILES: &[&str] = if cfg!(windows) {
    &["Cookies", "Local State"]
} else {
    &["Cookies"]
};

impl Login {
    fn account(&self) -> Option<&str> {
        self.keys
            .get("lastKnownAccountUuid")
            .and_then(Value::as_str)
    }

    /// The parts a complete sign-in has and this one lacks, by name (never by value).
    fn missing(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.account().is_none() {
            out.push("lastKnownAccountUuid");
        }
        if !TOKEN_KEYS.iter().any(|k| self.keys.contains_key(*k)) {
            out.push("oauth:tokenCache");
        }
        for name in REQUIRED_FILES {
            if !self.files.iter().any(|(n, b)| n == name && b.is_some()) {
                out.push(*name);
            }
        }
        out
    }

    /// Byte for byte the same keys and files (compared, never shown).
    fn same(&self, other: &Login) -> bool {
        self.keys == other.keys && self.files == other.files
    }
}

/// The slot whose login is in Claude's folder: the named account with that folder's account,
/// else Main.
fn current_login(registry: &Registry, root: &Path) -> String {
    let uuid = uuid_of(root);
    uuid.and_then(|u| {
        registry
            .accounts
            .iter()
            .find(|a| a.account_uuid.as_deref() == Some(u.as_str()))
            .map(|a| a.name.clone())
    })
    .unwrap_or_else(|| "main".into())
}

/// A slot's account: in Claude's folder, saved, or in its own folder from its first sign-in.
fn slot_account(slot: &Slot, current: &str) -> Option<String> {
    if slot.name == current {
        return uuid_of(&main_root().ok()?);
    }
    read_saved_login(&slot.name)
        .ok()
        .flatten()
        .and_then(|l| l.account().map(str::to_string))
        .or_else(|| (slot.name != "main").then(|| uuid_of(&slot.dir)).flatten())
}

type ConfigMap = serde_json::Map<String, Value>;

/// A file that may be absent: `None` only when it does not exist. Any other failure is an error,
/// so an unreadable file is never taken for a missing one and a login is never swapped half read.
/// Only the path and the error's kind are reported, never the bytes.
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, Fail> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => fail(
            format!(
                "{} cannot be read ({}); nothing was changed",
                path.display(),
                e.kind()
            ),
            "unreadable",
        ),
    }
}

/// A `config.json` document: `None` when there is none, an error when it is not a JSON object.
fn parse_config(path: &Path, bytes: Option<&[u8]>) -> Result<Option<ConfigMap>, Fail> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(map)) => Ok(Some(map)),
        _ => fail(
            format!(
                "{} is not a settings file the host app can read; nothing was changed",
                path.display()
            ),
            "unreadable",
        ),
    }
}

fn read_config(dir: &Path) -> Result<Option<ConfigMap>, Fail> {
    let path = dir.join("config.json");
    parse_config(&path, read_optional(&path)?.as_deref())
}

/// Each login file by name, `None` when it is absent (an unreadable one is an error).
type LoginFiles = Vec<(&'static str, Option<Vec<u8>>)>;

fn read_files(dir: &Path) -> Result<LoginFiles, Fail> {
    LOGIN_FILES
        .iter()
        .map(|name| -> Result<_, Fail> { Ok((*name, read_optional(&dir.join(name))?)) })
        .collect()
}

/// The login in a folder's settings and files, when an account is signed in there.
fn login_of(
    config: Option<&ConfigMap>,
    files: Vec<(&'static str, Option<Vec<u8>>)>,
) -> Option<Login> {
    let keys: ConfigMap = LOGIN_KEYS
        .iter()
        .filter_map(|k| {
            config
                .and_then(|c| c.get(*k))
                .map(|v| ((*k).to_string(), v.clone()))
        })
        .collect();
    let login = Login { keys, files };
    login.account().is_some().then_some(login)
}

/// The login in a Claude data folder: `None` when no account is signed in there, an error when
/// one of its files cannot be read.
fn read_folder_login(dir: &Path) -> Result<Option<Login>, Fail> {
    let config = read_config(dir)?;
    Ok(login_of(config.as_ref(), read_files(dir)?))
}

/// A login saved by a switch: `None` when there is none, an error when it cannot be read.
fn read_saved_login(slot: &str) -> Result<Option<Login>, Fail> {
    let dir = logins_dir()?.join(slot);
    let path = dir.join("login.json");
    let Some(keys) = parse_config(&path, read_optional(&path)?.as_deref())? else {
        return Ok(None);
    };
    let login = Login {
        keys,
        files: read_files(&dir)?,
    };
    Ok(login.account().is_some().then_some(login))
}

/// Save a login for `slot`, replacing the saved one only once the new copy is complete.
fn save_login(slot: &str, login: &Login) -> Result<(), Fail> {
    let base = logins_dir()?;
    let io = |p: &Path, e: std::io::Error| (format!("{}: {e}", p.display()), "io");
    let fresh = base.join(format!(".{slot}.new"));
    let dest = base.join(slot);
    let _ = fs::remove_dir_all(&fresh);
    ensure_private_dir(&fresh).map_err(|e| (e.to_string(), "io"))?;
    for (name, bytes) in &login.files {
        if let Some(bytes) = bytes {
            let path = fresh.join(name);
            write_private(&path, bytes, true).map_err(|e| io(&path, e))?;
        }
    }
    let keys = Value::Object(login.keys.clone());
    write_json(&fresh.join("login.json"), &keys)?;
    // The old folder is replaced in one step (an exchange where the volume has one; else it is set
    // aside and put back if the new one cannot be published), never deleted first.
    match rightkit_fs::publish_dir(&fresh, &dest) {
        // The new login is in place; only the old tree's removal or a directory flush failed.
        Err(e) if e.target_replaced() => Ok(()),
        Err(e) => Err(io(&dest, std::io::Error::from(e))),
        Ok(_) => Ok(()),
    }
}

/// Put a login into a closed Claude data folder: its `config.json` keys replace those of `config`
/// (the folder's document as the snapshot read it; everything else in it is kept) and its files
/// replace the folder's. Only `swap_login` calls this, inside its journal.
fn put_login(root: &Path, config: Option<ConfigMap>, login: &Login) -> Result<(), Fail> {
    let io = |p: &Path, e: std::io::Error| (format!("{}: {e}", p.display()), "io");
    let mut config = config.unwrap_or_default();
    for key in LOGIN_KEYS {
        config.remove(key);
    }
    config.extend(login.keys.clone());
    let path = root.join("config.json");
    let bytes =
        serde_json::to_vec_pretty(&Value::Object(config)).map_err(|e| (e.to_string(), "io"))?;
    write_private(&path, &bytes, true).map_err(|e| io(&path, e))?;
    // Claude's unfinished-write copy of config.json would bring the old login back on start.
    let journal = root.join("config.json.journal");
    if journal.exists() {
        let aside = logins_dir()?.join(format!("config.json.journal-{}", sync::now_ms()));
        if fs::rename(&journal, &aside).is_err() {
            fs::remove_file(&journal).map_err(|e| io(&journal, e))?;
        }
    }
    for (name, bytes) in &login.files {
        let path = root.join(name);
        match bytes {
            Some(bytes) => write_private(&path, bytes, true).map_err(|e| io(&path, e))?,
            None if path.exists() => fs::remove_file(&path).map_err(|e| io(&path, e))?,
            None => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- swap journal

/// What a swap can change in Claude's folder: `config.json`, Claude's unfinished-write copy of it,
/// and `LOGIN_FILES`.
const SWAP_FILES: [&str; 5] = [
    "config.json",
    "config.json.journal",
    "Cookies",
    "Cookies-journal",
    "Local State",
];

/// Claude's folder's [`SWAP_FILES`] as they are, each read whole before anything is written.
struct Snapshot {
    files: Vec<(&'static str, Option<Vec<u8>>)>,
}

impl Snapshot {
    /// An unreadable file, or a `config.json` that is not a settings document, stops the swap.
    fn take(root: &Path) -> Result<Self, Fail> {
        let files = SWAP_FILES
            .iter()
            .map(|name| -> Result<_, Fail> { Ok((*name, read_optional(&root.join(name))?)) })
            .collect::<Result<Vec<_>, Fail>>()?;
        let snapshot = Snapshot { files };
        snapshot.config(root)?;
        Ok(snapshot)
    }

    fn get(&self, name: &str) -> Option<&[u8]> {
        self.files
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, bytes)| bytes.as_deref())
    }

    fn config(&self, root: &Path) -> Result<Option<ConfigMap>, Fail> {
        parse_config(&root.join("config.json"), self.get("config.json"))
    }

    /// The login signed in when the snapshot was taken.
    fn login(&self, root: &Path) -> Result<Option<Login>, Fail> {
        let files = LOGIN_FILES
            .iter()
            .map(|name| (*name, self.get(name).map(<[u8]>::to_vec)))
            .collect();
        Ok(login_of(self.config(root)?.as_ref(), files))
    }
}

/// The swap journal, `<the host app state>/claude-desktop-swap/`: `before/` holds the snapshot and
/// `swap.json` ([`SwapJournal`]) is written once `before/` is whole. Owner-only, like the saved
/// logins, and removed when the swap is finished or undone.
fn swap_dir() -> Result<PathBuf, Fail> {
    Ok(state_base()?.join("claude-desktop-swap"))
}

#[derive(Debug, Serialize, Deserialize)]
struct SwapJournal {
    version: u32,
    /// `snapshot`: `before/` is whole and Claude's folder is unchanged. `applying`: Claude's folder
    /// is being written. `applied`: it holds the incoming login, read back.
    stage: String,
    root: PathBuf,
    from: String,
    to: String,
    started_ms: u64,
    files: Vec<SwapFile>,
}

/// One snapshot file: whether it existed, and its size and fingerprint, so a torn copy is never
/// put back.
#[derive(Debug, Serialize, Deserialize)]
struct SwapFile {
    name: String,
    present: bool,
    size: u64,
    hash: u64,
}

struct Swap {
    dir: PathBuf,
    journal: SwapJournal,
}

impl Swap {
    /// Write the snapshot to `before/`, then the journal at `snapshot`.
    fn begin(root: &Path, snapshot: &Snapshot, from: &str, to: &str) -> Result<Self, Fail> {
        let io = |p: &Path, e: std::io::Error| (format!("{}: {e}", p.display()), "io");
        let dir = swap_dir()?;
        if dir.exists() {
            // Recovery runs first under the same lock, so this is a journal it could not clear.
            return fail(
                format!(
                    "an earlier switch left {} behind; nothing was changed",
                    dir.display()
                ),
                "interrupted",
            );
        }
        let before = dir.join("before");
        ensure_private_dir(&before).map_err(|e| (e.to_string(), "io"))?;
        let mut files = Vec::new();
        for (name, bytes) in &snapshot.files {
            if let Some(bytes) = bytes {
                let path = before.join(name);
                write_private(&path, bytes, true).map_err(|e| io(&path, e))?;
            }
            files.push(SwapFile {
                name: (*name).to_string(),
                present: bytes.is_some(),
                size: bytes.as_ref().map_or(0, |b| b.len() as u64),
                hash: bytes.as_deref().map_or(0, sync::fingerprint),
            });
        }
        let mut swap = Swap {
            dir,
            journal: SwapJournal {
                version: 1,
                stage: String::new(),
                root: root.to_path_buf(),
                from: from.to_string(),
                to: to.to_string(),
                started_ms: sync::now_ms(),
                files,
            },
        };
        swap.set_stage("snapshot")?;
        Ok(swap)
    }

    fn set_stage(&mut self, stage: &str) -> Result<(), Fail> {
        self.journal.stage = stage.to_string();
        write_json(&self.dir.join("swap.json"), &self.journal)
    }

    /// Done or undone: the journal goes first, then the snapshot's copies of the login.
    fn finish(self) {
        let _ = fs::remove_file(self.dir.join("swap.json"));
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Put Claude's folder back to the snapshot in `dir`: each file as it was, one that was absent
/// removed. Every copy is checked against the journal before anything is written.
fn restore_snapshot(dir: &Path, journal: &SwapJournal) -> Result<(), Fail> {
    let io = |p: &Path, e: std::io::Error| (format!("{}: {e}", p.display()), "io");
    let before = dir.join("before");
    let mut back: Vec<(&str, Option<Vec<u8>>)> = Vec::new();
    for file in &journal.files {
        if !SWAP_FILES.contains(&file.name.as_str()) {
            return fail(
                format!("the switch journal names an unknown file {}", file.name),
                "interrupted",
            );
        }
        let bytes = if file.present {
            match read_optional(&before.join(&file.name))? {
                Some(b) if b.len() as u64 == file.size && sync::fingerprint(&b) == file.hash => {
                    Some(b)
                }
                _ => {
                    return fail(
                        format!(
                            "the saved copy of {} in {} is incomplete",
                            file.name,
                            dir.display()
                        ),
                        "interrupted",
                    );
                }
            }
        } else {
            None
        };
        back.push((file.name.as_str(), bytes));
    }
    for (name, bytes) in back {
        let path = journal.root.join(name);
        match bytes {
            Some(bytes) => write_private(&path, &bytes, true).map_err(|e| io(&path, e))?,
            None => match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(&path, e)),
            },
        }
    }
    Ok(())
}

/// Finish or undo a swap that a crash or failure left behind, before anything else touches
/// Claude's folder (every command that holds the switcher's lock runs this first). A journal at
/// `applying` is rolled back to its snapshot, only while Claude's folder has no running Claude;
/// one at `snapshot` (nothing written yet) or `applied` (the new login is whole) is cleared.
fn recover_swap(slot_list: &[Slot]) -> Result<Option<Value>, Fail> {
    let dir = swap_dir()?;
    if !dir.exists() {
        return Ok(None);
    }
    let path = dir.join("swap.json");
    let journal = match read_optional(&path)? {
        None => None,
        Some(bytes) => Some(serde_json::from_slice::<SwapJournal>(&bytes).map_err(|_| {
            (
                format!(
                    "{} cannot be read, so an interrupted switch cannot be undone; nothing was changed",
                    path.display()
                ),
                "interrupted",
            )
        })?),
    };
    let Some(journal) = journal else {
        // The snapshot was never finished, so Claude's folder was never written.
        let _ = fs::remove_dir_all(&dir);
        return Ok(Some(json!({"swap": "discarded"})));
    };
    let done = |outcome: &str| {
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir_all(&dir);
        Some(json!({"swap": outcome, "from": journal.from, "to": journal.to}))
    };
    match journal.stage.as_str() {
        "snapshot" => Ok(done("discarded")),
        "applied" => Ok(done("finished")),
        "applying" => {
            if sync::data_dir_key(&journal.root) != sync::data_dir_key(&main_root()?) {
                return fail(
                    format!(
                        "the interrupted switch in {} names another Claude folder; nothing was changed",
                        dir.display()
                    ),
                    "interrupted",
                );
            }
            let busy = instances(slot_list)
                .iter()
                .any(|i| i.kind == Kind::Slot("main".into()) || i.kind == Kind::Unknown);
            if busy {
                return fail(
                    "a switch was interrupted while Claude's sign-in was being changed; quit Claude, then run `host claude desktop list` to put the sign-in back",
                    "interrupted",
                );
            }
            restore_snapshot(&dir, &journal)?;
            Ok(done("rolled_back"))
        }
        other => fail(
            format!("the switch journal has an unknown stage {other}; nothing was changed"),
            "interrupted",
        ),
    }
}

/// Swap the login in Claude's closed folder as one recoverable operation (#425). The folder's
/// [`SWAP_FILES`] are read whole (an unreadable one stops the swap) and journalled, the outgoing
/// login is saved, the incoming one is put in and read back byte for byte. A failure puts the
/// snapshot back at once; a crash leaves the journal at `applying` and the next command that holds
/// the switcher's lock puts it back (`recover_swap`). Returns the slot the outgoing login was
/// saved for.
fn swap_login(
    root: &Path,
    current: &str,
    target: &str,
    login: &Login,
) -> Result<Option<String>, Fail> {
    let snapshot = Snapshot::take(root)?;
    let outgoing = snapshot.login(root)?;
    let mut swap = Swap::begin(root, &snapshot, current, target)?;
    let mut saved = None;
    if let Some(outgoing) = &outgoing {
        // An incomplete login never replaces a complete saved one.
        let keep_saved = !outgoing.missing().is_empty()
            && read_saved_login(current)
                .ok()
                .flatten()
                .is_some_and(|l| l.missing().is_empty());
        if !keep_saved {
            if let Err(e) = save_login(current, outgoing) {
                swap.finish();
                return Err(e);
            }
            saved = Some(current.to_string());
        }
    }
    let applied = (|| -> Result<(), Fail> {
        let config = snapshot.config(root)?;
        swap.set_stage("applying")?;
        put_login(root, config, login)?;
        match read_folder_login(root)? {
            Some(now) if now.same(login) => Ok(()),
            _ => fail(
                "Claude's folder did not hold the new sign-in once it was written",
                "verify_failed",
            ),
        }
    })();
    if let Err((message, code)) = applied {
        return match restore_snapshot(&swap.dir, &swap.journal) {
            Ok(()) => {
                swap.finish();
                Err((
                    format!("{message}; Claude's sign-in was put back as it was"),
                    code,
                ))
            }
            Err((why, _)) => Err((
                format!(
                    "{message}; putting the old sign-in back failed too ({why}): do not open Claude, the next `host claude desktop list` tries again"
                ),
                code,
            )),
        };
    }
    swap.set_stage("applied")?;
    swap.finish();
    Ok(saved)
}

/// The accounts `claude sync` leaves out, read strictly: a registry that cannot be read stops the
/// switch before anything is quit.
fn exclusions() -> Result<BTreeSet<String>, Fail> {
    let path = sync::registry_path().map_err(|e| (e.to_string(), "unsupported"))?;
    sync::read_registry(&path)
        .map(|r| r.excluded())
        .map_err(|e| (e.to_string(), "unreadable"))
}

// ----------------------------------------------------------------- sync scope

/// The folders a full `claude sync` brings up to each other after Claude's folder (#427):
/// Claude's folder and every the host app account folder, each from all the others. It holds the
/// switcher's lock while the sync writes, so no switch or add runs alongside it.
pub struct SlotScope {
    _guard: FileLock,
    pub targets: Vec<sync::CrossTarget>,
    /// A swap a crash had left behind, put right before the sync (`recover_swap`).
    pub recovered: Option<Value>,
}

/// The scope of a sync of `root`, which must be Claude's own folder: a sync pointed at any other
/// folder (`--root`) never reaches the account folders, and the caller then passes no targets.
pub fn slot_scope(root: &Path) -> Result<SlotScope, Fail> {
    let guard = lock()?;
    let registry = load_registry();
    let slot_list = slots(&registry)?;
    if sync::data_dir_key(root) != sync::data_dir_key(&slot_list[0].dir) {
        return fail(
            format!(
                "{} is not Claude's own folder, so the account folders are not part of its sync",
                root.display()
            ),
            "scope",
        );
    }
    let recovered = recover_swap(&slot_list)?;
    let mut targets = Vec::new();
    for slot in slot_list.iter().filter(|s| s.dir.is_dir()) {
        let sources: Vec<PathBuf> = slot_list
            .iter()
            .filter(|s| s.name != slot.name && s.dir.is_dir())
            .map(|s| s.dir.clone())
            .collect();
        if sources.is_empty() {
            continue;
        }
        targets.push(sync::CrossTarget {
            label: slot.name.clone(),
            dir: slot.dir.clone(),
            sources,
        });
    }
    Ok(SlotScope {
        _guard: guard,
        targets,
        recovered,
    })
}

/// Copy the record files of the target's account folders under the backups folder before the
/// merge. Returns the backup folder, or None when there was nothing to back up.
fn backup_folders(target: &Path, slot: &str) -> Result<Option<String>, Fail> {
    let folders = sync::org_folders(target);
    if folders.is_empty() {
        return Ok(None);
    }
    let base = backups_dir()?;
    let dir = base.join(format!("desktop-switch-{}-{slot}", sync::now_ms()));
    for (account, path) in &folders {
        let org = path.file_name().unwrap_or_default();
        let dest = dir.join(account).join(org);
        ensure_private_dir(&dest).map_err(|e| (e.to_string(), "io"))?;
        let entries = fs::read_dir(path).map_err(|e| (format!("{}: {e}", path.display()), "io"))?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let wanted = name.starts_with("local_")
                || name.starts_with("deleted_")
                || name == "archived-sessions.idx";
            if wanted && entry.file_type().is_ok_and(|t| t.is_file()) {
                let bytes = fs::read(entry.path())
                    .map_err(|e| (format!("{}: {e}", entry.path().display()), "io"))?;
                write_private(&dest.join(&name), &bytes, false)
                    .map_err(|e| (format!("{}: {e}", dest.join(&name).display()), "io"))?;
            }
        }
    }
    prune_switch_backups(&base);
    Ok(Some(dir.to_string_lossy().into_owned()))
}

/// Keep the newest switch backups within both the count (`KEEP_SWITCH_BACKUPS`) and the size
/// (`SWITCH_BACKUPS_MAX_BYTES`) caps. The newest always survives, even when it alone is over the
/// size cap, because it is the one this switch just made.
fn prune_switch_backups(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    let mut mine: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let rest = name.strip_prefix("desktop-switch-")?;
            let ms = rest.split('-').next()?.parse().ok()?;
            Some((ms, e.path()))
        })
        .collect();
    mine.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    for (_, path) in mine.iter().skip(KEEP_SWITCH_BACKUPS) {
        let _ = fs::remove_dir_all(path);
    }
    let mut total = 0u64;
    for (i, (_, path)) in mine.iter().take(KEEP_SWITCH_BACKUPS).enumerate() {
        total = total.saturating_add(sync::dir_bytes(path));
        if i > 0 && total > SWITCH_BACKUPS_MAX_BYTES {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// How long a started Claude may take to show as a running main process.
const OPEN_WAIT: Duration = Duration::from_secs(25);

/// True once Claude's own folder has a running main instance.
fn wait_running(slot_list: &[Slot]) -> bool {
    let start = Instant::now();
    loop {
        if instances(slot_list)
            .iter()
            .any(|i| i.kind == Kind::Slot("main".into()))
        {
            return true;
        }
        if start.elapsed() >= OPEN_WAIT {
            return false;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

// ------------------------------------------------------------------- repoint

fn cli_session_id(value: &Value) -> Option<String> {
    value
        .get("cliSessionId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Every transcript id any copy of a chat names, by record file name, across Claude's folder (every
/// account) and `sources` (the account folders from before the one-folder switch). Take it before a
/// merge: [`repoint`] needs the ids the merge is about to overwrite (#427).
pub fn transcript_ids(target: &Path, sources: &[PathBuf]) -> BTreeMap<String, BTreeSet<String>> {
    let mut ids: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut roots: Vec<&Path> = vec![target];
    roots.extend(sources.iter().map(PathBuf::as_path));
    for root in roots {
        for (_, path) in sync::org_folders(root) {
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !(name.starts_with("local_") && name.ends_with(".json")) {
                    continue;
                }
                let Some(value) = fs::read(entry.path())
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                else {
                    continue;
                };
                if let Some(id) = cli_session_id(&value) {
                    ids.entry(name).or_default().insert(id);
                }
            }
        }
    }
    ids
}

/// Make every chat of the signed-in account point at its furthest-along transcript. A chat's
/// record holds `cliSessionId`, the id of the transcript under the real home's
/// `.claude/projects` that Claude shows; the same chat's records in the other account folders can
/// hold a later id (the chat went on there, or from the phone, and the record's turn count did not
/// outrank the older copy). The merge ranks records by turns, so a copy that is behind in
/// transcript can win or tie and leave the account showing old messages (#427). The transcript
/// files themselves are the truth, measured by how many events they hold, never by modification
/// time (a copy or a touch of an older file moves its date but adds no event): when another copy's
/// id names a transcript with more events than the one this record names, this record takes that
/// id (the old one moves to `priorCliSessionIds`, so no branch is lost). Only `cliSessionId`,
/// `priorCliSessionIds` and `transcriptUnavailable` are touched, and only when a transcript file
/// of the other id exists. Returns how many records were rewritten. `ids` comes from
/// [`transcript_ids`] taken BEFORE the merge: the merge overwrites the losing copies (the ones that
/// name the newer transcript) with the rank winner, so they can no longer be read after it.
pub fn repoint(target: &Path, ids: &BTreeMap<String, BTreeSet<String>>) -> usize {
    let projects = real_home().join(".claude").join("projects");
    let project_dirs: Vec<PathBuf> = fs::read_dir(&projects)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    // The events (lines) in a transcript, the most of any project folder's copy of that id.
    let written = |id: &str| -> Option<u64> {
        use std::io::Read;
        project_dirs
            .iter()
            .filter_map(|p| fs::File::open(p.join(format!("{id}.jsonl"))).ok())
            .map(|mut file| {
                let mut buf = [0u8; 64 * 1024];
                let mut lines = 0u64;
                while let Ok(n) = file.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    lines += buf[..n].iter().filter(|b| **b == b'\n').count() as u64;
                }
                lines
            })
            .max()
    };
    let active = sync::read_active_account(target);
    let mut rewritten = 0;
    for (account, path) in sync::org_folders(target) {
        if Some(&account) != active.as_ref() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(candidates) = ids.get(&name) else {
                continue;
            };
            let file = entry.path();
            let Ok(bytes) = fs::read(&file) else { continue };
            let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let Some(current) = cli_session_id(&value) else {
                continue;
            };
            let current_at = written(current.as_str());
            let newest = candidates
                .iter()
                .filter(|id| **id != current)
                .filter_map(|id| written(id.as_str()).map(|at| (at, id)))
                .max();
            let Some((at, id)) = newest else { continue };
            let later = current_at.is_none_or(|cur| at > cur);
            if !later {
                continue;
            }
            let Some(map) = value.as_object_mut() else {
                continue;
            };
            let mut prior: Vec<Value> = map
                .get("priorCliSessionIds")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !prior.iter().any(|p| p.as_str() == Some(current.as_str())) {
                prior.push(Value::String(current.clone()));
            }
            map.insert("priorCliSessionIds".into(), Value::Array(prior));
            map.insert("cliSessionId".into(), Value::String(id.clone()));
            map.remove("transcriptUnavailable");
            let out = if bytes.starts_with(b"{\n") {
                serde_json::to_vec_pretty(&value)
            } else {
                serde_json::to_vec(&value)
            };
            let Ok(out) = out else { continue };
            if write_private(&file, &out, true).is_ok() {
                rewritten += 1;
            }
        }
    }
    rewritten
}

// ------------------------------------------------------------------- repair

/// Records that a launch with the wrong home damaged: `cliSessionId` dropped and
/// `transcriptUnavailable` set. Restore the id from a good copy in another folder, and drop the
/// flag where the transcript exists under the real home's `.claude/projects`. Returns how many
/// records were rewritten.
fn repair(target: &Path, sources: &[PathBuf]) -> usize {
    let projects = real_home().join(".claude").join("projects");
    let project_dirs: Vec<PathBuf> = fs::read_dir(&projects)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    let transcript_exists = |id: &str| {
        project_dirs
            .iter()
            .any(|p| p.join(format!("{id}.jsonl")).is_file())
    };
    // The furthest-along good copy's cliSessionId per record file name.
    let mut good: BTreeMap<String, (i64, String)> = BTreeMap::new();
    for root in sources {
        for (_, path) in sync::org_folders(root) {
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !(name.starts_with("local_") && name.ends_with(".json")) {
                    continue;
                }
                let Some(value) = fs::read(entry.path())
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                else {
                    continue;
                };
                let Some(cli) = value
                    .get("cliSessionId")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let turns = value
                    .get("completedTurns")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                if good.get(&name).is_none_or(|(t, _)| turns > *t) {
                    good.insert(name, (turns, cli.to_string()));
                }
            }
        }
    }
    let active = sync::read_active_account(target);
    let mut rewritten = 0;
    for (account, path) in sync::org_folders(target) {
        if Some(&account) != active.as_ref() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("local_") && name.ends_with(".json")) {
                continue;
            }
            let file = entry.path();
            let Ok(bytes) = fs::read(&file) else { continue };
            let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let Some(map) = value.as_object_mut() else {
                continue;
            };
            let mut changed = false;
            let has_id = map
                .get("cliSessionId")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            if !has_id && let Some((_, cli)) = good.get(&name) {
                map.insert("cliSessionId".into(), Value::String(cli.clone()));
                changed = true;
            }
            let id = map
                .get("cliSessionId")
                .and_then(Value::as_str)
                .map(str::to_string);
            if map.contains_key("transcriptUnavailable")
                && id.as_deref().is_some_and(&transcript_exists)
            {
                map.remove("transcriptUnavailable");
                changed = true;
            }
            if !changed {
                continue;
            }
            let pretty = bytes.starts_with(b"{\n");
            let out = if pretty {
                serde_json::to_vec_pretty(&value)
            } else {
                serde_json::to_vec(&value)
            };
            let Ok(out) = out else { continue };
            if write_private(&file, &out, true).is_ok() {
                rewritten += 1;
            }
        }
    }
    rewritten
}

// ------------------------------------------------- account watch and chat move (#353)

/// Whether the `claudeAccountScan` setting is on (the notch's `notch-state.json`; absent or
/// unreadable means on, as every switch defaults). Off, neither the account watch nor
/// `host claude accounts move` runs.
pub fn account_scan_enabled() -> bool {
    #[cfg(not(windows))]
    {
        rightkit_app_host::settings()
            .get("claudeAccountScan")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }
    #[cfg(windows)]
    {
        true
    }
}

/// Modification times of the folders where a new Claude account or profile shows up: the
/// `ClaudeProfiles` folder and each profile in it, and every data folder's `claude-code-sessions`.
/// A path that is missing reads as 0, so one that appears is a change. Only metadata is read.
pub fn scan_roots_state() -> Vec<(PathBuf, u128)> {
    let mtime = |p: &Path| -> u128 {
        fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    let profiles = real_home().join("ClaudeProfiles");
    if let Ok(listing) = fs::read_dir(&profiles) {
        paths.extend(listing.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    paths.push(profiles);
    for (root, _, _) in crate::desktop_roots::desktop_chat_roots() {
        paths.push(root.join("claude-code-sessions"));
        paths.push(root);
    }
    paths.sort();
    paths.dedup();
    paths.into_iter().map(|p| (p.clone(), mtime(&p))).collect()
}

/// Runs [`scan`] only when an account root's modification time differs from `last`, then writes
/// the result to `<the host app state>/claude-accounts-scan.json` (`{"scanned_ms", "found"}`, ids and
/// counts only) for the notch's Claude card. Returns whether it scanned.
pub fn publish_scan_if_changed(last: &mut Option<Vec<(PathBuf, u128)>>) -> Result<bool, Fail> {
    let mut state = scan_roots_state();
    // The running accounts are part of the state: a window opened or closed republishes (#470).
    let slot_list = slots(&load_registry()).unwrap_or_default();
    let running = running_accounts(&instances_light(&slot_list));
    state.push((PathBuf::from(format!("running:{}", running.join(","))), 0));
    if last.as_ref() == Some(&state) {
        refresh_stale_scan()?;
        return Ok(false);
    }
    let out = json!({"scanned_ms": sync::now_ms(), "found": scan()});
    write_json(&state_base()?.join("claude-accounts-scan.json"), &out)?;
    *last = Some(state);
    Ok(true)
}

/// Re-stamps the published scan with its same `found` and a new `scanned_ms` once it is over 5
/// minutes old, so a quiet machine's card does not treat it as stale. Does not rescan. A missing
/// or unreadable file is left alone.
fn refresh_stale_scan() -> Result<(), Fail> {
    const MAX_AGE_MS: u64 = 5 * 60 * 1000;
    let path = state_base()?.join("claude-accounts-scan.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(doc) = serde_json::from_str::<Value>(&text) else {
        return Ok(());
    };
    let now = sync::now_ms();
    let Some(scanned) = doc.get("scanned_ms").and_then(Value::as_u64) else {
        return Ok(());
    };
    let Some(found) = doc.get("found").cloned() else {
        return Ok(());
    };
    if now.saturating_sub(scanned) <= MAX_AGE_MS {
        return Ok(());
    }
    write_json(&path, &json!({"scanned_ms": now, "found": found}))
}

/// One discovered data folder for `move`: `(folder, name, signed-in account)`.
fn move_candidates() -> Vec<(PathBuf, String, Option<String>)> {
    crate::desktop_roots::desktop_chat_roots()
        .into_iter()
        .map(|(root, label, main)| {
            let name = if main {
                "main".to_string()
            } else {
                label.unwrap_or_else(|| root.to_string_lossy().into_owned())
            };
            let account = uuid_of(&root);
            (root, name, account)
        })
        .collect()
}

/// A `from`/`to` argument as one discovered data folder: its name (`main`, a slot or profile
/// name), its folder path, or its signed-in account id (full, or the first 6 or more characters).
fn resolve_account(spec: &str) -> Result<(PathBuf, String, Option<String>), Fail> {
    let all = move_candidates();
    let hits: Vec<_> = all
        .iter()
        .filter(|(root, name, account)| {
            name == spec
                || root.to_string_lossy() == spec
                || account
                    .as_deref()
                    .is_some_and(|a| a == spec || (spec.len() >= 6 && a.starts_with(spec)))
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => fail(
            format!(
                "{spec} matches no Claude account folder (found: {})",
                all.iter()
                    .map(|c| c.1.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "not_found",
        ),
        _ => fail(
            format!("{spec} matches more than one Claude account folder; use its folder path"),
            "ambiguous",
        ),
    }
}

/// Whether a data folder belongs to a second Claude profile (Claude B, under the account's
/// `ClaudeProfiles`, run with its own `--user-data-dir`): never a move's source or target (#368).
fn is_profile_root(dir: &Path) -> bool {
    let key = sync::data_dir_key(dir);
    let profiles = sync::data_dir_key(&real_home().join("ClaudeProfiles"));
    let sep = std::path::MAIN_SEPARATOR;
    key == profiles
        || key.starts_with(&format!("{profiles}{sep}"))
        || dir.components().any(|c| c.as_os_str() == "ClaudeProfiles")
}

/// A dry run the hub showed the user, held in the resident engine until `move_apply` takes it.
struct MovePlan {
    id: String,
    from: PathBuf,
    to: PathBuf,
    counts: (u64, u64, u64),
    at: Instant,
}

static MOVE_PLANS: std::sync::Mutex<Vec<MovePlan>> = std::sync::Mutex::new(Vec::new());
const MOVE_PLAN_TTL: Duration = Duration::from_secs(600);

fn plan_counts(plan: &Value) -> (u64, u64, u64) {
    let n = |k: &str| plan.get(k).and_then(Value::as_u64).unwrap_or(0);
    (n("would_add"), n("would_update_if_newer"), n("unchanged"))
}

/// The dry run of [`move_chats`] for the hub (#353), with a `plan_id` that `move_apply` requires
/// and `target_running`. Writes nothing and never touches Claude.
pub fn move_plan(from: &str, to: &str) -> Result<Value, Fail> {
    let (from_dir, _, _) = resolve_account(from)?;
    let (to_dir, _, _) = resolve_account(to)?;
    let mut plan = move_chats(&from_dir.to_string_lossy(), &to_dir.to_string_lossy(), true)?;
    let counts = plan_counts(&plan);
    let id = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (&from_dir, &to_dir, counts, sync::now_ms()).hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    };
    let mut id = id;
    if let Ok(mut plans) = MOVE_PLANS.lock() {
        plans.retain(|p| p.at.elapsed() < MOVE_PLAN_TTL);
        // The plan the user may have open stays valid while the dry run still gives its counts.
        if let Some(held) = plans
            .iter_mut()
            .find(|p| p.from == from_dir && p.to == to_dir && p.counts == counts)
        {
            held.at = Instant::now();
            id = held.id.clone();
        } else {
            plans.retain(|p| p.from != from_dir || p.to != to_dir);
            plans.push(MovePlan {
                id: id.clone(),
                from: from_dir,
                to: to_dir.clone(),
                counts,
                at: Instant::now(),
            });
        }
    }
    plan["plan_id"] = json!(id);
    plan["target_running"] = json!(sync::claude_running_for(&to_dir));
    Ok(plan)
}

/// What the account scan offers to move (#353): every other data folder whose chats the signed-in
/// main folder lacks, each as a [`move_plan`]. Empty when the scan setting is off or nothing moves.
pub fn move_offers() -> Result<Value, Fail> {
    if !account_scan_enabled() {
        return Ok(json!([]));
    }
    let all: Vec<_> = move_candidates()
        .into_iter()
        .filter(|(root, _, _)| !is_profile_root(root))
        .collect();
    let Some(main) = all
        .iter()
        .find(|(_, name, account)| name == "main" && account.is_some())
    else {
        return Ok(json!([]));
    };
    let mut offers = Vec::new();
    for (root, _, _) in all.iter().filter(|c| c.0 != main.0) {
        let Ok(plan) = move_plan(&root.to_string_lossy(), &main.0.to_string_lossy()) else {
            continue;
        };
        let (add, update, _) = plan_counts(&plan);
        if add + update > 0 {
            offers.push(plan);
        }
    }
    Ok(json!(offers))
}

/// Applies the plan `move_plan` returned, by its id alone. Refuses an unknown or expired id, a
/// target Claude is running on (nothing is closed), and a folder that changed since the plan was
/// shown (the user reviews a new one).
pub fn move_apply(plan_id: &str) -> Result<Value, Fail> {
    let plan = {
        let mut plans = MOVE_PLANS
            .lock()
            .map_err(|_| ("plan store poisoned".to_string(), "io"))?;
        plans.retain(|p| p.at.elapsed() < MOVE_PLAN_TTL);
        let at = plans.iter().position(|p| p.id == plan_id);
        match at {
            Some(i) => plans.remove(i),
            None => {
                return fail(
                    "that move plan is unknown or expired; review the move again",
                    "no_plan",
                );
            }
        }
    };
    if sync::claude_running_for(&plan.to) {
        return fail(
            "Claude is running on the target account; close it yourself, then move",
            "running",
        );
    }
    let (from, to) = (plan.from.to_string_lossy(), plan.to.to_string_lossy());
    let now = move_chats(&from, &to, true)?;
    if plan_counts(&now) != plan.counts {
        return fail(
            "the chats changed since you reviewed the move; review it again",
            "changed",
        );
    }
    move_chats(&from, &to, false)
}

/// `host claude accounts move <from> <to> [--dry-run]`: brings the chats of `from` into `to`'s
/// signed-in account, by the same merge the sync uses (the furthest-along copy of each chat wins,
/// deletion markers carry), after a backup (`claude restore <id>` undoes it). `from` is left as it
/// is. `to` must be closed. Dry run lists what would be copied and writes nothing. Chat contents
/// and logins are never printed.
pub fn move_chats(from: &str, to: &str, dry_run: bool) -> Result<Value, Fail> {
    if !account_scan_enabled() {
        return fail(
            "the claudeAccountScan setting is off; turn it on to move chats between accounts",
            "disabled",
        );
    }
    let (from_dir, from_name, _) = resolve_account(from)?;
    let (to_dir, to_name, to_account) = resolve_account(to)?;
    for (dir, name) in [(&from_dir, &from_name), (&to_dir, &to_name)] {
        if is_profile_root(dir) {
            return fail(
                format!(
                    "{name} is a second Claude profile (under ClaudeProfiles); the host app never moves chats into or out of it"
                ),
                "profile",
            );
        }
    }
    if sync::data_dir_key(&from_dir) == sync::data_dir_key(&to_dir) {
        return fail("from and to are the same Claude folder", "same");
    }
    let Some(active) = to_account else {
        return fail(
            format!("{to_name} has no signed-in account, so there is nowhere to put chats"),
            "no_account",
        );
    };
    let excluded = sync::registry_path()
        .map(|p| sync::load_registry(&p).excluded())
        .unwrap_or_default();
    if excluded.contains(&active) {
        return fail(
            format!("{to_name}'s account is excluded from chat sync (`host claude include`)"),
            "excluded",
        );
    }
    let list = |dir: &Path, only: Option<&str>| -> Vec<(String, PathBuf)> {
        sync::org_folders(dir)
            .into_iter()
            .filter(|(a, _)| !excluded.contains(a) && only.is_none_or(|o| a == o))
            .map(|(_, p)| {
                (
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    p,
                )
            })
            .collect()
    };
    let is_record =
        |n: &str| (n.starts_with("local_") || n.starts_with("deleted_")) && n.ends_with(".json");
    if dry_run {
        // Same pairing as `merge_folders`: every source org folder (the other account's, plus
        // the target's non-active accounts') is merged into every org folder of the active account.
        let dests = list(&to_dir, Some(&active));
        let mut sources = list(&from_dir, None);
        let to_active_folders: BTreeSet<PathBuf> = dests.iter().map(|(_, p)| p.clone()).collect();
        sources.extend(
            list(&to_dir, None)
                .into_iter()
                .filter(|(_, p)| !to_active_folders.contains(p)),
        );
        let (mut add, mut update, mut same) = (0usize, 0usize, 0usize);
        for (_, dest) in &dests {
            let mut seen: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
            for (_, src) in &sources {
                for entry in fs::read_dir(src).into_iter().flatten().flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if is_record(&name) {
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        seen.entry(name).or_default().push(size);
                    }
                }
            }
            for (name, sizes) in seen {
                match fs::metadata(dest.join(&name)) {
                    Err(_) => add += 1,
                    Ok(m) if sizes.iter().any(|s| *s != m.len()) => update += 1,
                    Ok(_) => same += 1,
                }
            }
        }
        return Ok(json!({
            "dry_run": true, "from": from_name, "to": to_name,
            "would_add": add, "would_update_if_newer": update, "unchanged": same,
        }));
    }
    let _guard = lock()?;
    if sync::claude_running_for(&to_dir) {
        return fail(
            format!("Claude is running on {to_name}; close it, then move"),
            "running",
        );
    }
    let label: String = to_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string();
    let label = if label.is_empty() {
        "target".to_string()
    } else {
        label
    };
    let backups = sync::default_backups().map_err(|e| (e.to_string(), "io"))?;
    // No account of `to` is synced by the first phase; only the cross-folder merge runs.
    let result = sync::apply_scoped(
        &to_dir,
        &backups,
        &|| sync::claude_running_for(&to_dir),
        sync::now_ms(),
        Some(&BTreeSet::new()),
        &[sync::CrossTarget {
            label,
            dir: to_dir.clone(),
            sources: vec![from_dir],
        }],
        &excluded,
    )
    .map_err(|e| (e.to_string(), e.code()))?;
    Ok(json!({
        "dry_run": false, "from": from_name, "to": to_name,
        "backup": result.backup, "files_changed": result.files_changed,
        "slots": result.across,
    }))
}

/// What the host app calls Claude account `id` on this computer (#454): the name chosen in the hub or
/// on the other computer (newest rename wins, `claude_names`), else `Claude A`, `Claude B`, ... by
/// first-seen time across both computers, else `Claude <first 8 of the id>`.
pub fn account_label(id: &str) -> String {
    crate::names::label(id)
}
