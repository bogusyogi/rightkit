//! Vendor CLI discovery: find a launchable executable on `PATH` the way the platform starts
//! it, and probe its version and sign-in with bounded, non-interactive runs owned by `rightkit-child-process`.
//!
//! [`detect`] caches one snapshot per vendor for the life of the process. Setting
//! [`DISABLE_VENDOR_CLIS_ENV`] reports every vendor as not installed, for isolated hosts that
//! must never run a real CLI.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::OnceLock;
use std::time::Duration;

/// Environment variable that disables every vendor CLI probe when set to anything but an empty
/// value or `0`. The name is chosen for this port (CodeRight's host uses
/// `CODERIGHT_DISABLE_VENDOR_CLIS`); the owner should confirm it.
pub const DISABLE_VENDOR_CLIS_ENV: &str = "RIGHTKIT_DISABLE_VENDOR_CLIS";

/// Timeout [`detect`] gives each probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The vendor CLIs this crate can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorCli {
    Claude,
    Codex,
}

impl VendorCli {
    /// The executable name searched for on `PATH`.
    pub fn binary_name(self) -> &'static str {
        match self {
            VendorCli::Claude => "claude",
            VendorCli::Codex => "codex",
        }
    }
}

/// What a probe learned. `installed` is true only when a candidate ran `--version`
/// successfully; dead launchers are skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VendorCliStatus {
    pub installed: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    /// Sign-in as the CLI itself reports it. `None` when unknown (no probe, a timeout, or a
    /// reply that is not the official answer).
    pub signed_in: Option<bool>,
}

/// Every launchable candidate for `name` on `PATH`, in search order. On Windows the
/// `PATHEXT` extensions come first, in their order, and the bare name (npm's extensionless
/// shim) comes last.
pub fn find_binaries(name: &str) -> Vec<PathBuf> {
    let Some(paths) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let suffixes = executable_suffixes();
    std::env::split_paths(&paths)
        .flat_map(|dir| {
            suffixes
                .iter()
                .map(move |suffix| dir.join(format!("{name}{suffix}")))
                .collect::<Vec<PathBuf>>()
        })
        .filter(|candidate| is_launchable(candidate))
        .collect()
}

/// The first launchable candidate for `name` on `PATH`. See [`find_binaries`].
pub fn find_binary(name: &str) -> Option<PathBuf> {
    find_binaries(name).into_iter().next()
}

fn executable_suffixes() -> Vec<String> {
    if cfg!(windows) {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        let mut suffixes: Vec<String> = extensions
            .split(';')
            .map(str::trim)
            .filter(|extension| !extension.is_empty())
            .map(str::to_string)
            .collect();
        suffixes.push(String::new());
        suffixes
    } else {
        vec![String::new()]
    }
}

fn is_launchable(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && executable_bit(&metadata),
        Err(_) => false,
    }
}

