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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rightkit-reveal-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plans_per_platform() {
        let p = Path::new("/data/a b/file.txt");
        let mac = plan(RevealPlatform::MacOs, RevealAction::Reveal, p).unwrap();
        assert_eq!(mac.program, "open");
        assert_eq!(
            mac.args,
            vec![OsString::from("-R"), p.as_os_str().to_os_string()]
        );
        let mac_open = plan(RevealPlatform::MacOs, RevealAction::Open, p).unwrap();
        assert_eq!(mac_open.args, vec![p.as_os_str().to_os_string()]);
        let linux = plan(RevealPlatform::Linux, RevealAction::Reveal, p).unwrap();
        assert_eq!(linux.program, "xdg-open");
        assert_eq!(linux.args, vec![OsString::from("/data/a b")]);
        let linux_open = plan(RevealPlatform::Linux, RevealAction::Open, p).unwrap();
        assert_eq!(linux_open.args, vec![p.as_os_str().to_os_string()]);
    }

    #[test]
    fn linux_reveal_of_the_filesystem_root_opens_the_root() {
        let linux = plan(RevealPlatform::Linux, RevealAction::Reveal, Path::new("/")).unwrap();
        assert_eq!(linux.args, vec![OsString::from("/")]);
    }

    #[test]
    fn the_windows_reveal_argument_is_quoted_verbatim() {
        let plan = plan(
            RevealPlatform::Windows,
            RevealAction::Reveal,
            Path::new("/c/My Docs/a.txt"),
        )
        .unwrap();
        assert_eq!(plan.program, "explorer.exe");
        assert!(plan.raw_windows_args);
        assert_eq!(
            plan.args,
            vec![OsString::from("/select,\"/c/My Docs/a.txt\"")]
        );
    }

    #[test]
    fn a_relative_path_gets_no_plan() {
        let error = plan(RevealPlatform::Linux, RevealAction::Open, Path::new("a/b")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn an_empty_scope_is_refused() {
        let error = PathScope::new(Vec::<PathBuf>::new()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn paths_inside_the_root_resolve_and_the_root_itself_does() {
        let root = temp("inside");
        let file = root.join("a.txt");
        fs::write(&file, b"x").unwrap();
        let scope = PathScope::new([&root]).unwrap();
        assert_eq!(
            scope.resolve(&file).unwrap(),
            plain(fs::canonicalize(&file).unwrap())
        );
        assert!(scope.resolve(&root).is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dotdot_escape_is_refused() {
        let root = temp("dotdot");
        let inner = root.join("inner");
        fs::create_dir_all(&inner).unwrap();
        let outside = temp("dotdot-outside");
        let scope = PathScope::new([&inner]).unwrap();
        let sneaky = inner
            .join("..")
            .join("..")
            .join(outside.file_name().unwrap());
        let error = scope.resolve(&sneaky).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_sibling_sharing_the_root_prefix_is_refused() {
        let base = temp("prefix");
        let root = base.join("data");
        let sibling = base.join("data2");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        let scope = PathScope::new([&root]).unwrap();
        let error = scope.resolve(&sibling).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_path_is_refused() {
        let root = temp("missing");
        let scope = PathScope::new([&root]).unwrap();
        let error = scope.resolve(&root.join("nope")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_root_is_refused() {
        let root = temp("link");
        let outside = temp("link-outside");
        fs::write(outside.join("secret.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let scope = PathScope::new([&root]).unwrap();
        let error = scope
            .resolve(&root.join("escape").join("secret.txt"))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let error = launch_command(&root.join("escape"), &scope, RevealAction::Reveal).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn launch_command_refuses_an_outside_path_before_building_anything() {
        let root = temp("cmd-root");
        let outside = temp("cmd-outside");
        let scope = PathScope::new([&root]).unwrap();
        for action in [RevealAction::Reveal, RevealAction::Open] {
            let error = launch_command(&outside, &scope, action).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn open_refuses_runnable_files_and_reveal_does_not() {
        let root = temp("runnable");
        let script = root.join("run.bat");
        fs::write(&script, b"echo x").unwrap();
        // A bundle is runnable only with a Contents/Info.plist (see `is_bundle`).
        let bundle = root.join("Thing.app");
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        fs::write(bundle.join("Contents").join("Info.plist"), b"<plist/>").unwrap();
        let scope = PathScope::new([&root]).unwrap();
        for path in [&script, &bundle] {
            let error = launch_command(path, &scope, RevealAction::Open).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(launch_command(path, &scope, RevealAction::Reveal).is_ok());
        }
        let document = root.join("notes.txt");
        fs::write(&document, b"x").unwrap();
        assert!(launch_command(&document, &scope, RevealAction::Open).is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_named_like_a_script_opens_but_a_bundle_refuses() {
        let root = temp("folder-named");
        let folder = root.join("chart.js");
        fs::create_dir_all(&folder).unwrap();
        let fake_app = root.join("Foo.app");
        fs::create_dir_all(&fake_app).unwrap();
        let real_app = root.join("Real.app");
        fs::create_dir_all(real_app.join("Contents")).unwrap();
        fs::write(real_app.join("Contents").join("Info.plist"), b"<plist/>").unwrap();
        let scope = PathScope::new([&root]).unwrap();
        assert!(!is_runnable(&folder.canonicalize().unwrap()));
        assert!(!is_runnable(&fake_app.canonicalize().unwrap()));
        assert!(is_runnable(&real_app.canonicalize().unwrap()));
        assert!(launch_command(&folder, &scope, RevealAction::Open).is_ok());
        let error = launch_command(&real_app, &scope, RevealAction::Open).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn open_installer_is_refused_off_windows_and_outside_the_scope() {
        let root = temp("installer");
        let setup = root.join("setup.exe");
        fs::write(&setup, b"MZ").unwrap();
        let scope = PathScope::new([&root]).unwrap();
        // Off Windows the call is refused before anything starts. On Windows it would launch the
        // file, so that case is left to a Windows run.
        if !cfg!(windows) {
            let error = open_installer(&setup, &scope).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn open_refuses_a_file_with_an_execute_bit() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("execbit");
        let file = root.join("tool");
        fs::write(&file, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let scope = PathScope::new([&root]).unwrap();
        let error = launch_command(&file, &scope, RevealAction::Open).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let _ = fs::remove_dir_all(&root);
    }
}
