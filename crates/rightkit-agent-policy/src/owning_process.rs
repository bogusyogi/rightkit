//! Owning-process protection for shell commands (CR1 / issue #53).
//!
//! An agent shell command must not be able to terminate the host app daemon or
//! host process, its process group, or any host process by name. This
//! check is an unconditional deny: it applies in every permission mode
//! (including bypass) because it protects the runtime that enforces policy.
//!
//! The protected identity comes only from the running process itself
//! ([`std::process::id`] and, on Unix, `getpgrp`). It is never read from the
//! network or from agent-supplied input.
//!
//! Scope is deliberately narrow. Killing any other user process stays under the
//! normal approval path. The matcher is lexical: it recognises kill-class
//! commands in command position (directly or behind common wrappers/shells) and
//! checks their arguments for the protected pid, process group, broadcast
//! targets (`-1`, `0`), `$PPID`, or a host binary name. It cannot see
//! through arbitrary indirection (computed pids, scripts on disk); OS-level
//! process-group isolation of spawned shells remains the backstop.

use crate::shell_stages::split_pipelines;

/// Identity of the process the policy must keep alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwningProcess {
    pub pid: u32,
    /// Process group id when the platform has one (Unix).
    pub pgid: Option<u32>,
}

impl OwningProcess {
    /// The current process, read from the OS.
    pub fn current() -> Self {
        Self {
            pid: std::process::id(),
            pgid: current_pgid(),
        }
    }
}

#[cfg(unix)]
fn current_pgid() -> Option<u32> {
    // SAFETY: getpgrp has no preconditions and cannot fail.
    let pgid = unsafe { libc::getpgrp() };
    u32::try_from(pgid).ok()
}

#[cfg(not(unix))]
fn current_pgid() -> Option<u32> {
    None
}

/// Base binary name used until the host calls [`set_protected_process_name`].
pub const DEFAULT_PROTECTED_PROCESS_NAME: &str = "agenthost";

static PROTECTED_BASE: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Set the host's own binary base name (for example `myapp`). The protected
/// set becomes `myapp`, `myappd`, `myapp-daemon`, `myapp-cli`, `myapp-tui` and
/// `myapp-host`; lifecycle commands (`myapp stop`) and service-manager commands
/// naming it are refused. Call once at startup, before any policy check.
pub fn set_protected_process_name(base: &str) {
    let base = base.trim().to_ascii_lowercase();
    if base.is_empty() {
        return;
    }
    if let Ok(mut slot) = PROTECTED_BASE.write() {
        *slot = Some(base);
    }
}

/// The current protected base name.
pub fn protected_process_name() -> String {
    PROTECTED_BASE
        .read()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_else(|| DEFAULT_PROTECTED_PROCESS_NAME.to_string())
}

fn protected_names() -> Vec<String> {
    let base = protected_process_name();
    ["", "d", "-daemon", "-cli", "-tui", "-host"]
        .iter()
        .map(|suffix| format!("{base}{suffix}"))
        .collect()
}

const KILL_COMMANDS: &[&str] = &[
    "kill",
    "pkill",
    "killall",
    "killall5",
    "skill",
    "taskkill",
    "tskill",
    "pskill",
    "stop-process",
    "spps",
    "wmic",
];

/// Upstream pipeline stages whose arguments commonly select the kill target.
const PID_PRODUCERS: &[&str] = &[
    "pgrep",
    "pidof",
    "ps",
    "tasklist",
    "get-process",
    "gps",
    "get-ciminstance",
    "get-wmiobject",
    "grep",
    "egrep",
    "fgrep",
    "awk",
    "select-string",
    "where-object",
    "findstr",
];

/// Tokens that may precede a kill command while it is still in command position.
const WRAPPERS: &[&str] = &[
    "sudo",
    "doas",
    "xargs",
    "env",
    "nohup",
    "timeout",
    "command",
    "exec",
    "time",
    "nice",
    "ionice",
    "setsid",
    "stdbuf",
    "builtin",
    "bash",
    "sh",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "powershell",
    "pwsh",
    "cmd",
    "call",
    "start",
    "then",
    "do",
    "else",
    "!",
    "start-process",
    "invoke-expression",
    "iex",
    "bypass",
    "unrestricted",
    "remotesigned",
];

