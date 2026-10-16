//! Run a journey in the logged-on user's interactive desktop session from a session-0 process
//! (an SSH logon or a service on Windows), through a ONE-SHOT Task Scheduler task.
//!
//! Why: a hidden WebView2 app never gets a ready window from session 0, so a native journey driven
//! over SSH fails with "app window never became ready ... session 0". The whole journey command
//! (the harness AND the app) must run in the interactive session; this module relaunches it there.
//!
//! Design (see the README, Windows section):
//! * Opt-in only: [`opted_in`] (`--interactive` or `RIGHTKIT_QA_INTERACTIVE=1`). Without it a
//!   session-0 process keeps failing with the existing clear error ([`Decision::NotOptedIn`]).
//! * [`decide`] is pure: it maps [`SessionFacts`] to a [`Decision`]. The task is only created when
//!   the current account is the console (logged-on interactive) account.
//! * The task runs THIS executable in `__child <spec.json>` mode ([`child_main`]), never `cmd.exe`
//!   or a shell: the journey program and its arguments travel as JSON data and are spawned as argv,
//!   so no journey argument is ever parsed by a shell. Only one generated path appears on the task
//!   command line, quoted with the `CommandLineToArgvW` rules ([`quote_windows_arg`]) and
//!   XML-escaped ([`task_xml`]).
//! * The child redirects stdout and stderr to files in a private temp dir and writes the exit code
//!   file last, by rename (atomic). The parent polls, streams new output, and propagates the code.
//! * [`run_interactive`] owns a cleanup guard: the task is ended and deleted and the temp dir
//!   removed on success, error, timeout and unwinding.
//!
//! Account and token: `LogonType = InteractiveToken`, `UserId` = the current account, `RunLevel =
//! LeastPrivilege`. No password is stored, requested or accepted. Task Scheduler runs an
//! InteractiveToken task only for a user who is logged on, in that user's session.
use crate::util::{err, new_id, sleep_ms, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Opt-in environment variable (`1` or `true`).
pub const OPT_IN_ENV: &str = "RIGHTKIT_QA_INTERACTIVE";
/// Set by the child wrapper in the journey's environment so a journey never relaunches itself.
pub const CHILD_MARKER_ENV: &str = "RIGHTKIT_QA_INTERACTIVE_CHILD";
/// Task Scheduler folder every one-shot task lives in; the task name is `<folder><uuid>`.
pub const TASK_FOLDER: &str = "\\RightKit\\QA\\";
/// Exit code the wrapper reports when it could not start the journey at all.
pub const WRAPPER_FAILED: i32 = 125;
/// Files inside the job directory.
pub const SPEC_FILE: &str = "spec.json";
pub const OUT_FILE: &str = "stdout.log";
pub const ERR_FILE: &str = "stderr.log";
pub const EXIT_FILE: &str = "exit.txt";
pub const PID_FILE: &str = "pid.txt";
pub const TASK_XML_FILE: &str = "task.xml";

/// True when the flag is given or the environment value is `1` / `true`.
pub fn opted_in(flag: bool, env_value: Option<&str>) -> bool {
    flag || matches!(
        env_value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true")
    )
}

/// A Windows account, `domain\user` (domain may be empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub domain: String,
    pub user: String,
}

impl Account {
    pub fn new(domain: impl Into<String>, user: impl Into<String>) -> Self {
        Account {
            domain: domain.into(),
            user: user.into(),
        }
    }
    pub fn qualified(&self) -> String {
        if self.domain.is_empty() {
            self.user.clone()
        } else {
            format!("{}\\{}", self.domain, self.user)
        }
    }
}

/// Same account: user names equal ignoring case, and domains equal ignoring case unless either
/// side has none.
pub fn same_account(a: &Account, b: &Account) -> bool {
    a.user.to_lowercase() == b.user.to_lowercase()
        && (a.domain.is_empty()
            || b.domain.is_empty()
            || a.domain.to_lowercase() == b.domain.to_lowercase())
}

/// Who is logged on at the physical console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleUser {
    /// The query failed; the Task Scheduler decides.
    Unknown,
    /// No session, or a session with no user (the logon screen).
    NobodyLoggedOn,
    User(Account),
}

/// What the process can tell about where it runs. Pure data so [`decide`] is testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    pub session_id: Option<u32>,
    /// `Some(false)` when the process window station is not visible (service or SSH shape).
    pub visible_station: Option<bool>,
    pub console_user: ConsoleUser,
    pub current_user: Option<Account>,
}

