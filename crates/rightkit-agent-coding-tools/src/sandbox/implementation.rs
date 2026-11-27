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

/// The default name of the reserved launch-environment key carrying extra read/execute roots for
/// one confined launch. The Windows AppContainer backend consumes (and removes) it
/// in `prepare_launch`, leasing each root read-only to the per-command
/// container SID for the lifetime of that command. Entries are separated by
/// [`SANDBOX_READ_ROOTS_SEPARATOR`] (`|` cannot appear in a Windows path).
/// Other backends ignore it. The key in force is [`sandbox_read_roots_env_name`], which follows
/// the host's environment prefix; this constant is that name under the default prefix.
pub const SANDBOX_READ_ROOTS_ENV: &str = "RIGHTKIT_SANDBOX_READ_ROOTS";

/// The launch-environment key that carries read/execute roots for this host:
/// `<prefix>_SANDBOX_READ_ROOTS` with the prefix of `rightkit_agent_config::set_env_prefix`
/// (`RIGHTKIT_SANDBOX_READ_ROOTS` by default, [`SANDBOX_READ_ROOTS_ENV`]).
pub fn sandbox_read_roots_env_name() -> String {
    rightkit_agent_config::env_name("SANDBOX_READ_ROOTS")
}

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
        vec![(sandbox_read_roots_env_name(), joined)]
    }
}

/// Read the requested read/execute roots out of a launch environment.
///
/// The key can arrive through any launch env (e.g. a configured MCP server's
/// `env`), so a root is honoured only when it is an interpreter install
/// directory, the one purpose this channel exists for. Anything else is
/// dropped, so the key cannot open arbitrary directories to a confined child.
pub fn parse_sandbox_read_roots(env: &[(String, String)]) -> Vec<std::path::PathBuf> {
    let name = sandbox_read_roots_env_name();
    env.iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(&name))
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

/// The process-wide RightKit sandbox that owns the pinned read roots. Unlimited because a host
/// that admits every launch through `process_resource_authority` before it reaches the sandbox
/// needs no second cap, and the Practical profile would cap confined children at 4.
fn process_sandbox() -> &'static rightkit_sandbox::Sandbox {
    static SANDBOX: std::sync::OnceLock<rightkit_sandbox::Sandbox> = std::sync::OnceLock::new();
    SANDBOX.get_or_init(|| {
        rightkit_sandbox::Sandbox::new(rightkit_sandbox::ResourceAuthority::unlimited())
    })
}

/// Pin interpreter install directories as read/execute roots for the children confined through
/// `rightkit-sandbox` in this process. RightKit refuses a root that is too broad or not an
/// interpreter directory, and shares one pin per root. The caller holds the returned handle for as
/// long as the roots must stay readable; dropping the last handle on a root removes its pin.
///
/// On Windows these pins reach the `rightkit-sandbox` AppContainer backend only, not this crate's
/// own AppContainer launches, which take their extra roots from [`sandbox_read_roots_env`].
pub fn pin_interpreter_read_roots(
    roots: &[std::path::PathBuf],
) -> std::io::Result<rightkit_sandbox::PinHandle> {
    process_sandbox()
        .pin_read_roots(roots)
        .map_err(|error| std::io::Error::other(error.to_string()))
}