/// Path of the daemon's exclusive lock/owner file (records the daemon pid).
/// Registered once by the daemon after it acquires ownership; the recorded pid
/// is protected in addition to this process, so a host/engine that is not the
/// daemon still refuses to signal it.
static DAEMON_LOCK_PATH: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// Register the daemon owner-file path whose recorded pid must stay alive.
pub fn register_daemon_lock_path(path: std::path::PathBuf) {
    if let Ok(mut slot) = DAEMON_LOCK_PATH.lock() {
        *slot = Some(path);
    }
}

fn daemon_lock_owner() -> Option<OwningProcess> {
    let path = DAEMON_LOCK_PATH.lock().ok()?.clone()?;
    let pid = std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|&pid| pid > 0)?;
    Some(OwningProcess { pid, pgid: None })
}

/// Return a deny reason when `command` tries to signal or terminate the owning
/// process, its process group, the daemon recorded in its lock file, or a
/// host process by name, or to stop/restart the daemon via the CLI or a
/// service manager.
pub fn owning_process_kill_reason(command: &str, owner: &OwningProcess) -> Option<String> {
    if let Some(reason) = owning_process_kill_reason_for(command, owner) {
        return Some(reason);
    }
    if let Some(daemon) = daemon_lock_owner() {
        if daemon.pid != owner.pid {
            if let Some(reason) = owning_process_kill_reason_for(command, &daemon) {
                return Some(reason);
            }
        }
    }
    None
}

const CLI_LIFECYCLE_VERBS: &[&str] = &[
    "stop",
    "serve",
    "restart",
    "shutdown",
    "service",
    "kill",
    "uninstall",
];

const SERVICE_MANAGERS: &[&str] = &[
    "systemctl",
    "service",
    "launchctl",
    "sc",
    "net",
    "net1",
    "stop-service",
    "restart-service",
    "remove-service",
];

const SERVICE_VERBS: &[&str] = &[
    "stop",
    "restart",
    "kill",
    "disable",
    "mask",
    "reload",
    "try-restart",
    "reload-or-restart",
    "unload",
    "bootout",
    "remove",
    "delete",
    "kickstart",
    "-k",
];

fn is_lifecycle_binary(token: &str) -> bool {
    let base = protected_process_name();
    let token = normalize_command(token);
    token == base || token == format!("{base}d") || token == format!("{base}-daemon")
}

fn owning_process_kill_reason_for(command: &str, owner: &OwningProcess) -> Option<String> {
    for pipeline in split_pipelines(command) {
        for (index, stage) in pipeline.iter().enumerate() {
            if let Some(reason) = check_lifecycle_stage(stage) {
                return Some(reason);
            }
            let Some(kill_at) = kill_command_position(stage) else {
                continue;
            };
            let head = normalize_command(&stage[kill_at]);
            let args = &stage[kill_at + 1..];
            if let Some(reason) = check_kill_args(&head, args, owner) {
                return Some(reason);
            }
            // `pgrep myapp | xargs kill`: upstream stages feed the kill.
            for upstream in &pipeline[..index] {
                let Some(first) = upstream.first() else {
                    continue;
                };
                if PID_PRODUCERS.contains(&normalize_command(first).as_str()) {
                    if let Some(reason) = check_target_tokens(&upstream[1..], owner, false) {
                        return Some(reason);
                    }
                }
            }
        }
    }
    None
}

