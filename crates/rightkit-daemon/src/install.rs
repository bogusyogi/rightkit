//! Install, start, stop, query and remove a user-level background service (rightkit#405):
//! launchd on macOS, `systemd --user` on Linux and a Task Scheduler task on Windows, behind one
//! trait.
//!
//! Every manager works through a [`CommandRunner`], the one place a command line leaves the
//! process, so a host or a test can supply its own and the OS service manager is never needed to
//! exercise the code that builds the unit file, the plist or the task XML. [`SystemRunner`] is the
//! real one.
//!
//! What is written and run is built from a [`ServiceSpec`] and nothing else: the service name is
//! restricted to `[A-Za-z0-9._-]`, every value is escaped for the file format it lands in (XML,
//! plist, systemd), and a program path must be absolute. The unit files are written atomically,
//! owner-only. Nothing here asks for administrator rights: the services are per-user.

use crate::record::write_atomic;
use std::io;
use std::path::{Path, PathBuf};

/// When the service is started again after it exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RestartPolicy {
    /// Never (run at login only).
    Never,
    /// After a non-zero exit.
    #[default]
    OnFailure,
    /// After any exit.
    Always,
}

/// What to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// The service name: `[A-Za-z0-9._-]`, at most 128 characters. launchd uses it as the label.
    pub name: String,
    /// The program: an absolute path.
    pub exec: PathBuf,
    pub args: Vec<String>,
    /// Environment variables for the service. Not supported by Task Scheduler (refused there).
    pub env: Vec<(String, String)>,
    pub restart: RestartPolicy,
}

impl ServiceSpec {
    pub fn new(name: impl Into<String>, exec: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            exec: exec.into(),
            args: Vec::new(),
            env: Vec::new(),
            restart: RestartPolicy::default(),
        }
    }

    fn validate(&self) -> io::Result<()> {
        let bad = |what: &str| {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                what.to_string(),
            ))
        };
        if self.name.is_empty()
            || self.name.len() > 128
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return bad("a service name is 1 to 128 characters of letters, digits, . _ -");
        }
        if !self.exec.is_absolute() {
            return bad("the service program must be an absolute path");
        }
        let text_ok = |s: &str| !s.contains('\0') && !s.contains('\n') && !s.contains('\r');
        if !text_ok(&self.exec.to_string_lossy())
            || !self.args.iter().all(|a| text_ok(a))
            || !self
                .env
                .iter()
                .all(|(k, v)| text_ok(v) && !k.is_empty() && !k.contains('=') && text_ok(k))
        {
            return bad("service arguments and environment hold no NUL or line break");
        }
        Ok(())
    }
}

/// What a manager says about a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServiceState {
    NotInstalled,
    /// Installed and not running.
    Stopped,
    /// Running; the process id when the manager reports one.
    Running {
        pid: Option<u32>,
    },
}

/// The result of one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

/// Runs a command line for a manager. The seam: replace it to observe or fake the OS tools.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput>;
}

