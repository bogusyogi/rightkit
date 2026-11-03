//! QA runs never touch real user data.
//!
//! A test run may not start an app on the user's real data or on a location that can raise a
//! system dialog (macOS file-access prompts for Documents, Desktop, Downloads, iCloud Drive and
//! removable or network volumes; the same cloud-sync folders on Windows). [`Guard`] is evaluated
//! by `control::launch` (and by the engine launch paths) before anything is started, with no
//! opt-out for apps: a violation is a typed [`UserDataViolation`] naming the path and the rule,
//! and nothing is launched. An engine launch must also name an isolated data directory
//! ([`check_engine_launch`]); the one exception is a probe of the engine's own refusal, available
//! on Unix only and only with HOME redirected.
//!
//! The rule for an app launched with a workspace:
//! * the workspace home must be a QA root this runner created (it sits directly under a
//!   `rightkit-qa` directory inside a system temp root on the internal disk and carries the
//!   `.rightkit-app.json` binding), and must itself break none of the forbidden-location rules;
//! * every directory passed to the app (the launch working directory and each absolute path in
//!   the environment) must resolve, symlinks followed, inside that home.
//!
//! Forbidden anywhere: the user's Documents, Desktop, Downloads, Pictures, Music and Movies;
//! iCloud Drive (`~/Library/Mobile Documents`, `~/Library/CloudStorage`), OneDrive, Dropbox and
//! Google Drive folders; the user's real application-support / AppData / `.config` directory for
//! the app under test; any `/Volumes/*` volume; any Windows UNC (network) path.
//!
//! Scenarios that need existing state build it with generated fixtures (the default) or with
//! [`seed_from`], which refuses to read the real per-user data of the app under test unless the
//! scenario declares the copy scrubbed and a verification callback returns true before launch.
use crate::storage;
use crate::util::{err, Error, Result};
use crate::workspace::QaWorkspace;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf, Prefix};

/// Why a path was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// The user's home, Documents, Desktop, Downloads, Pictures, Music or Movies.
    UserFolder,
    /// iCloud Drive, OneDrive, Dropbox, Google Drive or another cloud-synced folder.
    CloudStorage,
    /// An external or network volume (`/Volumes/*`, a UNC path).
    ExternalVolume,
    /// The user's real application-support / AppData / config directory of the app under test.
    RealAppData,
    /// Not inside the QA root the runner created for this launch.
    OutsideQaRoot,
    /// Not inside a system temp root on the internal disk.
    OutsideTemp,
    /// The directory is not a QA home created by this runner (no binding file).
    NotQaRoot,
    /// The path cannot be judged (relative, or containing `..`).
    Unresolvable,
    /// A seed copy reads real user data without a verified scrub.
    UnscrubbedRealData,
    /// The launch names no isolated data directory, so the app or engine would resolve its real
    /// per-user folder by its own default.
    NoIsolatedDataDir,
    /// A launch argument is on the target's deny list (a switch that selects the real library).
    ForbiddenArgument,
    /// The refusal-probe opt-out is unavailable: no second barrier stands between the child and
    /// the real per-user folder (Windows always; elsewhere HOME / XDG homes not redirected).
    ProbeUnavailable,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Rule::UserFolder => "user-folder",
            Rule::CloudStorage => "cloud-storage",
            Rule::ExternalVolume => "external-volume",
            Rule::RealAppData => "real-app-data",
            Rule::OutsideQaRoot => "outside-qa-root",
            Rule::OutsideTemp => "outside-temp",
            Rule::NotQaRoot => "not-a-qa-root",
            Rule::Unresolvable => "unresolvable",
            Rule::UnscrubbedRealData => "unscrubbed-real-data",
            Rule::NoIsolatedDataDir => "no-isolated-data-dir",
            Rule::ForbiddenArgument => "forbidden-argument",
            Rule::ProbeUnavailable => "refusal-probe-unavailable",
        })
    }
}

/// A launch refused before anything started. `From<UserDataViolation> for Error` keeps `?` working
/// in code that returns [`crate::Result`]; match on `rule` where the reason matters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDataViolation {
    pub path: PathBuf,
    pub rule: Rule,
    pub detail: String,
}