/// `Some(reason)` when the process is outside the interactive desktop; `None` when interactive or
/// undeterminable (the same rule the launch hint uses).
pub fn non_interactive_reason(f: &SessionFacts) -> Option<String> {
    match (f.session_id, f.visible_station) {
        (Some(0), _) => Some("session 0".into()),
        (s, Some(false)) => Some(format!(
            "session {}, non-visible window station",
            s.map(|v| v.to_string()).unwrap_or_else(|| "?".into())
        )),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Already interactive (or not Windows): run the journey directly.
    Direct,
    /// Relaunch in the interactive session as this account.
    Relaunch(Account),
    NotOptedIn { reason: String },
    NoInteractiveUser { reason: String },
    WrongUser { current: String, console: String },
}

pub fn decide(f: &SessionFacts, opt_in: bool) -> Decision {
    let Some(reason) = non_interactive_reason(f) else {
        return Decision::Direct;
    };
    if !opt_in {
        return Decision::NotOptedIn { reason };
    }
    let console = match &f.console_user {
        ConsoleUser::NobodyLoggedOn => {
            return Decision::NoInteractiveUser { reason };
        }
        ConsoleUser::Unknown => None,
        ConsoleUser::User(a) => Some(a),
    };
    let Some(current) = f.current_user.clone() else {
        return Decision::WrongUser {
            current: "<unknown>".into(),
            console: console.map(|a| a.qualified()).unwrap_or_else(|| "<unknown>".into()),
        };
    };
    match console {
        Some(c) if !same_account(&current, c) => Decision::WrongUser {
            current: current.qualified(),
            console: c.qualified(),
        },
        _ => Decision::Relaunch(current),
    }
}

impl Decision {
    /// The error a caller reports for a decision that cannot proceed; `None` for Direct/Relaunch.
    pub fn refusal(&self) -> Option<String> {
        match self {
            Decision::Direct | Decision::Relaunch(_) => None,
            Decision::NotOptedIn { reason } => Some(format!(
                "this harness process is not in the interactive desktop session ({reason}); a hidden WebView2 app cannot get a ready window from here. Set {OPT_IN_ENV}=1 (or pass --interactive) to run the journey in the logged-on user's session through a one-shot Task Scheduler task"
            )),
            Decision::NoInteractiveUser { reason } => Some(format!(
                "this harness process is not in the interactive desktop session ({reason}) and nobody is logged on at the console, so there is no interactive session to run in. Log on at the machine and retry"
            )),
            Decision::WrongUser { current, console } => Some(format!(
                "the logged-on interactive user is {console} but this process runs as {current}; the interactive run is only started for the same account (no password is ever used). Sign in over SSH as {console}, or log the console user off"
            )),
        }
    }
}

/// Quote one argument for a Windows command line (`CommandLineToArgvW` rules).
pub fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

fn xml_escape(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {
                return err(format!(
                    "control character U+{:04X} cannot go into a task definition",
                    c as u32
                ));
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

/// What the generated task runs.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    /// `domain\user`; the same account as the caller (InteractiveToken).
    pub user: String,
    pub exe: PathBuf,
    /// Already-quoted command line arguments for `exe`.
    pub arguments: String,
    pub working_dir: PathBuf,
    pub time_limit_secs: u64,
    pub description: String,
}

/// The one-shot task definition: no triggers (started only by `/run`), InteractiveToken for the
/// given user, least privilege, no password anywhere, a hard execution time limit.
pub fn task_xml(t: &TaskSpec) -> Result<String> {
    let s = |p: &Path| -> Result<String> {
        match p.to_str() {
            Some(v) => xml_escape(v),
            None => err(format!("path is not valid Unicode: {}", p.display())),
        }
    };
    Ok(format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n",
            "<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n",
            "  <RegistrationInfo><Description>{desc}</Description></RegistrationInfo>\n",
            "  <Principals><Principal id=\"Author\"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n",
            "  <Settings>\n",
            "    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n",
            "    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n",
            "    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n",
            "    <AllowHardTerminate>true</AllowHardTerminate>\n",
            "    <StartWhenAvailable>false</StartWhenAvailable>\n",
            "    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\n",
            "    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>\n",
            "    <AllowStartOnDemand>true</AllowStartOnDemand>\n",
            "    <Enabled>true</Enabled>\n",
            "    <Hidden>false</Hidden>\n",
            "    <RunOnlyIfIdle>false</RunOnlyIfIdle>\n",
            "    <WakeToRun>false</WakeToRun>\n",
            "    <ExecutionTimeLimit>PT{limit}S</ExecutionTimeLimit>\n",
            "    <Priority>7</Priority>\n",
            "  </Settings>\n",
            "  <Actions Context=\"Author\"><Exec><Command>{cmd}</Command><Arguments>{args}</Arguments><WorkingDirectory>{wd}</WorkingDirectory></Exec></Actions>\n",
            "</Task>\n"
        ),
        desc = xml_escape(&t.description)?,
        user = xml_escape(&t.user)?,
        limit = t.time_limit_secs.max(1),
        cmd = s(&t.exe)?,
        args = xml_escape(&t.arguments)?,
        wd = s(&t.working_dir)?,
    ))
}