#[cfg(unix)]
fn executable_bit(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_bit(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// Most bytes kept from each of stdout and stderr by [`run_bounded`]. Output beyond the cap
/// is drained and discarded so a chatty probe never blocks on a full pipe.
pub const PROBE_OUTPUT_CAP: usize = 64 * 1024;

/// Run `program` with `args`, stdin closed and output captured. The child is killed when it
/// outlives `timeout`. `None` when it cannot start, times out, or cannot be waited on.
///
/// Stdout and stderr drain concurrently, each capped at [`PROBE_OUTPUT_CAP`]. The
/// deadline terminates the whole process tree through `rightkit_child_process::run`. Native
/// output readers are cancelled & joined after a short drain window, including when a
/// descendant retains a pipe. This guarantee applies with every feature configuration.
pub fn run_bounded(program: &Path, args: &[&str], timeout: Duration) -> Option<Output> {
    let options = rightkit_child_process::RunOptions::new(timeout)
        .stdout_cap(PROBE_OUTPUT_CAP)
        .stderr_cap(PROBE_OUTPUT_CAP);
    let run = rightkit_child_process::run(program, args, &options).ok()?;
    if run.timed_out {
        return None;
    }
    Some(Output {
        status: run.status,
        stdout: run.stdout.bytes,
        stderr: run.stderr.bytes,
    })
}

/// The first line of a `--version` reply, trimmed, if it is not empty.
fn version_line(stdout: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}

/// `claude auth status` prints `{"loggedIn": bool, ...}` without prompting. Only that boolean
/// is authoritative; logged-out status may exit nonzero, so the exit code is not read.
pub fn claude_auth_status(stdout: &[u8]) -> Option<bool> {
    let body: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    body.get("loggedIn")?.as_bool()
}

/// Probe one vendor now, bounding each run by `timeout`. Disabled hosts and missing CLIs
/// report `installed: false`.
pub fn probe(kind: VendorCli, timeout: Duration) -> VendorCliStatus {
    if vendor_clis_disabled() {
        return VendorCliStatus::default();
    }
    let working = find_binaries(kind.binary_name())
        .into_iter()
        .find_map(|path| {
            let output = run_bounded(&path, &["--version"], timeout)
                .filter(|output| output.status.success())?;
            Some((path, version_line(&output.stdout)))
        });
    let Some((path, version)) = working else {
        return VendorCliStatus::default();
    };
    let signed_in = match kind {
        VendorCli::Claude => run_bounded(&path, &["auth", "status"], timeout)
            .and_then(|output| claude_auth_status(&output.stdout)),
        VendorCli::Codex => None,
    };
    VendorCliStatus {
        installed: true,
        path: Some(path),
        version,
        signed_in,
    }
}

/// The cached probe for `kind`, computed once per process with [`PROBE_TIMEOUT`].
pub fn detect(kind: VendorCli) -> VendorCliStatus {
    static CLAUDE: OnceLock<VendorCliStatus> = OnceLock::new();
    static CODEX: OnceLock<VendorCliStatus> = OnceLock::new();
    let cell = match kind {
        VendorCli::Claude => &CLAUDE,
        VendorCli::Codex => &CODEX,
    };
    cell.get_or_init(|| probe(kind, PROBE_TIMEOUT)).clone()
}

/// `(id, display)` model rows for a vendor that is installed. Claude Code has no
/// non-interactive model-list command, so its rows are the aliases `--model` accepts. Codex
/// lists none here.
pub fn list_models(kind: VendorCli) -> Vec<(String, String)> {
    if !detect(kind).installed {
        return Vec::new();
    }
    match kind {
        VendorCli::Claude => ["opus", "sonnet", "haiku"]
            .iter()
            .map(|alias| {
                let mut display = alias.to_string();
                display[..1].make_ascii_uppercase();
                (alias.to_string(), display)
            })
            .collect(),
        VendorCli::Codex => Vec::new(),
    }
}

fn vendor_clis_disabled() -> bool {
    std::env::var_os(DISABLE_VENDOR_CLIS_ENV).is_some_and(|value| !value.is_empty() && value != "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_status_reads_only_the_official_boolean() {
        assert_eq!(claude_auth_status(br#"{"loggedIn":true}"#), Some(true));
        assert_eq!(claude_auth_status(br#"{"loggedIn":false}"#), Some(false));
        assert_eq!(claude_auth_status(b"failed to start"), None);
        assert_eq!(claude_auth_status(br#"{"error":"unavailable"}"#), None);
    }

    #[test]
    fn version_line_takes_the_first_nonempty_trimmed_line() {
        let reply = b"  1.2.3 (Claude Code)\nmore\n";
        assert_eq!(version_line(reply).as_deref(), Some("1.2.3 (Claude Code)"));
        assert_eq!(version_line(b"\n"), None);
    }

    #[test]
    fn bare_name_is_the_last_suffix() {
        let suffixes = executable_suffixes();
        assert_eq!(suffixes.last().map(String::as_str), Some(""));
    }

    #[test]
    fn binary_names_are_stable() {
        assert_eq!(VendorCli::Claude.binary_name(), "claude");
        assert_eq!(VendorCli::Codex.binary_name(), "codex");
    }
}

#[cfg(all(test, unix))]
mod bounded_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn run_bounded_caps_chatty_output_and_keeps_both_streams() {
        let output = run_bounded(
            Path::new("/bin/sh"),
            &["-c", "head -c 300000 /dev/zero; printf err >&2"],
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), PROBE_OUTPUT_CAP);
        assert_eq!(output.stderr, b"err".to_vec());
    }

    #[cfg(unix)]
    #[test]
    fn run_bounded_gives_up_at_the_deadline() {
        let started = std::time::Instant::now();
        let output = run_bounded(
            Path::new("/bin/sh"),
            &["-c", "sleep 30"],
            Duration::from_millis(300),
        );
        assert!(output.is_none());
        assert!(started.elapsed() < Duration::from_secs(15));
    }
}
