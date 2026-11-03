//! Reveal a file or folder in the system file manager, or open it with its default handler,
//! through [`OwnedCommand`], and only inside caller-supplied roots.
//!
//! The helper starts `open -R` (macOS), `explorer.exe /select,` (Windows) or `xdg-open` on the
//! parent folder (other Unix). Every path is first resolved with `std::fs::canonicalize`
//! (symlinks and `..` followed) and must lie inside one of the roots of a [`PathScope`]; a
//! path outside every root, a path that does not exist and an empty scope are all refused
//! before anything is started. Opening a file the system would run (an executable, a script,
//! an application bundle, a shortcut) is refused as well: [`RevealAction::Open`] is for
//! documents and folders.
//!
//! The helper is detached: the file manager outlives the call and is never killed by it.

use crate::{OwnedChild, OwnedCommand, DEFAULT_ENV_ALLOWLIST};
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

/// Extra variables a Linux desktop launcher needs on top of [`DEFAULT_ENV_ALLOWLIST`].
const LAUNCHER_ENV: &[&str] = &[
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "XDG_CONFIG_HOME",
    "XDG_CONFIG_DIRS",
    "XDG_DATA_DIRS",
];

/// How long [`reveal`] and [`open`] wait for the launcher to report before leaving it running.
pub const LAUNCH_WAIT: Duration = Duration::from_secs(5);

/// File extensions (lower case, no dot) that mean "this runs code" on at least one platform.
const RUNNABLE_EXTENSIONS: &[&str] = &[
    "app", "command", "tool", "workflow", "action", "pkg", "dmg", "exe", "com", "bat", "cmd",
    "msi", "msp", "scr", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "hta", "reg",
    "lnk", "url", "scf", "cpl", "dll", "jar", "desktop", "sh", "run", "appimage",
];

/// What to do with a path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevealAction {
    /// Show the item selected in its folder (Linux: open the parent folder).
    Reveal,
    /// Open the item with its default handler. Refused for runnable files.
    Open,
}

/// The host a [`LaunchPlan`] is built for. Present so every platform's command line can be
/// checked on any host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevealPlatform {
    MacOs,
    Windows,
    Linux,
}

impl RevealPlatform {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// The program and arguments to start. `raw_windows_args` means the single argument is already
/// quoted the way `explorer.exe` wants it and must reach the command line verbatim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchPlan {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub raw_windows_args: bool,
}

/// The roots a path must lie inside. Roots are canonicalised when the scope is built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathScope {
    roots: Vec<PathBuf>,
}

impl PathScope {
    /// Fails when a root cannot be canonicalised or when no root is given: a scope that
    /// allows nothing is a mistake, not a policy.
    pub fn new<I, P>(roots: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut resolved = Vec::new();
        for root in roots {
            resolved.push(plain(std::fs::canonicalize(root.as_ref())?));
        }
        if resolved.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a path scope needs at least one allowed root",
            ));
        }
        Ok(Self { roots: resolved })
    }

    /// The canonical form of `path` when it exists and lies inside a root (a root itself
    /// counts); `PermissionDenied` when it resolves outside every root, `NotFound` (or the
    /// OS error) when it cannot be resolved.
    pub fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        let canonical = plain(std::fs::canonicalize(path)?);
        // `starts_with` compares whole components: `/data2` is not inside `/data`.
        if self.roots.iter().any(|root| canonical.starts_with(root)) {
            Ok(canonical)
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is outside the allowed roots", canonical.display()),
            ))
        }
    }
}

/// Drops the `\\?\` prefix `canonicalize` adds on Windows (Explorer does not take it). A UNC
/// path keeps its prefix.
fn plain(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(text) = path.to_str() {
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            if !rest.starts_with("UNC\\") {
                return PathBuf::from(rest);
            }
        }
    }
    path
}

/// The command line for `action` on `canonical` (an absolute, canonical path) on `platform`.
/// Pure: starts nothing and reads nothing from the file system.
pub fn plan(
    platform: RevealPlatform,
    action: RevealAction,
    canonical: &Path,
) -> io::Result<LaunchPlan> {
    if !canonical.has_root() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a launch plan needs a rooted canonical path",
        ));
    }
    let path = canonical.as_os_str().to_os_string();
    let simple = |program: &str, args: Vec<OsString>| LaunchPlan {
        program: program.into(),
        args,
        raw_windows_args: false,
    };
    Ok(match (platform, action) {
        (RevealPlatform::MacOs, RevealAction::Reveal) => simple("open", vec!["-R".into(), path]),
        (RevealPlatform::MacOs, RevealAction::Open) => simple("open", vec![path]),
        (RevealPlatform::Windows, RevealAction::Reveal) => {
            let text = canonical.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "path is not valid Unicode")
            })?;
            if text.contains('"') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path contains a quote",
                ));
            }
            LaunchPlan {
                program: "explorer.exe".into(),
                args: vec![format!("/select,\"{text}\"").into()],
                raw_windows_args: true,
            }
        }
        (RevealPlatform::Windows, RevealAction::Open) => simple("explorer.exe", vec![path]),
        (RevealPlatform::Linux, RevealAction::Reveal) => {
            let folder = canonical.parent().unwrap_or(canonical);
            simple("xdg-open", vec![folder.as_os_str().to_os_string()])
        }
        (RevealPlatform::Linux, RevealAction::Open) => simple("xdg-open", vec![path]),
    })
}

