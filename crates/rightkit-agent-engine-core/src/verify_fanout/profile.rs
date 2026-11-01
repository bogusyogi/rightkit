//! Project-aware verification profile (PL-007 discovery leg).
//!
//! Discovery is strictly read-only: it never mutates the workspace, installs
//! nothing, and resolves everything *before* any verifier executes.
//!
//! Two discovery surfaces:
//!
//! * `<state-dir>/verification.json` (schema `version: 1`) — the project's
//!   declared verification commands as argv arrays (no shell strings), plus
//!   the `lspRequired` promotion flag. A present-but-malformed file is a
//!   *required* configuration failure, never a silent default.
//! * Python project discovery — for each verified `*.py` file, the nearest
//!   ancestor (bounded by the workspace root) carrying `pyproject.toml`,
//!   `setup.cfg`, or `requirements.txt` is the project root; the interpreter
//!   resolves from that project's `.venv`/`venv` first, then the workspace
//!   root's, then a provisioned real `python`/`py` on PATH.
//!
//! `find_on_path` honors the `RIGHTKIT_VERIFY_PATH` override (falling back
//! to `PATH`) so tests can point executable discovery at a scratch directory
//! without racing the process environment.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::finding::FindingCategory;
use super::verifiers::CommandVerifierSpec;

/// One verified Python file with its resolved project context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPythonFile {
    /// The verified file.
    pub path: PathBuf,
    /// Nearest ancestor with a Python project marker (or the workspace root
    /// when no marker exists).
    pub project_root: PathBuf,
    /// The resolved interpreter, or `None` when no `.venv`/`venv`/PATH Python
    /// could be found. `None` is a *required* unavailability, not a skip.
    pub interpreter: Option<PathBuf>,
}

/// Everything the fan-out knows about the project before any unit runs.
#[derive(Debug, Clone, Default)]
pub struct VerificationProfile {
    /// Commands declared by `<state-dir>/verification.json` (empty when the
    /// file is absent; also empty when malformed — the `config_error`
    /// failure is the required signal for that case).
    pub commands: Vec<CommandVerifierSpec>,
    /// `lspRequired` from the config: promotes LSP units from optional
    /// feedback to required gate coverage. Defaults `false`.
    pub lsp_required: bool,
    /// Resolved Python context per verified `*.py` file.
    pub python_files: Vec<ResolvedPythonFile>,
    /// Provisioned `pyright` CLI, resolved only when Python files are in
    /// scope. `None` means the optional typecheck unit reports unavailable.
    pub pyright: Option<PathBuf>,
    /// Set when `<state-dir>/verification.json` exists but failed strict
    /// parsing/validation. Surfaced as a required Failed outcome.
    pub config_error: Option<String>,
}

pub fn verification_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".rightkit").join("verification.json")
}

// ─── <state-dir>/verification.json (schema v1) ──────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationConfigFile {
    version: u32,
    #[serde(default, rename = "lspRequired")]
    lsp_required: bool,
    #[serde(default)]
    commands: Vec<CommandConfigEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandConfigEntry {
    id: String,
    category: String,
    /// argv array: `["py", "-m", "pytest", "{paths}"]`. Never a shell string.
    argv: Vec<String>,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default, rename = "pathPrefixes")]
    path_prefixes: Vec<String>,
    #[serde(default = "default_required")]
    required: bool,
    #[serde(default)]
    fixable: bool,
}

fn default_required() -> bool {
    true
}

/// Map a wire category string to the normalized finding category. Accepts the
/// stable `FindingCategory::as_str` vocabulary; anything else is rejected so
/// a typo is a config failure, not a silent "test" bucket.
pub(crate) fn category_from_wire(raw: &str) -> Option<FindingCategory> {
    match raw {
        "lsp" => Some(FindingCategory::Lsp),
        "syntax" => Some(FindingCategory::Syntax),
        "typecheck" => Some(FindingCategory::TypeCheck),
        "lint" => Some(FindingCategory::Lint),
        "format" => Some(FindingCategory::Format),
        "static-analysis" => Some(FindingCategory::StaticAnalysis),
        "dependency" => Some(FindingCategory::Dependency),
        "config" => Some(FindingCategory::Config),
        "test" => Some(FindingCategory::Test),
        _ => None,
    }
}

