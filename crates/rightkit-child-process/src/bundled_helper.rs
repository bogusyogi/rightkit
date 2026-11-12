//! Locating and verifying a helper program that ships inside the running app's `.app` bundle
//! (macOS): placement first, then the code-signature check against a Developer ID team.
//!
//! * [`resolve_bundled`] finds `Contents/{MacOS,Helpers,Library/LoginItems}/<name>`.
//! * [`resolve_bundled_login_item`] finds the nested login-item app
//!   `Contents/Library/LoginItems/<name>.app/Contents/MacOS/<name>`.
//! * [`verify_team_signature`] runs `/usr/bin/codesign --verify --strict` against the team and
//!   fails closed.
//!
//! Every resolver canonicalises its result and requires it to lie under the canonical bundle
//! (or, for the login item, its own nested app), so a symlink out of the bundle is refused. The
//! path logic is plain filesystem code and runs on every OS; only the signature check needs
//! `/usr/bin/codesign`, which a host without it reports as untrusted.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Bundle subdirectories (under `Contents/`) searched by [`resolve_bundled`], in order.
const BUNDLE_HELPER_DIRS: [&str; 3] = ["MacOS", "Helpers", "Library/LoginItems"];

/// The nested login-item location, relative to the bundle root: the helper `<name>` is the
/// executable of its own `<name>.app` (the unit that is signed and carries the provisioning
/// profile).
const LOGIN_ITEMS_DIR: &str = "Contents/Library/LoginItems";

const CODESIGN: &str = "/usr/bin/codesign";
const CODESIGN_TIMEOUT: Duration = Duration::from_secs(20);

/// Why a bundled helper could not be resolved or verified. The messages are the user-facing
/// text; callers that have their own error type keep them as they are.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BundledHelperError {
    /// The helper name is not a single path component. Holds the reason.
    InvalidName(String),
    /// The helper is not where the bundle layout puts it, or resolves outside it. Holds the
    /// whole message.
    NotFound(String),
    /// The helper failed the code-signature check (or the check could not run). Holds the reason
    /// that follows `untrusted helper: `.
    Untrusted(String),
}

impl fmt::Display for BundledHelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(reason) => write!(f, "invalid login item helper name: {reason}"),
            Self::NotFound(message) => f.write_str(message),
            Self::Untrusted(reason) => write!(f, "untrusted helper: {reason}"),
        }
    }
}

impl std::error::Error for BundledHelperError {}

/// Options of [`resolve_bundled_login_item_with`].
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct LoginItemOptions {
    /// Development builds: when the running executable is not inside an `.app` bundle, or the
    /// bundle has no nested login-item app, accept a bare file `<name>` beside the running
    /// executable (it must still canonicalise to a regular file directly in that directory). Off
    /// by default; a packaged build must never run an unbundled helper, because a sibling file
    /// carries no provisioning profile and no entitlement.
    pub allow_unbundled_sibling: bool,
}

impl LoginItemOptions {
    /// Set [`allow_unbundled_sibling`](Self::allow_unbundled_sibling).
    pub fn allow_unbundled_sibling(mut self, allow: bool) -> Self {
        self.allow_unbundled_sibling = allow;
        self
    }
}

/// Resolve the helper `name` inside the running app's bundle, or `None`. `name` is a single file
/// name (no separator, no `.`/`..`), looked up as
/// `Contents/{MacOS,Helpers,Library/LoginItems}/<name>` of the `.app` that contains the running
/// executable. The result is canonical, a regular file (not a symlink out of the bundle) and
/// verified to lie under the canonical bundle root; any doubt is `None`. This checks placement
/// only; pass the result to [`verify_team_signature`] to check who signed it.
pub fn resolve_bundled(name: &str) -> Option<PathBuf> {
    resolve_in_bundle(&std::env::current_exe().ok()?, name)
}

/// The path logic of [`resolve_bundled`], independent of the running executable (`exe` is any
/// file inside the `.app`).
pub fn resolve_in_bundle(exe: &Path, name: &str) -> Option<PathBuf> {
    let mut parts = Path::new(name).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(n)), None) if n == OsStr::new(name) => {}
        _ => return None,
    }
    let bundle = bundle_root(exe).ok()?;
    BUNDLE_HELPER_DIRS.iter().find_map(|dir| {
        let found = bundle
            .join("Contents")
            .join(dir)
            .join(name)
            .canonicalize()
            .ok()?;
        (found.starts_with(&bundle) && found.is_file()).then_some(found)
    })
}