/// `myapp stop|serve|restart|service ...` and service-manager commands
/// (`systemctl stop myapp`, `sc stop myapp`, `launchctl bootout ...`)
/// that target the daemon.
fn check_lifecycle_stage(stage: &[String]) -> Option<String> {
    if let Some(at) = command_position(stage, &is_lifecycle_binary) {
        let args = &stage[at + 1..];
        let service_status_only = args
            .iter()
            .any(|a| clean(a).to_ascii_lowercase() == "status");
        for arg in args {
            let a = clean(arg).to_ascii_lowercase();
            if CLI_LIFECYCLE_VERBS.contains(&a.as_str()) && !(a == "service" && service_status_only)
            {
                return Some(deny("host stop/serve/restart/service control"));
            }
        }
    }
    let base = protected_process_name();
    if let Some(at) = command_position(stage, &|t| {
        SERVICE_MANAGERS.contains(&normalize_command(t).as_str())
    }) {
        let head = normalize_command(&stage[at]);
        let args = &stage[at + 1..];
        let targets_daemon = args
            .iter()
            .any(|a| clean(a).to_ascii_lowercase().contains(base.as_str()));
        let implicit_stop = matches!(
            head.as_str(),
            "stop-service" | "restart-service" | "remove-service"
        );
        let has_verb = implicit_stop
            || args
                .iter()
                .any(|a| SERVICE_VERBS.contains(&clean(a).to_ascii_lowercase().as_str()));
        if targets_daemon && has_verb {
            return Some(deny("service manager command targets the daemon"));
        }
    }
    None
}

fn deny(what: &str) -> String {
    format!(
        "refusing to signal or terminate the host runtime itself ({what}); \
         owning-process protection cannot be approved or bypassed"
    )
}

fn check_kill_args(head: &str, args: &[String], owner: &OwningProcess) -> Option<String> {
    if head == "wmic"
        && !args.iter().any(|a| {
            let a = a.to_ascii_lowercase();
            a == "terminate" || a == "delete"
        })
    {
        return None;
    }
    if head == "killall5" {
        return Some(deny("killall5 signals every process"));
    }
    if head == "kill" {
        if let Some(reason) = check_kill_signal_targets(args) {
            return Some(reason);
        }
    }
    check_target_tokens(args, owner, true)
}

/// POSIX `kill` argument grammar: broadcast targets after the signal spec.
fn check_kill_signal_targets(args: &[String]) -> Option<String> {
    let mut rest: &[String] = args;
    if let Some(first) = rest.first() {
        let f = first.to_ascii_lowercase();
        if matches!(f.as_str(), "-s" | "-n" | "--signal") {
            rest = &rest[2.min(rest.len())..];
        } else if f != "--" && f.starts_with('-') && rest.len() > 1 {
            rest = &rest[1..];
        }
    }
    if rest.first().map(String::as_str) == Some("--") {
        rest = &rest[1..];
    }
    for target in rest {
        let t = clean(target);
        if t == "-1" || t == "0" {
            return Some(deny("broadcast kill target includes the daemon"));
        }
    }
    None
}

fn check_target_tokens(args: &[String], owner: &OwningProcess, allow_ppid: bool) -> Option<String> {
    for raw in args {
        for candidate in candidates(raw) {
            if allow_ppid && is_ppid_reference(&candidate) {
                return Some(deny("$PPID may be the daemon"));
            }
            if let Some(reason) = match_numeric(&candidate, owner) {
                return Some(reason);
            }
            if is_protected_name(&candidate) {
                return Some(deny("target is the host process by name"));
            }
        }
    }
    None
}

fn match_numeric(candidate: &str, owner: &OwningProcess) -> Option<String> {
    let (negative, digits) = match candidate.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, candidate),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: u64 = digits.parse().ok()?;
    if value == 0 {
        return None;
    }
    let is_pid = value == u64::from(owner.pid);
    let is_pgid = owner.pgid.is_some_and(|g| value == u64::from(g));
    if negative && (is_pid || is_pgid) {
        return Some(deny("target is the daemon's process group"));
    }
    if !negative && is_pid {
        return Some(deny("target pid is the daemon"));
    }
    if !negative && is_pgid {
        // A bare number equal to the group id is the group leader's pid.
        return Some(deny("target pid is the daemon's process group leader"));
    }
    None
}

fn is_ppid_reference(candidate: &str) -> bool {
    let c = candidate.trim_start_matches('-').to_ascii_lowercase();
    matches!(c.as_str(), "$ppid" | "${ppid}")
}