/// UTF-16 little endian with a byte order mark: the encoding `schtasks /xml` always accepts.
pub fn utf16le_with_bom(s: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// Parse the exit code file: one decimal integer. Windows codes above `i32::MAX` (for example
/// `0xC0000005`) are accepted as unsigned and returned with the same bit pattern as `i32`.
pub fn parse_exit_code(text: &str) -> Result<i32> {
    let t = text.trim();
    let n: i64 = match t.parse() {
        Ok(n) => n,
        Err(_) => return err(format!("exit code file is not a number: {t:?}")),
    };
    if n >= i32::MIN as i64 && n <= i32::MAX as i64 {
        Ok(n as i32)
    } else if n > i32::MAX as i64 && n <= u32::MAX as i64 {
        Ok(n as u32 as i32)
    } else {
        err(format!("exit code out of range: {n}"))
    }
}

/// Environment forwarded to the journey. Scheduled tasks do not inherit the SSH environment, so it
/// is explicit: every `RIGHTKIT_*` variable (except the opt-in and child markers), the variables
/// named in `pass` (case-insensitive), then `explicit` pairs, which win.
pub fn select_env(
    vars: impl IntoIterator<Item = (String, String)>,
    pass: &[String],
    explicit: &[(String, String)],
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (k, v) in vars {
        let upper = k.to_ascii_uppercase();
        let internal = upper == OPT_IN_ENV || upper == CHILD_MARKER_ENV;
        let named = pass.iter().any(|p| p.eq_ignore_ascii_case(&k));
        if !internal && (upper.starts_with("RIGHTKIT_") || named) {
            out.insert(k, v);
        }
    }
    for (k, v) in explicit {
        if k.is_empty() || k.contains(['=', '\0']) || v.contains('\0') {
            return err(format!("invalid environment entry {k:?}"));
        }
        out.insert(k.clone(), v.clone());
    }
    Ok(out)
}

/// Everything the child wrapper needs; written as JSON into the job directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChildSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub exit: PathBuf,
    pub pid: PathBuf,
}

/// The child wrapper, run inside the interactive session as `<exe> __child <spec.json>`. Reads and
/// deletes the spec, starts the journey with the files as stdout and stderr, records its pid,
/// waits, then writes the exit code file by rename. Returns the journey's exit code.
pub fn child_main(spec_path: &Path) -> i32 {
    let spec: ChildSpec = match std::fs::read(spec_path)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rightkit-qa-interactive: cannot read {}: {e}", spec_path.display());
            return WRAPPER_FAILED;
        }
    };
    // The spec may carry environment values; do not leave it around longer than needed.
    let _ = std::fs::remove_file(spec_path);
    let code = match run_child(&spec) {
        Ok(c) => c,
        Err(e) => {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&spec.stderr)
            {
                let _ = writeln!(f, "rightkit-qa-interactive: {e}");
            }
            WRAPPER_FAILED
        }
    };
    let tmp = spec.exit.with_extension("tmp");
    if std::fs::write(&tmp, format!("{code}\n")).is_ok() {
        let _ = std::fs::rename(&tmp, &spec.exit);
    }
    code
}

fn run_child(spec: &ChildSpec) -> Result<i32> {
    use std::process::{Command, Stdio};
    let out = File::create(&spec.stdout)?;
    let errf = File::create(&spec.stderr)?;
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(errf))
        .env_remove(OPT_IN_ENV)
        .env(CHILD_MARKER_ENV, "1");
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    cmd.envs(&spec.env);
    let mut child = cmd
        .spawn()
        .map_err(|e| crate::Error(format!("cannot start {}: {e}", spec.program)))?;
    let _ = std::fs::write(&spec.pid, child.id().to_string());
    let status = child.wait()?;
    Ok(status.code().unwrap_or(-1))
}

/// Task Scheduler operations, a trait so the orchestration is tested without a real scheduler.
pub trait Scheduler {
    /// Register the task `name` from the XML file (must fail if the name exists).
    fn create(&self, name: &str, xml_path: &Path) -> Result<()>;
    fn run(&self, name: &str) -> Result<()>;
    /// Best effort; never fails the caller.
    fn end(&self, name: &str);
    /// Best effort; never fails the caller.
    fn delete(&self, name: &str);
    /// Human diagnostic for a task that did not start (best effort).
    fn describe(&self, name: &str) -> String;
    /// Terminate the journey process tree by recorded pid.
    fn kill_tree(&self, pid: u32);
}

/// `schtasks.exe` from `%SystemRoot%\System32`, invoked with argv (no shell).
pub struct Schtasks;