impl fmt::Display for UserDataViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "QA refuses to touch {} (rule {}): {}; nothing was launched",
            self.path.display(),
            self.rule,
            self.detail
        )
    }
}
impl std::error::Error for UserDataViolation {}
impl From<UserDataViolation> for Error {
    fn from(v: UserDataViolation) -> Self {
        Error(v.to_string())
    }
}

const USER_FOLDERS: &[&str] = &[
    "documents",
    "desktop",
    "downloads",
    "pictures",
    "music",
    "movies",
    "videos",
];

/// Resolve symlinks in the longest existing prefix and append the rest, so a path that does not
/// exist yet is still judged by where its existing parents really are.
pub fn resolve(path: &Path) -> PathBuf {
    let mut base = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = fs::canonicalize(&base) {
            let mut out = c;
            for t in tail.iter().rev() {
                out.push(t);
            }
            return out;
        }
        match (base.file_name(), base.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                base = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn comps(p: &Path) -> Vec<String> {
    p.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect()
}

fn alnum(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_network_prefix(p: &Path) -> bool {
    matches!(
        p.components().next(),
        Some(Component::Prefix(pre)) if matches!(pre.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
    )
}

/// Rules applied to the components that follow a user's home directory.
fn under_home(rest: &[String], app: Option<&str>) -> Option<(Rule, String)> {
    if rest.is_empty() {
        return Some((Rule::UserFolder, "the user's home directory".into()));
    }
    if USER_FOLDERS.contains(&rest[0].as_str()) {
        return Some((Rule::UserFolder, format!("the user's {} folder", rest[0])));
    }
    if rest[0] == "library"
        && matches!(
            rest.get(1).map(String::as_str),
            Some("mobile documents") | Some("cloudstorage")
        )
    {
        return Some((Rule::CloudStorage, "iCloud Drive / CloudStorage".into()));
    }
    let app = alnum(app?);
    if app.is_empty() {
        return None;
    }
    let hit = |i: usize| rest.get(i).is_some_and(|c| alnum(c).contains(&app));
    let second = rest.get(1).map(String::as_str);
    let in_set = |set: &[&str]| second.is_some_and(|s| set.contains(&s));
    let found = match rest[0].as_str() {
        "library" => {
            in_set(&[
                "application support",
                "containers",
                "group containers",
                "preferences",
                "caches",
                "saved application state",
                "webkit",
                "logs",
            ]) && hit(2)
        }
        "appdata" => in_set(&["roaming", "local", "locallow"]) && hit(2),
        ".local" => in_set(&["share", "state"]) && hit(2),
        ".config" | ".cache" => hit(1),
        _ => false,
    };
    found.then(|| {
        (
            Rule::RealAppData,
            "the user's real data directory of the app under test".into(),
        )
    })
}

/// What decides whether a launch may proceed. Build one with [`Guard::for_workspace`] (apps) or
/// [`Guard::for_temp_roots`] (engines without a workspace).
#[derive(Debug, Clone)]
pub struct Guard {
    app: Option<String>,
    user_homes: Vec<PathBuf>,
    temp_roots: Vec<PathBuf>,
    qa_root: Option<PathBuf>,
}

fn default_user_homes() -> Vec<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .filter(|v| !v.is_empty())
        .map(|v| resolve(Path::new(&v)))
        .collect()
}

fn default_temp_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        resolve(&storage::app_temp_dir()),
        resolve(&std::env::temp_dir()),
        resolve(&storage::home_base()),
    ];
    if let Some(t) = std::env::var_os("RUNNER_TEMP").filter(|v| !v.is_empty()) {
        roots.push(resolve(Path::new(&t)));
    }
    roots.sort();
    roots.dedup();
    roots
}

/// The QA root of a QA home, or `None` when the home is in no place this runner creates one:
/// - a home directly under a `rightkit-qa` directory (a single-scenario run) is its own root;
/// - the home of a scenario world the pool created (`<rightkit-qa>/<run>/worlds/wNNN/home`,
///   `crate::pool::WorldPlan::world`) has the world directory `wNNN` as its root, because the
///   world's control socket and token file live there beside the home (`pool::world_env`).
fn qa_root_of(home: &Path) -> Option<PathBuf> {
    let named = |p: Option<&Path>, name: &str| {
        p.and_then(Path::file_name) == Some(std::ffi::OsStr::new(name))
    };
    if named(home.parent(), "rightkit-qa") {
        return Some(home.to_path_buf());
    }
    let world = home.parent();
    let is_world = world
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .is_some_and(|n| {
            n.len() == 4 && n.starts_with('w') && n[1..].bytes().all(|b| b.is_ascii_digit())
        });
    let worlds = world.and_then(Path::parent);
    let run = worlds.and_then(Path::parent);
    let pooled = named(Some(home), "home")
        && is_world
        && named(worlds, "worlds")
        && named(run.and_then(Path::parent), "rightkit-qa");
    pooled.then(|| world.expect("a world home has a parent").to_path_buf())
}

impl Guard {
    /// Guard for an engine or any launch without a workspace: paths must sit in a system temp
    /// root and break no forbidden-location rule.
    pub fn for_temp_roots(app: Option<&str>) -> Guard {
        Guard {
            app: app.map(str::to_owned),
            user_homes: default_user_homes(),
            temp_roots: default_temp_roots(),
            qa_root: None,
        }
    }

    /// Guard for an app launched in `ws`. Fails when the home is not a QA root this runner created
    /// or is itself in a forbidden location.
    pub fn for_workspace(ws: &QaWorkspace) -> std::result::Result<Guard, UserDataViolation> {
        let mut guard = Guard::for_temp_roots(None);
        let home = resolve(&ws.home);
        let fail = |rule, detail: String| UserDataViolation {
            path: home.clone(),
            rule,
            detail,
        };
        let binding = fs::read(home.join(".rightkit-app.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        let Some(binding) = binding else {
            return Err(fail(
                Rule::NotQaRoot,
                "no .rightkit-app.json binding: not a QA home created by rightkit-qa".into(),
            ));
        };
        guard.app = binding
            .get("app")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let Some(root) = qa_root_of(&home) else {
            return Err(fail(
                Rule::NotQaRoot,
                "the home is neither directly under a rightkit-qa directory nor a scenario world's home (<rightkit-qa>/<run>/worlds/wNNN/home)".into(),
            ));
        };
        guard.judge_outside_workspace(&root)?;
        guard.qa_root = Some(root);
        Ok(guard)
    }

    /// Replace the user homes the guard protects (default: `HOME` and `USERPROFILE` of this
    /// process). Tests point it at a fake home.
    pub fn with_user_homes(mut self, homes: Vec<PathBuf>) -> Guard {
        self.user_homes = homes.iter().map(|h| resolve(h)).collect();
        self
    }

    /// Name the app under test; its real data directories become forbidden.
    pub fn with_app(mut self, app: &str) -> Guard {
        self.app = Some(app.to_owned());
        self
    }

    /// First forbidden-location rule `resolved` breaks, ignoring any QA root.
    pub fn forbidden(&self, resolved: &Path) -> Option<(Rule, String)> {
        if is_network_prefix(resolved) {
            return Some((Rule::ExternalVolume, "a network (UNC) path".into()));
        }
        let c = comps(resolved);
        let has_prefix = matches!(resolved.components().next(), Some(Component::Prefix(_)));
        // A temp root or user home may itself sit on a /Volumes disk (the Mac build home is /Volumes/D);
        // paths under them are judged by the home, temp and QA-root rules instead (as in @rightkit/qa).
        let on_known_root = self
            .temp_roots
            .iter()
            .chain(self.user_homes.iter())
            .any(|root| resolved.starts_with(root));
        if !has_prefix && !on_known_root && c.first().map(String::as_str) == Some("volumes") {
            return Some((
                Rule::ExternalVolume,
                "an external or network volume under /Volumes".into(),
            ));
        }
        for (i, name) in c.iter().enumerate() {
            let cloud = name.starts_with("onedrive")
                || name.starts_with("dropbox")
                || name.starts_with("google drive")
                || name == "googledrive"
                || name == "my drive"
                || name == "icloud drive"
                || name == "iclouddrive"
                || name == "mobile documents"
                || (name == "cloudstorage" && i > 0 && c[i - 1] == "library");
            if cloud {
                return Some((
                    Rule::CloudStorage,
                    format!("a cloud-synced folder ({name})"),
                ));
            }
        }
        // `/Users/<name>/...` and `C:\Users\<name>\...` for any account, not only this process's.
        if matches!(c.first().map(String::as_str), Some("users") | Some("home")) && c.len() >= 2 {
            if let Some(hit) = under_home(&c[2..], self.app.as_deref()) {
                if c.len() > 2 {
                    return Some(hit);
                }
            }
        }
        for home in &self.user_homes {
            if let Ok(rel) = resolved.strip_prefix(home) {
                if let Some(hit) = under_home(&comps(rel), self.app.as_deref()) {
                    return Some(hit);
                }
            }
        }
        None
    }

    fn judge_outside_workspace(
        &self,
        resolved: &Path,
    ) -> std::result::Result<(), UserDataViolation> {
        let violation = |rule, detail: String| UserDataViolation {
            path: resolved.to_path_buf(),
            rule,
            detail,
        };
        if let Some((rule, detail)) = self.forbidden(resolved) {
            return Err(violation(rule, detail));
        }
        if !self.temp_roots.iter().any(|t| resolved.starts_with(t)) {
            return Err(violation(
                Rule::OutsideTemp,
                "not inside a system temp directory on the internal disk".into(),
            ));
        }
        Ok(())
    }

    /// Judge one directory the runner would hand to the app. Returns its resolved path.
    pub fn check_dir(&self, path: &Path) -> std::result::Result<PathBuf, UserDataViolation> {
        if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
            return Err(UserDataViolation {
                path: path.to_path_buf(),
                rule: Rule::Unresolvable,
                detail: "the path must be absolute and contain no `..`".into(),
            });
        }
        let resolved = resolve(path);
        if let Some(root) = &self.qa_root {
            if resolved.starts_with(root) {
                return Ok(resolved);
            }
            if let Some((rule, detail)) = self.forbidden(&resolved) {
                return Err(UserDataViolation {
                    path: resolved,
                    rule,
                    detail,
                });
            }
            return Err(UserDataViolation {
                detail: format!("not inside the QA root {}", root.display()),
                path: resolved,
                rule: Rule::OutsideQaRoot,
            });
        }
        self.judge_outside_workspace(&resolved)?;
        Ok(resolved)
    }

    /// Judge every absolute-path value of a launch environment.
    pub fn check_env(
        &self,
        env: &[(String, String)],
    ) -> std::result::Result<(), UserDataViolation> {
        for (_, value) in env {
            if Path::new(value).is_absolute() {
                self.check_dir(Path::new(value))?;
            }
        }
        Ok(())
    }
}

/// Engine launches have no workspace: judge the data directory and the environment values that
/// name data, home, profile or config directories.
///
/// This judges only what it is given. A launch that names no directory passes it, so engine
/// launches go through [`check_engine_launch`], which also refuses a launch with no isolated data
/// directory.
pub fn check_engine_dirs(data_dir: Option<&Path>, env: &[(String, String)]) -> Result<()> {
    Ok(engine_dirs_violation(
        &Guard::for_temp_roots(None),
        data_dir,
        env,
    )?)
}

fn engine_dirs_violation(
    guard: &Guard,
    data_dir: Option<&Path>,
    env: &[(String, String)],
) -> std::result::Result<(), UserDataViolation> {
    if let Some(dir) = data_dir {
        guard.check_dir(dir)?;
    }
    for (key, value) in env {
        let k = key.to_uppercase();
        let names_a_dir = ["DATA", "HOME", "PROFILE", "CONFIG"]
            .iter()
            .any(|w| k.contains(w))
            || matches!(k.as_str(), "APPDATA" | "LOCALAPPDATA" | "TMPDIR");
        if names_a_dir && Path::new(value).is_absolute() {
            guard.check_dir(Path::new(value))?;
        }
    }
    Ok(())
}

/// Everything the guard judges about one engine, sidecar or app-engine launch, before the process
/// starts. `env` is the environment the launcher will set on the child (target values, the data
/// directory variable, then the caller's values, in that order: the last value of a key wins, as
/// with `Command::env`).
#[derive(Debug, Clone)]
pub struct EngineLaunch<'a> {
    /// The program that would start; named in every refusal.
    pub binary: &'a Path,
    /// Arguments handed to the program.
    pub args: &'a [String],
    /// The directory the caller passed as the launch's data directory.
    pub data_dir: Option<&'a Path>,
    /// Environment variable through which the target receives its data directory.
    pub data_dir_env: Option<&'a str>,
    /// Flag through which the target takes its data directory in `args` (`--data-dir <dir>` or
    /// `--data-dir=<dir>`).
    pub data_dir_arg: Option<&'a str>,
    /// Flags the target declares as "use the real library"; a launch carrying one is refused.
    pub forbidden_args: &'a [String],
    /// The environment the child will get from the launcher.
    pub env: &'a [(String, String)],
    /// The caller opted out of the isolated-directory rule to probe the engine's own refusal.
    pub refusal_probe: bool,
}