/// Resolve the nested login-item helper `name` of the running app:
/// `<main bundle>/Contents/Library/LoginItems/<name>.app/Contents/MacOS/<name>`.
///
/// `name` must be a single path component (not empty, not `.` or `..`, no separator). The result
/// is canonical, a regular file, and must lie inside the nested `<name>.app` that itself lies
/// inside the canonical main bundle, so a symlink that leaves the helper app or the bundle is
/// refused (and so is one that points at another file of the same bundle). Any doubt is an
/// error ([`BundledHelperError::InvalidName`] for a bad `name`).
///
/// This checks placement only; pass the result to [`verify_team_signature`] as well.
/// [`resolve_bundled`] cannot find this layout.
pub fn resolve_bundled_login_item(name: &str) -> Result<PathBuf, BundledHelperError> {
    resolve_bundled_login_item_with(name, LoginItemOptions::default())
}

/// [`resolve_bundled_login_item`] with [`LoginItemOptions`].
pub fn resolve_bundled_login_item_with(
    name: &str,
    options: LoginItemOptions,
) -> Result<PathBuf, BundledHelperError> {
    let exe = std::env::current_exe().map_err(|e| {
        BundledHelperError::NotFound(format!(
            "cannot locate the running executable: {}",
            e.kind()
        ))
    })?;
    login_item_in_bundle_with(&exe, name, options)
}

/// The path logic of [`resolve_bundled_login_item`], independent of the running executable
/// (`exe` is any file inside the main `.app`).
pub fn login_item_in_bundle(exe: &Path, name: &str) -> Result<PathBuf, BundledHelperError> {
    login_item_in_bundle_with(exe, name, LoginItemOptions::default())
}

/// The path logic of [`resolve_bundled_login_item_with`], independent of the running executable
/// (`exe` is any file inside the main `.app`, or the development executable when the sibling
/// option is on).
pub fn login_item_in_bundle_with(
    exe: &Path,
    name: &str,
    options: LoginItemOptions,
) -> Result<PathBuf, BundledHelperError> {
    let bad_name = |reason: &str| BundledHelperError::InvalidName(reason.to_string());
    if name.is_empty() {
        return Err(bad_name("empty"));
    }
    let mut parts = Path::new(name).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(n)), None) if n == OsStr::new(name) => {}
        _ => return Err(bad_name("must be a single path component")),
    }
    // Refuse both separator spellings on every OS, so a name that is one component here cannot
    // be several on another.
    if name.contains(['/', '\\']) {
        return Err(bad_name("must be a single path component"));
    }
    match nested_login_item(exe, name) {
        Err(BundledHelperError::NotFound(_)) if options.allow_unbundled_sibling => {
            sibling_helper(exe, name)
        }
        other => other,
    }
}

fn missing(what: &str) -> BundledHelperError {
    BundledHelperError::NotFound(format!("login item helper {what}"))
}

/// The canonical root of the `.app` that contains `exe`.
fn bundle_root(exe: &Path) -> Result<PathBuf, BundledHelperError> {
    let exe = exe
        .canonicalize()
        .map_err(|_| missing("not found: the running executable cannot be resolved"))?;
    let bundle = exe
        .ancestors()
        .find(|a| {
            a.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"))
                && a.join("Contents").is_dir()
        })
        .ok_or_else(|| missing("not found: the running executable is not inside an .app bundle"))?;
    bundle
        .canonicalize()
        .map_err(|_| missing("not found: the app bundle cannot be resolved"))
}

fn nested_login_item(exe: &Path, name: &str) -> Result<PathBuf, BundledHelperError> {
    let bundle = bundle_root(exe)?;
    let app = bundle.join(LOGIN_ITEMS_DIR).join(format!("{name}.app"));
    let app = app
        .canonicalize()
        .map_err(|_| missing("not found in Contents/Library/LoginItems"))?;
    if !app.starts_with(&bundle) {
        return Err(missing("resolves outside the app bundle"));
    }
    let binary = app
        .join("Contents")
        .join("MacOS")
        .join(name)
        .canonicalize()
        .map_err(|_| missing("executable not found in its .app"))?;
    if !binary.starts_with(&app) {
        return Err(missing("executable resolves outside its .app"));
    }
    if !binary.is_file() {
        return Err(missing("executable is not a regular file"));
    }
    Ok(binary)
}

/// The development fallback: a regular file `name` directly beside `exe`.
fn sibling_helper(exe: &Path, name: &str) -> Result<PathBuf, BundledHelperError> {
    let dir = exe
        .canonicalize()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .ok_or_else(|| missing("not found: the running executable cannot be resolved"))?;
    let binary = dir
        .join(name)
        .canonicalize()
        .map_err(|_| missing("not found beside the running executable"))?;
    if binary.parent() != Some(dir.as_path()) {
        return Err(missing(
            "sibling resolves outside the executable's directory",
        ));
    }
    if !binary.is_file() {
        return Err(missing("sibling is not a regular file"));
    }
    Ok(binary)
}