/// Runs the real command (no shell, no window on Windows).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[String]) -> io::Result<CommandOutput> {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output()?;
        Ok(CommandOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// One user-level service manager.
pub trait ServiceManager: Send + Sync {
    /// Write the definition and register it (it starts at the next login, or at `start`).
    fn install(&self, spec: &ServiceSpec) -> io::Result<()>;
    fn start(&self, name: &str) -> io::Result<()>;
    fn stop(&self, name: &str) -> io::Result<()>;
    fn status(&self, name: &str) -> io::Result<ServiceState>;
    /// Stop, unregister and delete the definition. A service that is not installed is not an
    /// error.
    fn uninstall(&self, name: &str) -> io::Result<()>;
}

fn check_name(name: &str) -> io::Result<()> {
    ServiceSpec::new(name, "/x").validate()
}

fn failed(what: &str, out: &CommandOutput) -> io::Error {
    io::Error::other(format!(
        "{what} failed ({}): {}",
        out.code,
        out.stderr.trim()
    ))
}

fn run_ok(runner: &dyn CommandRunner, what: &str, program: &str, args: &[&str]) -> io::Result<()> {
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let out = runner.run(program, &args)?;
    if out.success() {
        Ok(())
    } else {
        Err(failed(what, &out))
    }
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// launchd (macOS)
// ---------------------------------------------------------------------------------------------

/// A launchd user agent: `~/Library/LaunchAgents/<name>.plist`, loaded into `gui/<uid>`.
pub struct LaunchAgent {
    runner: Box<dyn CommandRunner>,
    agents_dir: PathBuf,
    uid: u32,
}

impl LaunchAgent {
    /// The real manager for the current user.
    #[cfg(unix)]
    pub fn for_current_user() -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
            .ok_or_else(|| io::Error::other("no absolute HOME"))?;
        Ok(Self::with_parts(
            Box::new(SystemRunner),
            home.join("Library").join("LaunchAgents"),
            crate::transport::current_uid(),
        ))
    }

    /// A manager over an explicit runner, agents directory and uid.
    pub fn with_parts(runner: Box<dyn CommandRunner>, agents_dir: PathBuf, uid: u32) -> Self {
        Self {
            runner,
            agents_dir,
            uid,
        }
    }

    fn plist_path(&self, name: &str) -> PathBuf {
        self.agents_dir.join(format!("{name}.plist"))
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn target(&self, name: &str) -> String {
        format!("gui/{}/{name}", self.uid)
    }

    /// The plist text for `spec`.
    pub fn plist(spec: &ServiceSpec) -> String {
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
        );
        out.push_str(&format!(
            "  <key>Label</key>\n  <string>{}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string>\n",
            xml_escape(&spec.name),
            xml_escape(&spec.exec.to_string_lossy())
        ));
        for arg in &spec.args {
            out.push_str(&format!("    <string>{}</string>\n", xml_escape(arg)));
        }
        out.push_str("  </array>\n  <key>RunAtLoad</key>\n  <true/>\n");
        if !spec.env.is_empty() {
            out.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
            for (k, v) in &spec.env {
                out.push_str(&format!(
                    "    <key>{}</key>\n    <string>{}</string>\n",
                    xml_escape(k),
                    xml_escape(v)
                ));
            }
            out.push_str("  </dict>\n");
        }
        match spec.restart {
            RestartPolicy::Never => {}
            RestartPolicy::Always => out.push_str("  <key>KeepAlive</key>\n  <true/>\n"),
            RestartPolicy::OnFailure => out.push_str(
                "  <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n",
            ),
        }
        out.push_str("</dict>\n</plist>\n");
        out
    }
}

impl ServiceManager for LaunchAgent {
    fn install(&self, spec: &ServiceSpec) -> io::Result<()> {
        spec.validate()?;
        std::fs::create_dir_all(&self.agents_dir)?;
        let path = self.plist_path(&spec.name);
        write_atomic(&path, Self::plist(spec).as_bytes())?;
        // Replace a loaded copy: bootout fails when nothing is loaded, which is fine.
        let _ = self
            .runner
            .run("launchctl", &["bootout".into(), self.target(&spec.name)]);
        run_ok(
            &*self.runner,
            "launchctl bootstrap",
            "launchctl",
            &["bootstrap", &self.domain(), &path.to_string_lossy()],
        )
    }

    fn start(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        run_ok(
            &*self.runner,
            "launchctl kickstart",
            "launchctl",
            &["kickstart", "-k", &self.target(name)],
        )
    }

    fn stop(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        // `kill` ends the process; KeepAlive may start it again, which is the policy's call. A
        // service that is not loaded is already stopped.
        let out = self.runner.run(
            "launchctl",
            &["kill".into(), "SIGTERM".into(), self.target(name)],
        )?;
        if out.success() || !matches!(self.status(name)?, ServiceState::Running { .. }) {
            Ok(())
        } else {
            Err(failed("launchctl kill", &out))
        }
    }

    fn status(&self, name: &str) -> io::Result<ServiceState> {
        check_name(name)?;
        let out = self
            .runner
            .run("launchctl", &["print".into(), self.target(name)])?;
        if !out.success() {
            return Ok(if self.plist_path(name).exists() {
                ServiceState::Stopped
            } else {
                ServiceState::NotInstalled
            });
        }
        let pid = out
            .stdout
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pid = "))
            .find_map(|p| p.trim().parse::<u32>().ok());
        Ok(match pid {
            Some(pid) => ServiceState::Running { pid: Some(pid) },
            None => ServiceState::Stopped,
        })
    }

    fn uninstall(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        let _ = self
            .runner
            .run("launchctl", &["bootout".into(), self.target(name)]);
        match std::fs::remove_file(self.plist_path(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// systemd --user (Linux)
// ---------------------------------------------------------------------------------------------

/// A `systemd --user` service: `<config dir>/systemd/user/<name>.service`.
pub struct SystemdUser {
    runner: Box<dyn CommandRunner>,
    unit_dir: PathBuf,
}

impl SystemdUser {
    /// The real manager for the current user (`$XDG_CONFIG_HOME` or `~/.config`).
    pub fn for_current_user() -> io::Result<Self> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|h| h.is_absolute())
                    .map(|h| h.join(".config"))
            })
            .ok_or_else(|| io::Error::other("no absolute HOME or XDG_CONFIG_HOME"))?;
        Ok(Self::with_parts(
            Box::new(SystemRunner),
            base.join("systemd").join("user"),
        ))
    }

    pub fn with_parts(runner: Box<dyn CommandRunner>, unit_dir: PathBuf) -> Self {
        Self { runner, unit_dir }
    }

    fn unit_path(&self, name: &str) -> PathBuf {
        self.unit_dir.join(format!("{name}.service"))
    }

    fn quote(arg: &str) -> String {
        let mut out = String::from("\"");
        for c in arg.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '%' => out.push_str("%%"),
                '$' => out.push_str("$$"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// The unit text for `spec`.
    pub fn unit(spec: &ServiceSpec) -> String {
        let mut exec = Self::quote(&spec.exec.to_string_lossy());
        for arg in &spec.args {
            exec.push(' ');
            exec.push_str(&Self::quote(arg));
        }
        let mut out = format!(
            "[Unit]\nDescription={}\n\n[Service]\nExecStart={exec}\n",
            spec.name
        );
        for (k, v) in &spec.env {
            out.push_str(&format!(
                "Environment={}\n",
                Self::quote(&format!("{k}={v}"))
            ));
        }
        out.push_str(match spec.restart {
            RestartPolicy::Never => "Restart=no\n",
            RestartPolicy::OnFailure => "Restart=on-failure\n",
            RestartPolicy::Always => "Restart=always\n",
        });
        out.push_str("\n[Install]\nWantedBy=default.target\n");
        out
    }
}

impl ServiceManager for SystemdUser {
    fn install(&self, spec: &ServiceSpec) -> io::Result<()> {
        spec.validate()?;
        std::fs::create_dir_all(&self.unit_dir)?;
        write_atomic(&self.unit_path(&spec.name), Self::unit(spec).as_bytes())?;
        run_ok(
            &*self.runner,
            "systemctl daemon-reload",
            "systemctl",
            &["--user", "daemon-reload"],
        )?;
        run_ok(
            &*self.runner,
            "systemctl enable",
            "systemctl",
            &["--user", "enable", &format!("{}.service", spec.name)],
        )
    }

    fn start(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        run_ok(
            &*self.runner,
            "systemctl start",
            "systemctl",
            &["--user", "start", &format!("{name}.service")],
        )
    }

    fn stop(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        run_ok(
            &*self.runner,
            "systemctl stop",
            "systemctl",
            &["--user", "stop", &format!("{name}.service")],
        )
    }

    fn status(&self, name: &str) -> io::Result<ServiceState> {
        check_name(name)?;
        if !self.unit_path(name).exists() {
            return Ok(ServiceState::NotInstalled);
        }
        let unit = format!("{name}.service");
        let active = self.runner.run(
            "systemctl",
            &["--user".into(), "is-active".into(), unit.clone()],
        )?;
        if active.stdout.trim() != "active" {
            return Ok(ServiceState::Stopped);
        }
        let pid = self
            .runner
            .run(
                "systemctl",
                &[
                    "--user".into(),
                    "show".into(),
                    "-p".into(),
                    "MainPID".into(),
                    "--value".into(),
                    unit,
                ],
            )
            .ok()
            .and_then(|o| o.stdout.trim().parse::<u32>().ok())
            .filter(|p| *p != 0);
        Ok(ServiceState::Running { pid })
    }

    fn uninstall(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        let unit = format!("{name}.service");
        let _ = self
            .runner
            .run("systemctl", &["--user".into(), "stop".into(), unit.clone()]);
        let _ = self
            .runner
            .run("systemctl", &["--user".into(), "disable".into(), unit]);
        match std::fs::remove_file(self.unit_path(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let _ = self
            .runner
            .run("systemctl", &["--user".into(), "daemon-reload".into()]);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Task Scheduler (Windows)
// ---------------------------------------------------------------------------------------------

/// A Task Scheduler task that starts the program at the user's logon, in the user's own session
/// with the user's own rights (`LeastPrivilege`): no administrator rights are asked or granted.
/// The task definition is written as XML under `xml_dir` and registered with `schtasks`.
pub struct ScheduledTask {
    runner: Box<dyn CommandRunner>,
    xml_dir: PathBuf,
}

impl ScheduledTask {
    /// The real manager; the XML is staged under `xml_dir` (an existing owner-only directory).
    pub fn new(xml_dir: impl Into<PathBuf>) -> Self {
        Self::with_parts(Box::new(SystemRunner), xml_dir.into())
    }

    pub fn with_parts(runner: Box<dyn CommandRunner>, xml_dir: PathBuf) -> Self {
        Self { runner, xml_dir }
    }

    /// One Windows command-line argument, quoted as `CommandLineToArgvW` reads it.
    fn quote(arg: &str) -> String {
        if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
            return arg.to_string();
        }
        let mut out = String::from("\"");
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(backslashes * 2 + 1));
                    out.push('"');
                    backslashes = 0;
                }
                c => {
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

    /// The task XML for `spec`. A spec with environment variables is refused: a scheduled task
    /// has none.
    pub fn task_xml(spec: &ServiceSpec) -> io::Result<String> {
        spec.validate()?;
        if !spec.env.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a scheduled task cannot set environment variables",
            ));
        }
        let arguments = spec
            .args
            .iter()
            .map(|a| Self::quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        let restart = match spec.restart {
            RestartPolicy::Never => String::new(),
            _ => "<RestartOnFailure><Interval>PT1M</Interval><Count>999</Count></RestartOnFailure>"
                .to_string(),
        };
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n  <RegistrationInfo><Description>{name}</Description></RegistrationInfo>\n  <Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers>\n  <Principals><Principal id=\"Author\"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n  <Settings>\n    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n    <Hidden>true</Hidden>\n    {restart}\n  </Settings>\n  <Actions Context=\"Author\"><Exec><Command>{exec}</Command><Arguments>{arguments}</Arguments></Exec></Actions>\n</Task>\n",
            name = xml_escape(&spec.name),
            exec = xml_escape(&spec.exec.to_string_lossy()),
            arguments = xml_escape(&arguments),
        ))
    }
}

/// `schtasks /Create /XML` reads the file in its own encoding; the file is UTF-16 LE with a BOM,
/// which the declaration above says.
fn utf16_le_with_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

impl ServiceManager for ScheduledTask {
    fn install(&self, spec: &ServiceSpec) -> io::Result<()> {
        let xml = Self::task_xml(spec)?;
        std::fs::create_dir_all(&self.xml_dir)?;
        let path = self.xml_dir.join(format!("{}.xml", spec.name));
        write_atomic(&path, &utf16_le_with_bom(&xml))?;
        let result = run_ok(
            &*self.runner,
            "schtasks /Create",
            "schtasks",
            &[
                "/Create",
                "/TN",
                &spec.name,
                "/XML",
                &path.to_string_lossy(),
                "/F",
            ],
        );
        let _ = std::fs::remove_file(&path);
        result
    }

    fn start(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        run_ok(
            &*self.runner,
            "schtasks /Run",
            "schtasks",
            &["/Run", "/TN", name],
        )
    }

    fn stop(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        let out = self
            .runner
            .run("schtasks", &["/End".into(), "/TN".into(), name.to_string()])?;
        // A task that is not running is already stopped.
        if out.success() || !matches!(self.status(name)?, ServiceState::Running { .. }) {
            Ok(())
        } else {
            Err(failed("schtasks /End", &out))
        }
    }

    fn status(&self, name: &str) -> io::Result<ServiceState> {
        check_name(name)?;
        let out = self.runner.run(
            "schtasks",
            &[
                "/Query".into(),
                "/TN".into(),
                name.to_string(),
                "/FO".into(),
                "LIST".into(),
                "/V".into(),
            ],
        )?;
        if !out.success() {
            return Ok(ServiceState::NotInstalled);
        }
        let running = out.stdout.lines().any(|l| {
            l.split_once(':').is_some_and(|(k, v)| {
                k.trim().eq_ignore_ascii_case("Status") && v.trim().eq_ignore_ascii_case("Running")
            })
        });
        Ok(if running {
            ServiceState::Running { pid: None }
        } else {
            ServiceState::Stopped
        })
    }

    fn uninstall(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        let _ = self
            .runner
            .run("schtasks", &["/End".into(), "/TN".into(), name.to_string()]);
        let out = self.runner.run(
            "schtasks",
            &[
                "/Delete".into(),
                "/TN".into(),
                name.to_string(),
                "/F".into(),
            ],
        )?;
        if out.success() || matches!(self.status(name)?, ServiceState::NotInstalled) {
            Ok(())
        } else {
            Err(failed("schtasks /Delete", &out))
        }
    }
}

/// The manager for this platform: launchd, `systemd --user` or Task Scheduler (its XML is staged
/// under `xml_dir`, which only Windows uses).
pub fn current_manager(xml_dir: &Path) -> io::Result<Box<dyn ServiceManager>> {
    #[cfg(target_os = "macos")]
    {
        let _ = xml_dir;
        Ok(Box::new(LaunchAgent::for_current_user()?))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = xml_dir;
        Ok(Box::new(SystemdUser::for_current_user()?))
    }
    #[cfg(windows)]
    {
        Ok(Box::new(ScheduledTask::new(xml_dir)))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = xml_dir;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no user service manager for this platform",
        ))
    }
}