fn env_value<'e>(env: &'e [(String, String)], key: &str, windows: bool) -> Option<&'e str> {
    env.iter()
        .rev()
        .find(|(k, _)| {
            if windows {
                k.eq_ignore_ascii_case(key)
            } else {
                k == key
            }
        })
        .map(|(_, v)| v.as_str())
}

/// Value of the last `flag <value>` / `flag=<value>` in `args`.
fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let mut found = None;
    for (i, a) in args.iter().enumerate() {
        if a == flag {
            found = args.get(i + 1).map(String::as_str);
        } else if let Some(v) = a.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            found = Some(v);
        }
    }
    found
}

/// Judge one engine launch. A launch is refused, before anything starts, when:
/// 1. an argument equals (or is `flag=...` of) a flag in `forbidden_args`;
/// 2. the data directory, or an absolute environment value naming data, home, profile or config,
///    breaks [`check_engine_dirs`]'s rules;
/// 3. it names no isolated data directory: neither the `data_dir_env` variable nor the
///    `data_dir_arg` flag carries a non-empty value that [`Guard::check_dir`] accepts. A
///    `data_dir` the target cannot deliver (no `data_dir_env`) does not count, because the engine
///    would never see it and would resolve its real per-user folder by its own default.
///
/// With `refusal_probe` the third rule is replaced by a second barrier, available only on Unix:
/// the child environment must redirect `HOME` into a directory that passes the guard, and
/// `XDG_DATA_HOME` / `XDG_CONFIG_HOME` (set by the launcher or inherited from this process) must
/// do the same where set. On Windows the probe is refused outright: the engine finds its folder
/// through the Known Folder API, which ignores the environment, so no environment redirect is a
/// barrier there.
pub fn check_engine_launch(v: &EngineLaunch<'_>) -> std::result::Result<(), UserDataViolation> {
    check_engine_launch_in(
        &Guard::for_temp_roots(None),
        v,
        cfg!(windows),
        &|k: &str| std::env::var(k).ok(),
    )
}