/// Whether `team_id` is an Apple team identifier: exactly 10 ASCII uppercase letters or digits.
pub fn is_team_id(team_id: &str) -> bool {
    team_id.len() == 10
        && team_id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// The code requirement for a Developer ID team:
/// `anchor apple generic and certificate leaf[subject.OU] = "<TEAM>"`. `None` unless
/// [`is_team_id`], so the value can never alter the requirement text.
pub fn team_requirement(team_id: &str) -> Option<String> {
    is_team_id(team_id)
        .then(|| format!("anchor apple generic and certificate leaf[subject.OU] = \"{team_id}\""))
}

fn untrusted(reason: impl fmt::Display) -> BundledHelperError {
    BundledHelperError::Untrusted(reason.to_string())
}

/// What `codesign` should be pointed at: the enclosing `.app` when `binary` is
/// `<X>.app/Contents/MacOS/<file>` (the sealed, signed unit), else the file itself.
fn signed_unit(binary: &Path) -> &Path {
    let named = |p: &Path, n: &str| p.file_name().is_some_and(|f| f == OsStr::new(n));
    let layout = binary.parent().and_then(|macos| {
        let contents = macos.parent()?;
        let app = contents.parent()?;
        (named(macos, "MacOS")
            && named(contents, "Contents")
            && app
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("app")))
        .then_some(app)
    });
    layout.unwrap_or(binary)
}

/// Verify `path` against the Developer ID team `team_id` and return the canonical path that was
/// verified, which the caller must execute (not `path`). Fails closed on every doubt.
///
/// The path is canonicalised and `/usr/bin/codesign --verify --strict -R "=<requirement>"
/// <target>` is run (absolute path, no `PATH` lookup, no shell), where `<target>` is the
/// enclosing `.app` when the path has the form `<X>.app/Contents/MacOS/<file>` (the signed unit)
/// and the file itself otherwise. The check is bounded to 20 s; a timeout kills the `codesign`
/// process. A non-zero exit, a timeout, a missing or unrunnable `codesign` (including every
/// non-macOS platform), a path that is not a regular file, or a `team_id` that is not
/// [`is_team_id`] is [`BundledHelperError::Untrusted`]. There is no skip or override.
///
/// The check and a later spawn are two steps on the same canonical path, so a process that can
/// replace files inside the signed bundle between them could still run a different program.
pub fn verify_team_signature(path: &Path, team_id: &str) -> Result<PathBuf, BundledHelperError> {
    let requirement =
        team_requirement(team_id).ok_or_else(|| untrusted("the team id is not valid"))?;
    let canonical = path
        .canonicalize()
        .map_err(|e| untrusted(format!("cannot resolve the helper path ({})", e.kind())))?;
    if !canonical.is_file() {
        return Err(untrusted("the helper is not a regular file"));
    }
    // `-R` takes a requirement file path unless the text starts with `=`.
    let requirement = OsString::from(format!("={requirement}"));
    let target = signed_unit(&canonical);
    let args: [&OsStr; 5] = [
        OsStr::new("--verify"),
        OsStr::new("--strict"),
        OsStr::new("-R"),
        &requirement,
        target.as_os_str(),
    ];
    match run_bounded(Path::new(CODESIGN), &args, CODESIGN_TIMEOUT) {
        Ok(Bounded::Exited(status)) if status.success() => Ok(canonical),
        Ok(Bounded::Exited(status)) => Err(untrusted(format!(
            "the code signature check failed ({status})"
        ))),
        Ok(Bounded::TimedOut) => Err(untrusted(format!(
            "the code signature check did not finish within {} s",
            CODESIGN_TIMEOUT.as_secs()
        ))),
        Err(e) => Err(untrusted(format!(
            "the code signature check could not run ({})",
            e.kind()
        ))),
    }
}

enum Bounded {
    Exited(std::process::ExitStatus),
    TimedOut,
}

/// Run `program` with no stdin and no output, wait at most `limit`, and kill and reap it if it
/// is still running then (or if waiting fails). Output goes to the null device so there is no
/// pipe that could fill and stall the child.
///
/// Only the child itself is killed, not a process tree: `codesign` starts no children that
/// outlive it. The poll sleeps at most 10 ms at a time and the call ends by `limit`.
fn run_bounded(program: &Path, args: &[&OsStr], limit: Duration) -> std::io::Result<Bounded> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let now = Instant::now();
    let deadline = now
        .checked_add(limit)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 60 * 60));
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Bounded::Exited(status)),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Bounded::TimedOut);
        }
        std::thread::sleep(left.min(Duration::from_millis(10)));
    }
}