impl Schtasks {
    fn exe() -> PathBuf {
        match std::env::var_os("SystemRoot") {
            Some(root) => PathBuf::from(root).join("System32").join("schtasks.exe"),
            None => PathBuf::from("schtasks.exe"),
        }
    }
    #[cfg(windows)]
    fn call(args: &[&str]) -> Result<(bool, String)> {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new(Self::exe())
            .args(args)
            .stdin(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| crate::Error(format!("cannot run schtasks.exe: {e}")))?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.success(), text.trim().to_string()))
    }
    #[cfg(not(windows))]
    fn call(_args: &[&str]) -> Result<(bool, String)> {
        let _ = Self::exe();
        err("the interactive Task Scheduler run is Windows only")
    }
}

impl Scheduler for Schtasks {
    fn create(&self, name: &str, xml_path: &Path) -> Result<()> {
        let xml = xml_path.to_string_lossy();
        let (ok, text) = Self::call(&["/create", "/tn", name, "/xml", &xml])?;
        if ok {
            Ok(())
        } else {
            err(format!("schtasks /create failed: {text}"))
        }
    }
    fn run(&self, name: &str) -> Result<()> {
        let (ok, text) = Self::call(&["/run", "/tn", name])?;
        if ok {
            Ok(())
        } else {
            err(format!("schtasks /run failed: {text}"))
        }
    }
    fn end(&self, name: &str) {
        let _ = Self::call(&["/end", "/tn", name]);
    }
    fn delete(&self, name: &str) {
        let _ = Self::call(&["/delete", "/tn", name, "/f"]);
    }
    fn describe(&self, name: &str) -> String {
        match Self::call(&["/query", "/tn", name, "/v", "/fo", "list"]) {
            Ok((_, text)) => crate::util::tail(&text, 1500),
            Err(e) => e.to_string(),
        }
    }
    fn kill_tree(&self, pid: u32) {
        crate::process::kill_tree(pid);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One interactive run.
#[derive(Debug, Clone)]
pub struct Request {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    /// Whole-run limit, also the task's execution time limit (plus a margin).
    pub timeout: Duration,
    /// This executable (it must implement `__child`, see the `rightkit-qa-interactive` binary).
    pub child_exe: PathBuf,
    pub account: Account,
    pub poll: Duration,
    /// How long the journey may take to write its pid file before the task counts as not started.
    pub start_grace: Duration,
}

/// Ends and deletes the task, kills a still-running journey and removes the job directory when
/// dropped: on success, error, timeout and unwinding alike.
struct Cleanup<'a, S: Scheduler> {
    sched: &'a S,
    task: Option<String>,
    dir: PathBuf,
}

impl<S: Scheduler> Drop for Cleanup<'_, S> {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            self.sched.end(task);
            if !self.dir.join(EXIT_FILE).exists() {
                if let Some(pid) = read_pid(&self.dir.join(PID_FILE)) {
                    self.sched.kill_tree(pid);
                }
            }
            self.sched.delete(task);
        }
        for _ in 0..10 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                break;
            }
            sleep_ms(100);
        }
    }
}

fn read_pid(p: &Path) -> Option<u32> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

struct Tail {
    path: PathBuf,
    offset: u64,
}

impl Tail {
    fn drain(&mut self, f: &mut dyn FnMut(&[u8])) {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut file) = File::open(&self.path) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
            self.offset += buf.len() as u64;
            f(&buf);
        }
    }
}