/// Load `<state-dir>/verification.json` under strict schema v1 rules.
///
/// * Absent → `Ok(None)` (defaults apply).
/// * Present but malformed, wrong version, unknown fields, empty/unsafe argv,
///   unknown category, empty id, or duplicate id → `Err(reason)`; the caller
///   surfaces it as a required configuration failure.
fn load_verification_config(
    workspace_root: &Path,
) -> Result<Option<(bool, Vec<CommandVerifierSpec>)>, String> {
    let path = verification_config_path(workspace_root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "{}: could not read verification config: {error}",
                path.display()
            ))
        }
    };
    let parsed: VerificationConfigFile = serde_json::from_str(&text).map_err(|error| {
        format!(
            "{}: malformed verification config (schema v1): {error}",
            path.display()
        )
    })?;
    if parsed.version != 1 {
        return Err(format!(
            "{}: unsupported verification config version {}; expected 1",
            path.display(),
            parsed.version
        ));
    }
    let mut commands: Vec<CommandVerifierSpec> = Vec::with_capacity(parsed.commands.len());
    for entry in parsed.commands {
        let id = entry.id.trim().to_string();
        if id.is_empty() {
            return Err(format!(
                "{}: a verification command is missing its id",
                path.display()
            ));
        }
        if commands.iter().any(|spec| spec.id == id) {
            return Err(format!(
                "{}: duplicate verification command id '{id}'",
                path.display()
            ));
        }
        let Some(category) = category_from_wire(entry.category.trim()) else {
            return Err(format!(
                "{}: verification command '{id}' has unknown category '{}'",
                path.display(),
                entry.category
            ));
        };
        if entry.argv.is_empty() || entry.argv[0].trim().is_empty() {
            return Err(format!(
                "{}: verification command '{id}' has an empty or unsafe argv",
                path.display()
            ));
        }
        commands.push(CommandVerifierSpec {
            id,
            category,
            argv: entry.argv,
            applicable_extensions: entry
                .extensions
                .iter()
                .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|ext| !ext.is_empty())
                .collect(),
            path_prefixes: entry
                .path_prefixes
                .iter()
                .map(|prefix| prefix.replace('\\', "/"))
                .map(|prefix| prefix.trim_matches('/').to_string())
                .filter(|prefix| !prefix.is_empty())
                .collect(),
            required: entry.required,
            fixable: entry.fixable,
        });
    }
    Ok(Some((parsed.lsp_required, commands)))
}

// ─── Executable & Python project discovery (read-only) ─────────────────────

/// Resolve a bare executable name against the search path
/// (`RIGHTKIT_VERIFY_PATH` override, else `PATH`), honoring `PATHEXT`
/// suffixes on Windows. Returns the resolved file path.
#[cfg(not(feature = "host"))]
pub(crate) fn find_on_path(_name: &str) -> Option<PathBuf> {
    // The portable (mobile) profile never resolves an executable from the
    // process environment: it spawns none.
    None
}