fn is_protected_name(candidate: &str) -> bool {
    let base = candidate.rsplit(['/', '\\']).next().unwrap_or(candidate);
    let lower = base.to_ascii_lowercase();
    let trimmed = lower.trim_matches(|c| c == '*' || c == '?');
    let stem = trimmed.strip_suffix(".exe").unwrap_or(trimmed);
    let stem = stem.trim_matches(|c| c == '*' || c == '?');
    !stem.is_empty() && protected_names().iter().any(|n| n == stem)
}

fn clean(token: &str) -> String {
    token
        .trim_matches(|c: char| c == '\'' || c == '"' || c == ',' || c == ';')
        .to_string()
}

/// Split one argument into comparable candidates: handles `-Id:123`,
/// `name='myapp.exe'`, and `Stop-Process -Id 1,2`.
fn candidates(token: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: &str| {
        let c = clean(s);
        if !c.is_empty() {
            out.push(c);
        }
    };
    push(token);
    for part in token.split([',', '=', ':']) {
        push(part);
    }
    out
}

fn normalize_command(token: &str) -> String {
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// Index of a kill-class command in command position, if any.
fn kill_command_position(stage: &[String]) -> Option<usize> {
    command_position(stage, &|token| {
        KILL_COMMANDS.contains(&normalize_command(token).as_str())
    })
}

/// Index of the first token matching `is_target` while every earlier token is a
/// wrapper, flag, or env assignment (i.e. still in command position).
fn command_position(stage: &[String], is_target: &dyn Fn(&str) -> bool) -> Option<usize> {
    for (index, token) in stage.iter().enumerate() {
        if is_target(token) {
            return Some(index);
        }
        let lower = token.to_ascii_lowercase();
        let passable = WRAPPERS.contains(&lower.as_str())
            || lower.starts_with('-')
            || (lower.starts_with('/') && lower.len() <= 3)
            || lower.bytes().all(|b| b.is_ascii_digit())
            || is_env_assignment(token);
        if !passable {
            return None;
        }
    }
    None
}

fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((key, _)) => {
            !key.is_empty()
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !key.chars().next().is_some_and(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: OwningProcess = OwningProcess {
        pid: 4242,
        pgid: Some(4200),
    };
    const WIN_OWNER: OwningProcess = OwningProcess {
        pid: 4242,
        pgid: None,
    };

    fn denied(command: &str) -> bool {
        owning_process_kill_reason(command, &OWNER).is_some()
    }

    #[test]
    fn denies_unix_kills_of_daemon_pid_group_and_name() {
        for command in [
            "kill 4242",
            "kill -9 4242",
            "kill -KILL 4242",
            "kill -s TERM 4242",
            "kill -9 1 4242",
            "kill -- -4200",
            "kill -TERM -4200",
            "kill -9 -4242",
            "kill 4200",
            "sudo kill -9 4242",
            "echo go; kill 4242",
            "true && kill 4242",
            "bash -c 'kill -9 4242'",
            "sh -c \"cd /tmp && kill 4242\"",
            "timeout 5 kill 4242",
            "kill -9 -1",
            "kill -- -1",
            "kill -s KILL -1",
            "kill 0",
            "kill -9 $PPID",
            "kill -9 ${PPID}",
            "pkill agenthost",
            "pkill -9 -f agenthost",
            "pkill -f /usr/local/bin/agenthost",
            "killall agenthost",
            "killall -9 agenthostd",
            "killall5 -9",
            "kill $(pgrep agenthost)",
            "kill `pidof agenthost`",
            "pgrep agenthost | xargs kill",
            "pgrep -f agenthost | xargs -r kill -9",
            "ps aux | grep agenthost | awk '{print $2}' | xargs kill",
        ] {
            assert!(denied(command), "expected deny: {command}");
        }
    }

    #[test]
    fn denies_windows_kills_of_daemon() {
        for command in [
            "taskkill /PID 4242",
            "taskkill /F /PID 4242 /T",
            "taskkill /pid 4242",
            "taskkill /IM agenthost.exe /F",
            "taskkill /F /IM \"agenthost.exe\"",
            "Stop-Process -Id 4242",
            "Stop-Process -Id:4242 -Force",
            "Stop-Process -Id 1,4242",
            "Stop-Process -Name agenthost",
            "Stop-Process -Name 'agenthost*' -Force",
            "Get-Process agenthost | Stop-Process -Force",
            "Get-Process -Id 4242 | Stop-Process",
            "kill 4242",
            "powershell -NoProfile -Command \"Stop-Process -Id 4242\"",
            "cmd /c taskkill /PID 4242",
            "wmic process where \"name='agenthost.exe'\" call terminate",
            "pskill agenthost",
        ] {
            assert!(
                owning_process_kill_reason(command, &WIN_OWNER).is_some(),
                "expected deny: {command}"
            );
        }
    }

    #[test]
    fn leaves_other_process_kills_to_normal_approval() {
        for command in [
            "kill 1234",
            "kill -9 1234",
            "kill -1 1234",
            "kill -HUP 5555",
            "kill -l",
            "pkill node",
            "pkill -f /d/Claude/agenthost/target/debug/test-server",
            "killall python3",
            "taskkill /PID 1234 /F",
            "taskkill /IM notepad.exe",
            "Stop-Process -Name notepad",
            "Stop-Process -Id 1234",
            "Get-Process node | Stop-Process",
            "pgrep node | xargs kill",
            "grep -r kill agenthost",
            "echo 4242",
            "ls agenthost",
            "cargo build -p agenthost-policy",
            "cat agenthost.pid",
            "ps aux | grep agenthost",
        ] {
            assert!(!denied(command), "expected no deny: {command}");
        }
    }

    #[test]
    fn denies_agenthost_lifecycle_commands_and_service_managers() {
        for command in [
            "agenthost stop",
            "agenthost.exe stop",
            "agenthost serve",
            "agenthost restart",
            "agenthost service stop",
            "agenthost service uninstall",
            "./agenthost stop",
            "sudo agenthost restart",
            "x && agenthost stop",
            "x && taskkill /F /IM agenthost.exe",
            "echo hi; pkill -f agenthost",
            "systemctl stop agenthost",
            "systemctl --user restart agenthost.service",
            "sudo systemctl kill agenthost",
            "launchctl bootout gui/501/com.agenthost.daemon",
            "launchctl unload ~/Library/LaunchAgents/com.agenthost.daemon.plist",
            "sc stop agenthost",
            "sc.exe delete AgentHost",
            "net stop agenthost",
            "Stop-Service agenthost",
            "Restart-Service -Name agenthost",
            "powershell -Command \"Stop-Service agenthost\"",
        ] {
            assert!(
                owning_process_kill_reason(command, &OWNER).is_some(),
                "expected deny: {command}"
            );
        }
    }

    #[test]
    fn leaves_benign_agenthost_and_service_commands_alone() {
        for command in [
            "agenthost --version",
            "agenthost status",
            "agenthost service status",
            "agenthost run \"fix the bug\"",
            "systemctl status agenthost",
            "systemctl restart nginx",
            "sc query agenthost",
            "net stop spooler",
            "Get-Service agenthost",
            "Stop-Service spooler",
            "launchctl list",
            "kill 1234",
            "taskkill /PID 1234 /F",
        ] {
            assert!(
                owning_process_kill_reason(command, &OWNER).is_none(),
                "expected no deny: {command}"
            );
        }
    }

    #[test]
    fn daemon_pid_from_lock_file_is_protected() {
        let dir = std::env::temp_dir().join(format!("cr-owner-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = dir.join("daemon.lock");
        std::fs::write(&lock, "987654").unwrap();
        register_daemon_lock_path(lock);
        assert!(owning_process_kill_reason("kill -9 987654", &OWNER).is_some());
        assert!(owning_process_kill_reason("taskkill /F /PID 987654", &OWNER).is_some());
        assert!(owning_process_kill_reason("kill -9 987655", &OWNER).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn current_process_identity_comes_from_the_os() {
        let me = OwningProcess::current();
        assert_eq!(me.pid, std::process::id());
        #[cfg(unix)]
        assert!(me.pgid.is_some());
        #[cfg(not(unix))]
        assert!(me.pgid.is_none());
        let cmd = format!("kill -9 {}", me.pid);
        assert!(owning_process_kill_reason(&cmd, &me).is_some());
    }
}