/// Run the request through `sched` and return the journey's exit code. Output is handed to `sink`
/// as it is written. Errors: task creation or start failure, the task never starting, the timeout
/// (the journey tree is killed), an unreadable exit file.
pub fn run_interactive<S: Scheduler>(
    sched: &S,
    req: &Request,
    sink: &mut dyn FnMut(Stream, &[u8]),
) -> Result<i32> {
    let id = new_id();
    let dir = std::env::temp_dir().join(format!("rightkit-qa-interactive-{id}"));
    std::fs::create_dir_all(&dir)?;
    let mut guard = Cleanup {
        sched,
        task: None,
        dir: dir.clone(),
    };
    let (out_p, err_p, exit_p, pid_p) = (
        dir.join(OUT_FILE),
        dir.join(ERR_FILE),
        dir.join(EXIT_FILE),
        dir.join(PID_FILE),
    );
    let spec = ChildSpec {
        program: req.program.clone(),
        args: req.args.clone(),
        cwd: req.cwd.clone(),
        env: req.env.clone(),
        stdout: out_p.clone(),
        stderr: err_p.clone(),
        exit: exit_p.clone(),
        pid: pid_p.clone(),
    };
    let spec_p = dir.join(SPEC_FILE);
    std::fs::write(&spec_p, serde_json::to_vec(&spec)?)?;
    let spec_str = match spec_p.to_str() {
        Some(s) => s.to_string(),
        None => return err("temp directory path is not valid Unicode"),
    };
    let xml = task_xml(&TaskSpec {
        user: req.account.qualified(),
        exe: req.child_exe.clone(),
        arguments: format!("__child {}", quote_windows_arg(&spec_str)),
        working_dir: dir.clone(),
        time_limit_secs: req.timeout.as_secs() + 30,
        description: format!("rightkit-qa one-shot interactive journey {id}"),
    })?;
    let xml_p = dir.join(TASK_XML_FILE);
    std::fs::write(&xml_p, utf16le_with_bom(&xml))?;

    let name = format!("{TASK_FOLDER}{id}");
    // Set before create so a half-created task is still deleted.
    guard.task = Some(name.clone());
    sched.create(&name, &xml_p)?;
    sched.run(&name)?;

    let mut tail_out = Tail {
        path: out_p,
        offset: 0,
    };
    let mut tail_err = Tail {
        path: err_p,
        offset: 0,
    };
    let start = Instant::now();
    let deadline = start + req.timeout;
    loop {
        tail_out.drain(&mut |b| sink(Stream::Stdout, b));
        tail_err.drain(&mut |b| sink(Stream::Stderr, b));
        if let Ok(text) = std::fs::read_to_string(&exit_p) {
            tail_out.drain(&mut |b| sink(Stream::Stdout, b));
            tail_err.drain(&mut |b| sink(Stream::Stderr, b));
            return parse_exit_code(&text);
        }
        if Instant::now() >= deadline {
            return err(format!(
                "interactive journey timed out after {}s (the task and the journey process were stopped)",
                req.timeout.as_secs()
            ));
        }
        if !pid_p.exists() && start.elapsed() >= req.start_grace {
            return err(format!(
                "the interactive task did not start the journey within {}s. It only runs while {} is logged on at the console. Task Scheduler says: {}",
                req.start_grace.as_secs(),
                req.account.qualified(),
                sched.describe(&name)
            ));
        }
        std::thread::sleep(req.poll);
    }
}

/// Facts about this process. Not Windows: nothing known, so [`decide`] returns `Direct`.
#[cfg(not(windows))]
pub fn session_facts() -> SessionFacts {
    SessionFacts {
        session_id: None,
        visible_station: None,
        console_user: ConsoleUser::Unknown,
        current_user: None,
    }
}