/// Whether the system would run `canonical` rather than show it: a runnable extension, an
/// execute permission bit on a Unix file, or an application bundle directory.
fn is_runnable(canonical: &Path) -> bool {
    if canonical.is_dir() {
        // A folder is runnable only when it is a real application or workflow bundle. A folder
        // named `chart.js`, `highlight.js` or `Foo.app` without a bundle is opened as a folder.
        return is_bundle(canonical);
    }
    let extension_runs = canonical
        .extension()
        .and_then(OsStr::to_str)
        .map(|ext| RUNNABLE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false);
    if extension_runs {
        return true;
    }
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(canonical) {
        use std::os::unix::fs::PermissionsExt;
        if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
            return true;
        }
    }
    false
}

/// A macOS application or workflow bundle: a directory with the bundle extension and a
/// `Contents/Info.plist`.
fn is_bundle(dir: &Path) -> bool {
    let bundle_extension = dir
        .extension()
        .and_then(OsStr::to_str)
        .map(|ext| ext.to_ascii_lowercase());
    matches!(bundle_extension.as_deref(), Some("app") | Some("workflow"))
        && dir.join("Contents").join("Info.plist").is_file()
}

/// Checks `path` against `scope` (and, for [`RevealAction::Open`], against runnable files),
/// then returns the command, not yet started. Stdio is null and the environment is
/// [`DEFAULT_ENV_ALLOWLIST`] plus the desktop-session variables a launcher needs.
pub fn launch_command(
    path: &Path,
    scope: &PathScope,
    action: RevealAction,
) -> io::Result<OwnedCommand> {
    launch_checked(path, scope, action, false)
}

fn launch_checked(
    path: &Path,
    scope: &PathScope,
    action: RevealAction,
    allow_runnable: bool,
) -> io::Result<OwnedCommand> {
    let canonical = scope.resolve(path)?;
    if action == RevealAction::Open && !allow_runnable && is_runnable(&canonical) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} would be run, not opened", canonical.display()),
        ));
    }
    let plan = plan(RevealPlatform::current(), action, &canonical)?;
    let mut command = OwnedCommand::new(&plan.program);
    {
        let inner = command.command_mut();
        #[cfg(windows)]
        if plan.raw_windows_args {
            use std::os::windows::process::CommandExt;
            for arg in &plan.args {
                inner.raw_arg(arg);
            }
        } else {
            inner.args(&plan.args);
        }
        #[cfg(not(windows))]
        inner.args(&plan.args);
        inner
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    command.env_allowlist(DEFAULT_ENV_ALLOWLIST.iter().chain(LAUNCHER_ENV.iter()));
    Ok(command)
}

/// Starts the launcher detached (the file manager must outlive this call) and returns it.
pub fn launch(path: &Path, scope: &PathScope, action: RevealAction) -> io::Result<OwnedChild> {
    launch_command(path, scope, action)?.spawn_uncontained_detached()
}

fn run(path: &Path, scope: &PathScope, action: RevealAction) -> io::Result<()> {
    run_checked(path, scope, action, false)
}

fn run_checked(
    path: &Path,
    scope: &PathScope,
    action: RevealAction,
    allow_runnable: bool,
) -> io::Result<()> {
    let mut child =
        launch_checked(path, scope, action, allow_runnable)?.spawn_uncontained_detached()?;
    match child.wait_timeout(LAUNCH_WAIT)? {
        // `explorer.exe` reports failure with exit code 1 even when it succeeded.
        Some(status) if !status.success() && !cfg!(windows) => Err(io::Error::other(format!(
            "the launcher exited with {status}"
        ))),
        // Success, or still running: a launcher that stays up is handing off to the
        // desktop, and the detached child is reaped in the background.
        _ => Ok(()),
    }
}

/// Reveals `path` in the system file manager. See the module documentation.
pub fn reveal(path: &Path, scope: &PathScope) -> io::Result<()> {
    run(path, scope, RevealAction::Reveal)
}

/// Opens `path` with its default handler. See the module documentation.
pub fn open(path: &Path, scope: &PathScope) -> io::Result<()> {
    run(path, scope, RevealAction::Open)
}

/// Opens the folder `path` itself (on Windows, `explorer.exe <folder>`, not the parent with the
/// folder selected, which is what [`reveal`] does). `path` must be a directory inside the scope.
/// Folders are never refused for their name, so a folder named `chart.js` opens; a real
/// application bundle still refuses (it would run).
pub fn open_folder(path: &Path, scope: &PathScope) -> io::Result<()> {
    let canonical = scope.resolve(path)?;
    if !canonical.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a folder", canonical.display()),
        ));
    }
    run(path, scope, RevealAction::Open)
}

/// Runs a setup program (`.exe`, `.msi` or `.msp`) on Windows through the shell (`explorer.exe`
/// on the file), not as a plain child process, so the shell's Mark-of-the-Web and SmartScreen
/// checks still apply. [`open`] refuses these files because they run code; this is the one
/// path that runs them, and only for those three extensions inside the scope. Other platforms
/// return `Unsupported`. Whether SmartScreen prompts is decided by Windows, not by this crate.
pub fn open_installer(path: &Path, scope: &PathScope) -> io::Result<()> {
    if !cfg!(windows) {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    let canonical = scope.resolve(path)?;
    let extension = canonical
        .extension()
        .and_then(OsStr::to_str)
        .map(|ext| ext.to_ascii_lowercase());
    if !canonical.is_file() || !matches!(extension.as_deref(), Some("exe" | "msi" | "msp")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is not a setup program (exe, msi, msp)",
                canonical.display()
            ),
        ));
    }
    run_checked(path, scope, RevealAction::Open, true)
}
