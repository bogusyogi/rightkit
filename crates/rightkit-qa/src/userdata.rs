//! QA runs never touch real user data.
//!
//! A test run may not start an app on the user's real data or on a location that can raise a
//! system dialog (macOS file-access prompts for Documents, Desktop, Downloads, iCloud Drive and
//! removable or network volumes; the same cloud-sync folders on Windows). [`Guard`] is evaluated
//! by `control::launch` (and by the engine launch paths) before anything is started, with no
//! opt-out: a violation is a typed [`UserDataViolation`] naming the path and the rule, and
//! nothing is launched.
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
        if home.parent().and_then(Path::file_name) != Some(std::ffi::OsStr::new("rightkit-qa")) {
            return Err(fail(
                Rule::NotQaRoot,
                "the home is not directly under a rightkit-qa directory".into(),
            ));
        }
        guard.judge_outside_workspace(&home)?;
        guard.qa_root = Some(home);
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
        if !has_prefix && c.first().map(String::as_str) == Some("volumes") {
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
pub fn check_engine_dirs(data_dir: Option<&Path>, env: &[(String, String)]) -> Result<()> {
    let guard = Guard::for_temp_roots(None);
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
        if real.is_some() || s.declared_scrubbed {
            if !(s.verify)(&dest) {
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

/// TODO: observe live TCC prompts. `rightkit_control::mac::windows(pid)` lists windows of one pid
/// only; there is no enumeration of windows by owner name (`UserNotificationCenter`,
/// `CoreServicesUIAgent`), so no reliable detector exists in this crate yet. This hook reports
/// [`DialogCheck::Unsupported`] instead of guessing. Replace the body once the automation layer
/// can list windows by owner without clicking them.
pub fn live_permission_dialog_check(_app_pid: u32) -> DialogCheck {
    DialogCheck::Unsupported(
        "no window-by-owner enumeration in rightkit-control; permission dialogs are not observable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::new_id;

    struct Fx {
        root: PathBuf,
        fake_home: PathBuf,
        ws: QaWorkspace,
    }

    fn fx() -> Fx {
        let root = storage::app_temp_dir().join(format!("rkqa-ud-{}", new_id()));
        let fake_home = root.join("fakehome");
        for d in [
            "Documents",
            "Desktop",
            "Downloads",
            "Library/Mobile Documents/com~apple~CloudDocs",
            "Library/CloudStorage/OneDrive-Acme",
            "Library/Application Support/ViewRight/db",
            "AppData/Roaming/ViewRight",
            "Dropbox",
            "Elsewhere",
        ] {
            fs::create_dir_all(fake_home.join(d)).unwrap();
        }
        let ws = crate::workspace::create(&root.join("managed"), None, "viewright").unwrap();
        Fx {
            root,
            fake_home,
            ws,
        }
    }

    fn done(f: Fx) {
        let _ = crate::workspace::dispose(&f.ws, false, 1, 0);
        let _ = fs::remove_dir_all(&f.root);
    }

    fn guard(f: &Fx) -> Guard {
        Guard::for_workspace(&f.ws)
            .unwrap()
            .with_user_homes(vec![f.fake_home.clone()])
    }

    #[test]
    fn every_forbidden_location_is_refused_with_its_rule() {
        let f = fx();
        let g = guard(&f);
        let h = &f.fake_home;
        let cases: Vec<(PathBuf, Rule)> = vec![
            (h.join("Documents"), Rule::UserFolder),
            (h.join("Documents/sub/dir"), Rule::UserFolder),
            (h.join("Desktop"), Rule::UserFolder),
            (h.join("Downloads"), Rule::UserFolder),
            (h.clone(), Rule::UserFolder),
            (
                h.join("Library/Mobile Documents/com~apple~CloudDocs"),
                Rule::CloudStorage,
            ),
            (
                h.join("Library/CloudStorage/OneDrive-Acme"),
                Rule::CloudStorage,
            ),
            (h.join("Dropbox"), Rule::CloudStorage),
            (
                h.join("Library/Application Support/ViewRight/db"),
                Rule::RealAppData,
            ),
            (h.join("AppData/Roaming/ViewRight"), Rule::RealAppData),
        ];
        for (path, rule) in cases {
            let v = g.check_dir(&path).unwrap_err();
            assert_eq!(v.rule, rule, "{}", path.display());
            // The refused path is reported in resolved form; on Windows that is the canonical
            // spelling of the home, which the fixture's own spelling does not start.
            assert!(
                v.to_string().contains(&path.display().to_string())
                    || v.path.starts_with(&f.fake_home)
                    || v.path.starts_with(resolve(&f.fake_home)),
                "{v}"
            );
        }
        // A directory in the fake home that no rule names is still outside the QA root.
        let v = g.check_dir(&h.join("Elsewhere")).unwrap_err();
        assert_eq!(v.rule, Rule::OutsideQaRoot);
        done(f);
    }

    #[cfg(unix)]
    #[test]
    fn external_volumes_and_other_accounts_are_refused() {
        let f = fx();
        let g = guard(&f);
        let v = g
            .check_dir(Path::new("/Volumes/Backup/qa-home"))
            .unwrap_err();
        assert_eq!(v.rule, Rule::ExternalVolume);
        let v = g
            .check_dir(Path::new("/Users/someone-else/Documents/x"))
            .unwrap_err();
        assert_eq!(v.rule, Rule::UserFolder);
        let v = g
            .check_dir(Path::new(
                "/Users/someone-else/Library/Application Support/ViewRight",
            ))
            .unwrap_err();
        assert_eq!(v.rule, Rule::RealAppData);
        let v = g.check_dir(Path::new("relative/path")).unwrap_err();
        assert_eq!(v.rule, Rule::Unresolvable);
        done(f);
    }

    #[test]
    fn a_created_qa_root_and_its_directories_are_accepted() {
        let f = fx();
        let g = guard(&f);
        for dir in [
            f.ws.home.clone(),
            f.ws.data_dir.clone(),
            f.ws.home.join("tmp"),
            f.ws.home.join("not/created/yet"),
        ] {
            g.check_dir(&dir).unwrap();
        }
        let env: Vec<(String, String)> = f.ws.env.clone().into_iter().collect();
        g.check_env(&env).unwrap();
        done(f);
    }

    #[test]
    fn a_directory_that_is_not_a_qa_root_is_not_accepted_as_one() {
        let f = fx();
        let mut ws = f.ws.clone();
        ws.home = f.root.join("fakehome/Elsewhere");
        assert_eq!(Guard::for_workspace(&ws).unwrap_err().rule, Rule::NotQaRoot);
        ws.home = f.root.clone();
        assert_eq!(Guard::for_workspace(&ws).unwrap_err().rule, Rule::NotQaRoot);
        done(f);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_qa_root_is_refused() {
        let f = fx();
        let g = guard(&f);
        let into_documents = f.ws.home.join("looks-internal");
        std::os::unix::fs::symlink(f.fake_home.join("Documents"), &into_documents).unwrap();
        assert_eq!(
            g.check_dir(&into_documents).unwrap_err().rule,
            Rule::UserFolder
        );
        assert_eq!(
            g.check_dir(&into_documents.join("deeper/file"))
                .unwrap_err()
                .rule,
            Rule::UserFolder
        );
        let plain = f.ws.home.join("to-plain");
        std::os::unix::fs::symlink(f.fake_home.join("Elsewhere"), &plain).unwrap();
        assert_eq!(g.check_dir(&plain).unwrap_err().rule, Rule::OutsideQaRoot);
        done(f);
    }

    #[test]
    fn an_environment_path_outside_the_root_is_refused() {
        let f = fx();
        let g = guard(&f);
        let env = vec![(
            "VIEWRIGHT_DATA_DIR".to_string(),
            f.fake_home
                .join("Library/Application Support/ViewRight")
                .to_string_lossy()
                .into_owned(),
        )];
        assert_eq!(g.check_env(&env).unwrap_err().rule, Rule::RealAppData);
        done(f);
    }

    #[test]
    fn engine_data_dirs_must_be_in_temp_and_clear_of_user_folders() {
        let f = fx();
        check_engine_dirs(Some(&f.ws.data_dir), &[]).unwrap();
        let g = Guard::for_temp_roots(Some("viewright")).with_user_homes(vec![f.fake_home.clone()]);
        assert_eq!(
            g.check_dir(&f.fake_home.join("Documents"))
                .unwrap_err()
                .rule,
            Rule::UserFolder
        );
        done(f);
    }

    #[test]
    fn seed_refuses_real_app_data_without_a_verified_scrub() {
        let f = fx();
        let g = guard(&f);
        let real = f.fake_home.join("Library/Application Support/ViewRight/db");
        fs::write(real.join("app.sqlite"), "user rows").unwrap();
        let copy = |from: &Path, to: &Path| -> Result<()> {
            fs::create_dir_all(to)?;
            fs::copy(from.join("app.sqlite"), to.join("app.sqlite"))?;
            Ok(())
        };
        let rel = Path::new("seed");
        // No declaration.
        let e = seed_from_guarded(&g, &f.ws, &real, rel, copy, None).unwrap_err();
        assert!(e.to_string().contains("unscrubbed-real-data"), "{e}");
        assert!(!f.ws.data_dir.join("seed").exists());
        // Declared false.
        let no = ScrubDeclaration {
            declared_scrubbed: false,
            verify: &|_| true,
        };
        assert!(seed_from_guarded(&g, &f.ws, &real, rel, copy, Some(&no)).is_err());
        // Declared, but verification fails: copy deleted.
        let bad = ScrubDeclaration {
            declared_scrubbed: true,
            verify: &|_| false,
        };
        assert!(seed_from_guarded(&g, &f.ws, &real, rel, copy, Some(&bad)).is_err());
        assert!(!f.ws.data_dir.join("seed").exists());
        // Declared and verified.
        let good = ScrubDeclaration {
            declared_scrubbed: true,
            verify: &|dest| fs::write(dest.join("app.sqlite"), "").is_ok(),
        };
        let out = seed_from_guarded(&g, &f.ws, &real, rel, copy, Some(&good)).unwrap();
        assert_eq!(fs::read(out.join("app.sqlite")).unwrap(), b"");
        // A generated fixture needs no scrub.
        let generated = f.ws.scenario_dir("generated").unwrap();
        fs::write(generated.join("fixture.txt"), "fixture").unwrap();
        let ok = seed_from_guarded(
            &g,
            &f.ws,
            &generated,
            Path::new("seed2"),
            |from, to| {
                fs::create_dir_all(to)?;
                fs::copy(from.join("fixture.txt"), to.join("fixture.txt"))?;
                Ok(())
            },
            None,
        )
        .unwrap();
        assert!(ok.join("fixture.txt").exists());
        // Destination escapes are refused.
        assert!(seed_from_guarded(
            &g,
            &f.ws,
            &generated,
            Path::new("../x"),
            |_, _| Ok(()),
            None
        )
        .is_err());
        done(f);
    }

    #[test]
    fn dialog_owner_matching() {
        assert!(is_system_dialog_owner("UserNotificationCenter"));
        assert!(is_system_dialog_owner("CoreServicesUIAgent"));
        assert!(!is_system_dialog_owner("Finder"));
        assert_eq!(
            permission_dialog_in(&["Finder".into(), "CoreServicesUIAgent".into()]).as_deref(),
            Some("CoreServicesUIAgent")
        );
        assert_eq!(permission_dialog_in(&["Finder".into()]), None);
        assert!(matches!(
            live_permission_dialog_check(1),
            DialogCheck::Unsupported(_)
        ));
    }
}