fn check_engine_launch_in(
    guard: &Guard,
    v: &EngineLaunch<'_>,
    windows: bool,
    inherited: &dyn Fn(&str) -> Option<String>,
) -> std::result::Result<(), UserDataViolation> {
    let target = v.binary.display().to_string();
    let refuse = |rule, detail: String| UserDataViolation {
        path: v.binary.to_path_buf(),
        rule,
        detail,
    };
    for arg in v.args {
        for flag in v.forbidden_args {
            if arg == flag
                || arg
                    .strip_prefix(flag.as_str())
                    .is_some_and(|r| r.starts_with('='))
            {
                return Err(refuse(
                    Rule::ForbiddenArgument,
                    format!("launch of {target} carries {flag}, which the target declares as a switch to the real library"),
                ));
            }
        }
    }
    engine_dirs_violation(guard, v.data_dir, v.env)?;
    let mut named = 0usize;
    if let Some(var) = v.data_dir_env {
        if let Some(value) = env_value(v.env, var, windows).filter(|s| !s.is_empty()) {
            guard.check_dir(Path::new(value))?;
            named += 1;
        }
    }
    if let Some(flag) = v.data_dir_arg {
        if let Some(value) = arg_value(v.args, flag).filter(|s| !s.is_empty()) {
            guard.check_dir(Path::new(value))?;
            named += 1;
        }
    }
    if named > 0 {
        return Ok(());
    }
    if !v.refusal_probe {
        let why = if v.data_dir.is_some() {
            "a data directory was passed, but the target declares no data_dir_env, so the engine would never receive it"
        } else {
            "no data directory was passed"
        };
        return Err(refuse(
            Rule::NoIsolatedDataDir,
            format!("launch of {target} names no isolated data directory ({why}); an isolated directory is required, because the engine would resolve its real per-user folder by its own default. Pass one through the target's data_dir_env (or data_dir_arg); to assert the engine's own refusal on Unix, use allow_no_data_dir_for_refusal_probe with HOME redirected"),
        ));
    }
    if windows {
        return Err(refuse(
            Rule::ProbeUnavailable,
            format!("launch of {target} names no isolated data directory, and the refusal-probe opt-out is unavailable on Windows: the engine resolves its per-user folder through the Known Folder API, which ignores HOME, USERPROFILE and APPDATA in the child environment, so nothing would stand between it and the real folder"),
        ));
    }
    let barrier = |detail: String| {
        refuse(
            Rule::ProbeUnavailable,
            format!("launch of {target} names no isolated data directory, and the refusal-probe opt-out needs the child environment to redirect HOME and any XDG data/config home into a QA directory: {detail}"),
        )
    };
    let home = env_value(v.env, "HOME", false)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| barrier("HOME is not set in the child environment".into()))?;
    guard
        .check_dir(Path::new(home))
        .map_err(|e| barrier(format!("HOME: {e}")))?;
    for key in ["XDG_DATA_HOME", "XDG_CONFIG_HOME"] {
        let effective = env_value(v.env, key, false)
            .map(str::to_owned)
            .or_else(|| inherited(key))
            .filter(|s| !s.is_empty());
        if let Some(value) = effective {
            guard
                .check_dir(Path::new(&value))
                .map_err(|e| barrier(format!("{key}: {e}")))?;
        }
    }
    Ok(())
}