#[cfg(windows)]
pub fn session_facts() -> SessionFacts {
    use std::ffi::c_void;
    #[repr(C)]
    struct UserObjectFlags {
        inherit: i32,
        reserved: i32,
        flags: u32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetProcessWindowStation() -> *mut c_void;
        fn GetUserObjectInformationW(
            h: *mut c_void,
            index: i32,
            info: *mut c_void,
            len: u32,
            needed: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
        fn WTSGetActiveConsoleSessionId() -> u32;
    }
    #[link(name = "wtsapi32")]
    extern "system" {
        fn WTSQuerySessionInformationW(
            server: *mut c_void,
            session: u32,
            class: i32,
            buffer: *mut *mut u16,
            bytes: *mut u32,
        ) -> i32;
        fn WTSFreeMemory(p: *mut c_void);
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn GetUserNameW(buf: *mut u16, len: *mut u32) -> i32;
    }
    const UOI_FLAGS: i32 = 1;
    const WSF_VISIBLE: u32 = 1;
    const WTS_USER_NAME: i32 = 5;
    const WTS_DOMAIN_NAME: i32 = 7;
    const NO_SESSION: u32 = 0xFFFF_FFFF;

    // SAFETY: plain Win32 queries with correctly sized out-parameters; WTS buffers are freed.
    unsafe {
        let mut session = 0u32;
        let session_id = if ProcessIdToSessionId(GetCurrentProcessId(), &mut session) != 0 {
            Some(session)
        } else {
            None
        };
        let mut visible_station = None;
        let station = GetProcessWindowStation();
        if !station.is_null() {
            let mut info = UserObjectFlags {
                inherit: 0,
                reserved: 0,
                flags: 0,
            };
            let mut needed = 0u32;
            if GetUserObjectInformationW(
                station,
                UOI_FLAGS,
                &mut info as *mut _ as *mut c_void,
                std::mem::size_of::<UserObjectFlags>() as u32,
                &mut needed,
            ) != 0
            {
                visible_station = Some(info.flags & WSF_VISIBLE != 0);
            }
        }
        let wts = |sid: u32, class: i32| -> Option<String> {
            let mut buf: *mut u16 = std::ptr::null_mut();
            let mut bytes = 0u32;
            if WTSQuerySessionInformationW(std::ptr::null_mut(), sid, class, &mut buf, &mut bytes)
                == 0
                || buf.is_null()
            {
                return None;
            }
            let mut len = 0usize;
            while *buf.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(buf, len));
            WTSFreeMemory(buf as *mut c_void);
            Some(s)
        };
        let console = WTSGetActiveConsoleSessionId();
        let console_user = if console == NO_SESSION {
            ConsoleUser::NobodyLoggedOn
        } else {
            match wts(console, WTS_USER_NAME) {
                None => ConsoleUser::Unknown,
                Some(u) if u.trim().is_empty() => ConsoleUser::NobodyLoggedOn,
                Some(u) => ConsoleUser::User(Account::new(
                    wts(console, WTS_DOMAIN_NAME).unwrap_or_default(),
                    u,
                )),
            }
        };
        let mut name = [0u16; 257];
        let mut n = name.len() as u32;
        let current_user = if GetUserNameW(name.as_mut_ptr(), &mut n) != 0 && n > 1 {
            let user = String::from_utf16_lossy(&name[..(n as usize - 1)]);
            let domain = std::env::var("USERDOMAIN").unwrap_or_default();
            Some(Account::new(domain, user))
        } else {
            None
        };
        SessionFacts {
            session_id,
            visible_station,
            console_user,
            current_user,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn acct(d: &str, u: &str) -> Account {
        Account::new(d, u)
    }

    fn facts(session: u32, visible: bool, console: ConsoleUser, cur: Option<Account>) -> SessionFacts {
        SessionFacts {
            session_id: Some(session),
            visible_station: Some(visible),
            console_user: console,
            current_user: cur,
        }
    }

    #[test]
    fn quoting_follows_commandlinetoargvw() {
        assert_eq!(quote_windows_arg("plain"), "plain");
        assert_eq!(quote_windows_arg(""), "\"\"");
        assert_eq!(quote_windows_arg("a b"), "\"a b\"");
        assert_eq!(quote_windows_arg(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_windows_arg(r"C:\dir with\"), r#""C:\dir with\\""#);
        assert_eq!(quote_windows_arg(r#"x\"y z"#), r#""x\\\"y z""#);
        assert_eq!(quote_windows_arg(r"C:\a\b"), r"C:\a\b");
        // Shell metacharacters need no special handling: nothing here is ever parsed by a shell.
        assert_eq!(quote_windows_arg("a&b"), "a&b");
    }

    #[test]
    fn task_xml_is_interactive_token_without_password() {
        let xml = task_xml(&TaskSpec {
            user: "PC\\Ann & Co".into(),
            exe: PathBuf::from("C:\\bin\\rightkit-qa-interactive.exe"),
            arguments: "__child \"C:\\t\\a b\\spec.json\"".into(),
            working_dir: PathBuf::from("C:\\t\\a b"),
            time_limit_secs: 330,
            description: "x <y>".into(),
        })
        .unwrap();
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains("<UserId>PC\\Ann &amp; Co</UserId>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT330S</ExecutionTimeLimit>"));
        assert!(xml.contains("<Arguments>__child &quot;C:\\t\\a b\\spec.json&quot;</Arguments>"));
        assert!(xml.contains("x &lt;y&gt;"));
        assert!(!xml.contains("Password"));
        assert!(!xml.contains("<Triggers"));
        assert!(!xml.contains("cmd.exe"));
    }

    #[test]
    fn task_xml_rejects_control_characters() {
        let r = task_xml(&TaskSpec {
            user: "u\u{1}".into(),
            exe: PathBuf::from("x"),
            arguments: String::new(),
            working_dir: PathBuf::from("y"),
            time_limit_secs: 5,
            description: String::new(),
        });
        assert!(r.is_err());
    }

    #[test]
    fn utf16_has_bom_and_little_endian() {
        assert_eq!(utf16le_with_bom("A\u{e9}"), vec![0xFF, 0xFE, 0x41, 0x00, 0xE9, 0x00]);
    }

    #[test]
    fn exit_code_file_parsing() {
        assert_eq!(parse_exit_code("0\n").unwrap(), 0);
        assert_eq!(parse_exit_code(" 42 \r\n").unwrap(), 42);
        assert_eq!(parse_exit_code("-1").unwrap(), -1);
        assert_eq!(parse_exit_code("3221225477").unwrap(), 0xC000_0005u32 as i32);
        assert!(parse_exit_code("").is_err());
        assert!(parse_exit_code("abc").is_err());
        assert!(parse_exit_code("4294967296").is_err());
    }

    #[test]
    fn opt_in_rules() {
        assert!(opted_in(true, None));
        assert!(opted_in(false, Some("1")));
        assert!(opted_in(false, Some(" TRUE ")));
        assert!(!opted_in(false, Some("0")));
        assert!(!opted_in(false, Some("yes please")));
        assert!(!opted_in(false, None));
    }

    #[test]
    fn session_zero_decision() {
        let me = acct("PC", "ann");
        let interactive = facts(1, true, ConsoleUser::User(me.clone()), Some(me.clone()));
        assert_eq!(decide(&interactive, true), Decision::Direct);
        assert_eq!(decide(&interactive, false), Decision::Direct);

        let ssh = facts(0, false, ConsoleUser::User(acct("pc", "ANN")), Some(me.clone()));
        assert!(matches!(decide(&ssh, false), Decision::NotOptedIn { .. }));
        assert!(decide(&ssh, false).refusal().unwrap().contains(OPT_IN_ENV));
        assert_eq!(decide(&ssh, true), Decision::Relaunch(me.clone()));

        // A non-visible station outside session 0 is the same shape.
        let svc = facts(2, false, ConsoleUser::User(me.clone()), Some(me.clone()));
        assert!(matches!(decide(&svc, false), Decision::NotOptedIn { .. }));

        let other = facts(0, false, ConsoleUser::User(acct("PC", "bob")), Some(me.clone()));
        match decide(&other, true) {
            Decision::WrongUser { current, console } => {
                assert_eq!(current, "PC\\ann");
                assert_eq!(console, "PC\\bob");
            }
            d => panic!("{d:?}"),
        }
        let nobody = facts(0, false, ConsoleUser::NobodyLoggedOn, Some(me.clone()));
        assert!(matches!(decide(&nobody, true), Decision::NoInteractiveUser { .. }));
        // Unknown console user: the Task Scheduler enforces the account.
        let unknown = facts(0, false, ConsoleUser::Unknown, Some(me.clone()));
        assert_eq!(decide(&unknown, true), Decision::Relaunch(me.clone()));
        let nocur = facts(0, false, ConsoleUser::Unknown, None);
        assert!(matches!(decide(&nocur, true), Decision::WrongUser { .. }));
        // Undeterminable facts never block a run.
        let none = SessionFacts {
            session_id: None,
            visible_station: None,
            console_user: ConsoleUser::Unknown,
            current_user: None,
        };
        assert_eq!(decide(&none, false), Decision::Direct);
    }

    #[test]
    fn account_comparison() {
        assert!(same_account(&acct("PC", "Ann"), &acct("pc", "ann")));
        assert!(same_account(&acct("", "ann"), &acct("PC", "ann")));
        assert!(!same_account(&acct("PC", "ann"), &acct("OTHER", "ann")));
        assert!(!same_account(&acct("PC", "ann"), &acct("PC", "bob")));
    }

    #[test]
    fn env_selection_is_explicit() {
        let vars = vec![
            ("RIGHTKIT_MANAGED_ROOT".to_string(), "r".to_string()),
            ("RIGHTKIT_QA_INTERACTIVE".to_string(), "1".to_string()),
            ("rightkit_qa_interactive_child".to_string(), "1".to_string()),
            ("Path".to_string(), "p".to_string()),
            ("SECRET".to_string(), "s".to_string()),
            ("Keep".to_string(), "k".to_string()),
        ];
        let got = select_env(
            vars,
            &["keep".to_string()],
            &[("RIGHTKIT_MANAGED_ROOT".into(), "override".into()), ("X".into(), "y".into())],
        )
        .unwrap();
        assert_eq!(got.get("RIGHTKIT_MANAGED_ROOT").map(String::as_str), Some("override"));
        assert_eq!(got.get("Keep").map(String::as_str), Some("k"));
        assert_eq!(got.get("X").map(String::as_str), Some("y"));
        assert!(!got.contains_key("Path") && !got.contains_key("SECRET"));
        assert!(!got.contains_key("RIGHTKIT_QA_INTERACTIVE"));
        assert!(!got.contains_key("rightkit_qa_interactive_child"));
        assert!(select_env(Vec::new(), &[], &[("A=B".into(), "c".into())]).is_err());
        assert!(select_env(Vec::new(), &[], &[(String::new(), "c".into())]).is_err());
    }

    #[test]
    fn child_spec_roundtrips_hostile_arguments_as_data() {
        let spec = ChildSpec {
            program: "journey.exe".into(),
            args: vec!["a b".into(), "\"; calc & ".into(), "%PATH%".into(), "$(x)".into()],
            cwd: None,
            env: BTreeMap::from([("K".to_string(), "v\nw".to_string())]),
            stdout: "o".into(),
            stderr: "e".into(),
            exit: "x".into(),
            pid: "p".into(),
        };
        let back: ChildSpec = serde_json::from_slice(&serde_json::to_vec(&spec).unwrap()).unwrap();
        assert_eq!(back, spec);
    }

    /// Records calls and plays the child by writing the result files next to the task XML.
    struct Fake {
        calls: RefCell<Vec<String>>,
        dir: RefCell<Option<PathBuf>>,
        fail_create: bool,
        behave: Option<(&'static str, &'static str, &'static str)>,
    }

    impl Fake {
        fn new(behave: Option<(&'static str, &'static str, &'static str)>) -> Self {
            Fake {
                calls: RefCell::new(vec![]),
                dir: RefCell::new(None),
                fail_create: false,
                behave,
            }
        }
        fn log(&self, s: &str) {
            self.calls.borrow_mut().push(s.to_string());
        }
    }

    impl Scheduler for Fake {
        fn create(&self, name: &str, xml_path: &Path) -> Result<()> {
            assert!(name.starts_with(TASK_FOLDER));
            *self.dir.borrow_mut() = xml_path.parent().map(|p| p.to_path_buf());
            self.log("create");
            if self.fail_create {
                return err("denied");
            }
            Ok(())
        }
        fn run(&self, _name: &str) -> Result<()> {
            self.log("run");
            if let (Some((out, errs, code)), Some(dir)) = (self.behave, self.dir.borrow().clone()) {
                std::fs::write(dir.join(OUT_FILE), out).unwrap();
                std::fs::write(dir.join(ERR_FILE), errs).unwrap();
                std::fs::write(dir.join(PID_FILE), "1234").unwrap();
                std::fs::write(dir.join(EXIT_FILE), code).unwrap();
            }
            Ok(())
        }
        fn end(&self, _name: &str) {
            self.log("end");
        }
        fn delete(&self, _name: &str) {
            self.log("delete");
        }
        fn describe(&self, _name: &str) -> String {
            "no such task".into()
        }
        fn kill_tree(&self, pid: u32) {
            self.log(&format!("kill {pid}"));
        }
    }

    fn request(timeout_ms: u64, grace_ms: u64) -> Request {
        Request {
            program: "journey.exe".into(),
            args: vec!["--x".into()],
            cwd: None,
            env: BTreeMap::new(),
            timeout: Duration::from_millis(timeout_ms),
            child_exe: PathBuf::from("rightkit-qa-interactive.exe"),
            account: acct("PC", "ann"),
            poll: Duration::from_millis(10),
            start_grace: Duration::from_millis(grace_ms),
        }
    }

    #[test]
    fn success_streams_output_propagates_code_and_cleans_up() {
        let fake = Fake::new(Some(("hello\n", "warn\n", "3\n")));
        let mut got: Vec<(Stream, Vec<u8>)> = vec![];
        let code = run_interactive(&fake, &request(5000, 5000), &mut |s, b| got.push((s, b.to_vec())))
            .unwrap();
        assert_eq!(code, 3);
        assert!(got.contains(&(Stream::Stdout, b"hello\n".to_vec())));
        assert!(got.contains(&(Stream::Stderr, b"warn\n".to_vec())));
        let calls = fake.calls.borrow().clone();
        assert_eq!(calls, vec!["create", "run", "end", "delete"]);
        assert!(!fake.dir.borrow().clone().unwrap().exists());
    }

    #[test]
    fn timeout_kills_deletes_and_removes_files() {
        let fake = Fake::new(None);
        let r = run_interactive(&fake, &request(150, 60_000), &mut |_, _| {});
        let e = r.unwrap_err().to_string();
        assert!(e.contains("timed out"), "{e}");
        let calls = fake.calls.borrow().clone();
        assert_eq!(calls.last().map(String::as_str), Some("delete"));
        assert!(calls.contains(&"end".to_string()));
        assert!(!fake.dir.borrow().clone().unwrap().exists());
    }

    #[test]
    fn task_that_never_starts_is_reported_and_cleaned() {
        let fake = Fake::new(None);
        let e = run_interactive(&fake, &request(60_000, 50), &mut |_, _| {})
            .unwrap_err()
            .to_string();
        assert!(e.contains("did not start"), "{e}");
        assert!(e.contains("no such task"), "{e}");
        assert_eq!(fake.calls.borrow().last().map(String::as_str), Some("delete"));
        assert!(!fake.dir.borrow().clone().unwrap().exists());
    }

    #[test]
    fn failed_create_still_deletes_and_cleans() {
        let mut fake = Fake::new(None);
        fake.fail_create = true;
        let e = run_interactive(&fake, &request(1000, 1000), &mut |_, _| {})
            .unwrap_err()
            .to_string();
        assert!(e.contains("denied"));
        assert_eq!(fake.calls.borrow().last().map(String::as_str), Some("delete"));
        assert!(!fake.dir.borrow().clone().unwrap().exists());
    }

    #[test]
    fn unparsable_exit_file_is_an_error_not_success() {
        let fake = Fake::new(Some(("", "", "oops")));
        assert!(run_interactive(&fake, &request(1000, 1000), &mut |_, _| {}).is_err());
        assert_eq!(fake.calls.borrow().last().map(String::as_str), Some("delete"));
    }
}