#[cfg(feature = "host")]
pub(crate) fn find_on_path(name: &str) -> Option<PathBuf> {
    let dirs = std::env::var_os("RIGHTKIT_VERIFY_PATH").or_else(|| std::env::var_os("PATH"))?;
    let pathext: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .map(|value| {
                value
                    .split(';')
                    .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
                    .filter(|ext| !ext.is_empty())
                    .collect()
            })
            .unwrap_or_else(|_| vec!["exe".into(), "cmd".into(), "bat".into(), "com".into()])
    } else {
        Vec::new()
    };
    for dir in std::env::split_paths(&dirs) {
        let direct = dir.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        for ext in &pathext {
            let candidate = dir.join(format!("{name}.{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolve a configured program for the governed launch preflight: absolute
/// or separator-bearing paths resolve relative to the workspace root; bare
/// names resolve via [`find_on_path`]. `None` means the program is not
/// provisioned — the unit reports `Unavailable` rather than attempting a
/// spawn that can only fail.
pub(crate) fn resolve_program(program: &str, workspace_root: &Path) -> Option<PathBuf> {
    let trimmed = program.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "{python}" {
        return resolve_python_interpreter(workspace_root, workspace_root);
    }
    let path = Path::new(trimmed);
    if path.is_absolute() || trimmed.contains('/') || trimmed.contains('\\') {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace_root.join(path)
        };
        return candidate.is_file().then_some(candidate);
    }
    find_on_path(trimmed)
}

/// The nearest ancestor of `file` (bounded by `workspace_root`) carrying a
/// Python project marker: `pyproject.toml`, `setup.cfg`, or
/// `requirements.txt`. Falls back to the workspace root when no marker
/// exists so interpreter discovery still checks the workspace's own venvs.
pub(crate) fn find_python_project_root(file: &Path, workspace_root: &Path) -> PathBuf {
    const MARKERS: [&str; 3] = ["pyproject.toml", "setup.cfg", "requirements.txt"];
    let mut dir = if file.is_dir() {
        file.to_path_buf()
    } else {
        file.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace_root.to_path_buf())
    };
    loop {
        if MARKERS.iter().any(|marker| dir.join(marker).is_file()) {
            return dir;
        }
        if dir == workspace_root || !dir.starts_with(workspace_root) {
            break;
        }
        if !dir.pop() {
            break;
        }
    }
    workspace_root.to_path_buf()
}

/// Resolve the project Python interpreter: the project's own `.venv`/`venv`
/// first (`Scripts/python.exe` on Windows layouts, `bin/python` on POSIX —
/// both are checked regardless of host so checked-out venvs resolve
/// everywhere), then the workspace root's, then a provisioned real
/// `python`/`py` (Windows) or `python3`/`python` (POSIX) on the search path.
/// No interpreter is ever installed or guessed.
pub(crate) fn resolve_python_interpreter(
    project_root: &Path,
    workspace_root: &Path,
) -> Option<PathBuf> {
    let mut roots = vec![project_root.to_path_buf()];
    if workspace_root != project_root {
        roots.push(workspace_root.to_path_buf());
    }
    for root in roots {
        let candidates = if cfg!(windows) {
            [
                root.join(".venv/Scripts/python.exe"),
                root.join("venv/Scripts/python.exe"),
            ]
        } else {
            [root.join(".venv/bin/python"), root.join("venv/bin/python")]
        };
        for candidate in candidates {
            if is_real_interpreter(&candidate) {
                return Some(candidate);
            }
        }
    }
    #[cfg(feature = "host")]
    {
        let dirs = std::env::var_os("PATH")?;
        find_python_on_paths(&dirs)
    }
    #[cfg(not(feature = "host"))]
    {
        None
    }
}

/// A file that can actually be an interpreter. On Windows the Microsoft Store
/// `python.exe` app-execution alias is a 0-byte file or a reparse point (it
/// can live outside a `WindowsApps` directory through a copied or shortened
/// PATH entry); launching it opens the Store instead of running Python, so it
/// is never an interpreter.
fn is_real_interpreter(candidate: &Path) -> bool {
    if !candidate.is_file() {
        return false;
    }
    if !cfg!(windows) {
        return true;
    }
    let Ok(link) = std::fs::symlink_metadata(candidate) else {
        return false;
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if link.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    link.len() > 0
}

#[cfg_attr(not(feature = "host"), allow(dead_code))]
fn find_python_on_paths(dirs: &std::ffi::OsStr) -> Option<PathBuf> {
    find_python_in(dirs, interpreter_runs)
}

#[cfg_attr(not(feature = "host"), allow(dead_code))]
fn find_python_in(dirs: &std::ffi::OsStr, accept: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    // Names are ranked: a concrete interpreter anywhere on the search path
    // beats the `py.exe` launcher, which indirects through the registry and
    // per-user install locations and is fragile inside a sandbox.
    let names: &[&str] = if cfg!(windows) {
        &["python.exe", "py.exe"]
    } else {
        &["python3", "python"]
    };
    for name in names {
        for dir in std::env::split_paths(dirs) {
            if cfg!(windows)
                && dir.components().any(|part| {
                    part.as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case("WindowsApps")
                })
            {
                continue;
            }
            let candidate = dir.join(name);
            if is_real_interpreter(&candidate) && accept(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Read/execute roots a sandboxed launch of `program` needs when it is a
/// Python interpreter: the interpreter's own install directory (the standard
/// library lives beside it) and, for a venv, the base interpreter directory
/// named by `pyvenv.cfg` `home =`. Nothing broader is ever returned; non-Python
/// programs yield no roots.
pub(crate) fn interpreter_read_roots(program: &Path) -> Vec<PathBuf> {
    let is_python = program
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|stem| stem == "py" || stem.starts_with("python"));
    if !is_python {
        return Vec::new();
    }
    let Some(dir) = program.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
        return Vec::new();
    };
    let mut roots = vec![dir.to_path_buf()];
    // A venv keeps pyvenv.cfg beside Scripts/ or bin/ (or beside the exe in
    // a relocated layout).
    let cfg = [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .map(|candidate| candidate.join("pyvenv.cfg"))
        .find(|candidate| candidate.is_file());
    if let Some(text) = cfg.and_then(|cfg| std::fs::read_to_string(cfg).ok()) {
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("home") {
                let home = PathBuf::from(value.trim());
                if !home.as_os_str().is_empty() && !roots.contains(&home) {
                    roots.push(home);
                }
            }
        }
    }
    roots
}

/// A launcher such as `py.exe` can exist yet have no Python installed behind
/// it (it exits non-zero with "No installed Python found!"). Only accept a
/// candidate that actually executes `-c pass` successfully within a short
/// deadline.
#[cfg_attr(not(feature = "host"), allow(dead_code))]
fn interpreter_runs(candidate: &Path) -> bool {
    let options = rightkit_child_process::RunOptions::new(std::time::Duration::from_secs(5))
        .stdout_cap(0)
        .stderr_cap(0);
    rightkit_child_process::run(candidate, ["-c", "pass"], &options)
        .is_ok_and(|output| !output.timed_out && output.status.success())
}

/// Resolve the whole profile for one fan-out run. Read-only; never installs,
/// never guesses broad test suites — only configured argv commands and the
/// default Python syntax check are produced.
pub fn resolve_profile(workspace_root: &Path, verified_paths: &[PathBuf]) -> VerificationProfile {
    let mut profile = VerificationProfile::default();
    match load_verification_config(workspace_root) {
        Ok(Some((lsp_required, commands))) => {
            profile.lsp_required = lsp_required;
            profile.commands = commands;
        }
        Ok(None) => {}
        Err(error) => profile.config_error = Some(error),
    }
    for path in verified_paths {
        if path.extension().and_then(|e| e.to_str()) != Some("py") {
            continue;
        }
        let project_root = find_python_project_root(path, workspace_root);
        let interpreter = resolve_python_interpreter(&project_root, workspace_root);
        profile.python_files.push(ResolvedPythonFile {
            path: path.clone(),
            project_root,
            interpreter,
        });
    }
    if !profile.python_files.is_empty() {
        profile.pyright = find_on_path("pyright");
    }
    profile
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_profile_cannot_silently_drop_required_checks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".rightkit")).unwrap();
        std::fs::write(
            verification_config_path(dir.path()),
            r#"{"version":1,"commnads":[]}"#,
        )
        .unwrap();
        assert!(resolve_profile(dir.path(), &[])
            .config_error
            .unwrap()
            .contains("unknown field"));
    }

    #[test]
    fn nearest_project_venv_precedes_workspace_interpreter() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("nested");
        let suffix = if cfg!(windows) {
            ".venv/Scripts/python.exe"
        } else {
            ".venv/bin/python"
        };
        for root in [dir.path(), project.as_path()] {
            std::fs::create_dir_all(root.join(suffix).parent().unwrap()).unwrap();
            std::fs::write(root.join(suffix), "discovery fixture").unwrap();
            std::fs::write(root.join("pyproject.toml"), "[project]\nname='fixture'\n").unwrap();
        }
        let profile = resolve_profile(dir.path(), &[project.join("x.py")]);
        assert_eq!(profile.python_files[0].project_root, project);
        assert_eq!(
            profile.python_files[0].interpreter,
            Some(project.join(suffix))
        );
        assert_eq!(
            resolve_program("{python}", dir.path()),
            Some(dir.path().join(suffix))
        );
    }

    #[cfg(windows)]
    #[test]
    fn concrete_python_outranks_the_py_launcher_on_any_path_entry() {
        let dir = tempfile::tempdir().unwrap();
        let launcher_dir = dir.path().join("launcher");
        let python_dir = dir.path().join("python");
        for path in [&launcher_dir, &python_dir] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(launcher_dir.join("py.exe"), "discovery fixture").unwrap();
        std::fs::write(python_dir.join("python.exe"), "discovery fixture").unwrap();
        // The launcher's directory comes first on PATH but must still lose.
        let paths = std::env::join_paths([&launcher_dir, &python_dir]).unwrap();
        assert_eq!(
            find_python_in(&paths, |_| true),
            Some(python_dir.join("python.exe"))
        );
        // With no concrete interpreter, the launcher is still a fallback.
        let only_launcher = std::env::join_paths([&launcher_dir]).unwrap();
        assert_eq!(
            find_python_in(&only_launcher, |_| true),
            Some(launcher_dir.join("py.exe"))
        );
    }

    #[test]
    fn interpreter_read_roots_are_exactly_install_dir_and_venv_home() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Python311");
        std::fs::create_dir_all(&install).unwrap();
        assert_eq!(
            interpreter_read_roots(&install.join("python.exe")),
            vec![install.clone()]
        );
        assert!(interpreter_read_roots(&install.join("node.exe")).is_empty());

        let venv_scripts = dir.path().join("proj/.venv/Scripts");
        std::fs::create_dir_all(&venv_scripts).unwrap();
        std::fs::write(
            dir.path().join("proj/.venv/pyvenv.cfg"),
            format!("version = 3.11.9\nhome = {}\n", install.display()),
        )
        .unwrap();
        assert_eq!(
            interpreter_read_roots(&venv_scripts.join("python.exe")),
            vec![venv_scripts.clone(), install]
        );
    }

    #[cfg(windows)]
    #[test]
    fn python_discovery_skips_cmd_shims_and_windows_app_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let shims = dir.path().join("shims");
        let aliases = dir.path().join("Microsoft/WindowsApps");
        let real = dir.path().join("provisioned");
        for path in [&shims, &aliases, &real] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(shims.join("python.cmd"), "shim").unwrap();
        std::fs::write(aliases.join("python.exe"), "alias").unwrap();
        std::fs::write(real.join("python.exe"), "discovery fixture").unwrap();
        let paths = std::env::join_paths([&shims, &aliases, &real]).unwrap();
        // Discovery only: the fixture is not an executable, so skip the run probe.
        assert_eq!(
            find_python_in(&paths, |_| true),
            Some(real.join("python.exe"))
        );
    }
}