/// App launches get their data directory from the workspace: the launch environment must carry
/// `RIGHTKIT_QA_DATA_DIR`, pointing inside the QA root `guard` was built for. A workspace whose
/// environment lost it is refused rather than launched on the app's own default folder.
pub fn check_app_data_dir(
    guard: &Guard,
    app_binary: &Path,
    env: &[(String, String)],
) -> std::result::Result<(), UserDataViolation> {
    match env_value(env, "RIGHTKIT_QA_DATA_DIR", cfg!(windows)).filter(|s| !s.is_empty()) {
        Some(dir) => guard.check_dir(Path::new(dir)).map(|_| ()),
        None => Err(UserDataViolation {
            path: app_binary.to_path_buf(),
            rule: Rule::NoIsolatedDataDir,
            detail: format!(
                "launch of {} names no isolated data directory (RIGHTKIT_QA_DATA_DIR is missing from the launch environment); an isolated directory is required, because the app would resolve its real per-user folder by its own default",
                app_binary.display()
            ),
        }),
    }
}

/// How a seed copy proves it holds no user data.
pub struct ScrubDeclaration<'a> {
    /// The scenario states that its transform removes all user data.
    pub declared_scrubbed: bool,
    /// Called on the seeded copy before launch; must return true (for example "zero user rows").
    pub verify: &'a dyn Fn(&Path) -> bool,
}

