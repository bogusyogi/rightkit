// RIGHTKIT-SERIOUS: the `RIGHTKIT_SANDBOX_READ_ROOTS` env channel feeds the local AppContainer backend's `PinnedTable` (report T-3). The breadth and interpreter checks (`read_root_is_too_broad`) are RightKit's.
// `shell_job()` is a thin adapter over `rightkit_child_process::WindowsJob`.
include!("config.rs");
#[cfg(windows)]
pub use rightkit_child_process::WindowsJob;

/// Kill-on-close job with the host app's shell-child active-process cap.
#[cfg(windows)]
pub fn shell_job() -> std::io::Result<WindowsJob> {
    WindowsJob::new(Some(rightkit_child_process::DEFAULT_ACTIVE_PROCESS_LIMIT))
}
include!("windows_appcontainer.rs");

/// Reserved launch-environment key carrying extra read/execute roots for one
/// confined launch. The Windows AppContainer backend consumes (and removes) it
/// in `prepare_launch`, leasing each root read-only to the per-command
/// container SID for the lifetime of that command. Entries are separated by
/// [`SANDBOX_READ_ROOTS_SEPARATOR`] (`|` cannot appear in a Windows path).
/// Other backends ignore it.
pub const SANDBOX_READ_ROOTS_ENV: &str = "RIGHTKIT_SANDBOX_READ_ROOTS";

/// Separator between roots in [`SANDBOX_READ_ROOTS_ENV`].
pub const SANDBOX_READ_ROOTS_SEPARATOR: char = '|';

/// Build the launch-env entries that request read/execute access to `roots`.
/// Empty input yields no entry, so a launch without roots is unchanged.
pub fn sandbox_read_roots_env(roots: &[std::path::PathBuf]) -> Vec<(String, String)> {
    let joined = roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .filter(|root| !root.is_empty())
        .collect::<Vec<_>>()
        .join(&SANDBOX_READ_ROOTS_SEPARATOR.to_string());
    if joined.is_empty() {
        Vec::new()
    } else {
        vec![(SANDBOX_READ_ROOTS_ENV.to_string(), joined)]
    }
}

/// Read the requested read/execute roots out of a launch environment.
///
/// The key can arrive through any launch env (e.g. a configured MCP server's
/// `env`), so a root is honoured only when it is an interpreter install
/// directory, the one purpose this channel exists for. Anything else is
/// dropped, so the key cannot open arbitrary directories to a confined child.
pub fn parse_sandbox_read_roots(env: &[(String, String)]) -> Vec<std::path::PathBuf> {
    env.iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(SANDBOX_READ_ROOTS_ENV))
        .flat_map(|(_, value)| value.split(SANDBOX_READ_ROOTS_SEPARATOR))
        .filter(|root| !root.is_empty())
        .map(std::path::PathBuf::from)
        .filter(|root| is_interpreter_install_dir(root))
        .collect()
}

/// True when `dir` directly holds a Python interpreter executable. Deliberately narrower
/// than `rightkit_sandbox::is_interpreter_dir` (17 interpreters, also `bin`/`Scripts`):
/// this channel only exists to lease Python install roots.
pub fn is_interpreter_install_dir(dir: &std::path::Path) -> bool {
    const INTERPRETERS: &[&str] = &["python.exe", "python3.exe", "python", "python3"];
    INTERPRETERS.iter().any(|name| dir.join(name).is_file())
}

/// True when `root` is, or contains, a per-user directory, the user profile or a protected
/// secret folder (RightKit's check; stricter than the previous local one).
pub use rightkit_sandbox::read_root_is_too_broad;