/// Seed `<data_dir>/<dest_rel>` from `source` through the caller's `transform(source, dest)`.
///
/// Generated fixtures are the default and need no scrub. A `source` that is the real per-user data
/// of `app` (or in any forbidden location) is refused unless `scrub` declares the copy scrubbed and
/// its callback returns true on the result; when it returns false the copy is deleted and the
/// seed fails, so nothing launches on it.
pub fn seed_from(
    ws: &QaWorkspace,
    app: &str,
    source: &Path,
    dest_rel: &Path,
    transform: impl FnOnce(&Path, &Path) -> Result<()>,
    scrub: Option<&ScrubDeclaration<'_>>,
) -> Result<PathBuf> {
    let guard = Guard::for_workspace(ws)?.with_app(app);
    seed_from_guarded(&guard, ws, source, dest_rel, transform, scrub)
}

/// [`seed_from`] with an explicit guard (tests use a fake user home).
pub fn seed_from_guarded(
    guard: &Guard,
    ws: &QaWorkspace,
    source: &Path,
    dest_rel: &Path,
    transform: impl FnOnce(&Path, &Path) -> Result<()>,
    scrub: Option<&ScrubDeclaration<'_>>,
) -> Result<PathBuf> {
    if dest_rel.is_absolute() || dest_rel.components().any(|c| c == Component::ParentDir) {
        return err("seed destination must be relative to the QA data directory");
    }
    let dest = guard.check_dir(&ws.data_dir.join(dest_rel))?;
    let resolved_source = resolve(source);
    let real = guard.forbidden(&resolved_source);
    if let Some((_, detail)) = &real {
        match scrub {
            None => {
                return Err(UserDataViolation {
                    path: resolved_source,
                    rule: Rule::UnscrubbedRealData,
                    detail: format!("{detail}; use a generated fixture, or declare the copy scrubbed and supply a verification callback"),
                }
                .into())
            }
            Some(s) if !s.declared_scrubbed => {
                return Err(UserDataViolation {
                    path: resolved_source,
                    rule: Rule::UnscrubbedRealData,
                    detail: format!("{detail}; the scenario did not declare the copy scrubbed"),
                }
                .into())
            }
            Some(_) => {}
        }
    }
    transform(source, &dest)?;
    if let Some(s) = scrub {
        if (real.is_some() || s.declared_scrubbed) && !(s.verify)(&dest) {
            // The copy lives inside the QA data directory the guard just verified.
            let _ = if dest.is_dir() {
                fs::remove_dir_all(&dest)
            } else {
                fs::remove_file(&dest)
            };
            return Err(UserDataViolation {
                path: dest,
                rule: Rule::UnscrubbedRealData,
                detail: "the verification callback rejected the seeded copy; it was deleted and nothing launches".into(),
            }
            .into());
        }
    }
    Ok(dest)
}

/// Names of the macOS agents that show system permission prompts.
const SYSTEM_DIALOG_OWNERS: &[&str] = &[
    "usernotificationcenter",
    "coreservicesuiagent",
    "universalaccessauthwarn",
    "securityagent",
    "tccd",
];

/// True when `name` is a macOS process that owns permission prompts.
pub fn is_system_dialog_owner(name: &str) -> bool {
    let n = name.to_lowercase();
    SYSTEM_DIALOG_OWNERS.iter().any(|o| n == *o)
}

/// Outcome of a permission-dialog check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogCheck {
    /// Evidence shows no dialog.
    Clear,
    /// A dialog owner was seen.
    Dialog(String),
    /// This build cannot observe system dialogs; the string says why. Not evidence of absence.
    Unsupported(&'static str),
}

/// Positive evidence only: the first sampled foreground name that belongs to a system dialog
/// agent, from `StopReport::frontmost_states`. An empty answer proves nothing, because these
/// agents rarely become frontmost.
pub fn permission_dialog_in(frontmost_states: &[String]) -> Option<String> {
    frontmost_states
        .iter()
        .find(|n| is_system_dialog_owner(n))
        .cloned()
}

/// TODO: observe live TCC prompts. `rightkit_automation::macos::background::windows(pid)` lists windows of one pid
/// only; there is no enumeration of windows by owner name (`UserNotificationCenter`,
/// `CoreServicesUIAgent`), so no reliable detector exists in this crate yet. This hook reports
/// [`DialogCheck::Unsupported`] instead of guessing. Replace the body once the automation layer
/// can list windows by owner without clicking them.
pub fn live_permission_dialog_check(_app_pid: u32) -> DialogCheck {
    DialogCheck::Unsupported(
        "no window-by-owner enumeration in rightkit-qa-driver; permission dialogs are not observable",
    )
}
