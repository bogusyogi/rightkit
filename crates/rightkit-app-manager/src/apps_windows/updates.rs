//! Update checks and updates for installed apps on Windows, through winget.
//!
//! Check: `winget upgrade --include-unknown --disable-interactivity` run hidden
//! (CREATE_NO_WINDOW) and its table parsed. The Microsoft.WinGet.Client COM API was
//! not used: it needs WinRT/COM activation for the same list the CLI prints, and
//! the CLI is what a person would run. The CLI has no JSON output for `upgrade`, so
//! the table is read by splitting a row at runs of two or more spaces (winget pads
//! columns to at least that), which also keeps the parse independent of wide
//! characters and of the language's column titles; only the column order (Name,
//! Id, Version, Available, Source) is assumed.
//!
//! Update: `winget upgrade --id <id> --exact` for the one app. winget asks for its
//! own elevation when the installer needs it.
//!
//! A winget row is tied to an installed app by the Uninstall key name inside its
//! id (`ARP\Machine\X64\Git_is1`) or by an unambiguous name match; rows that match
//! no listed app are dropped because there is no app to show them on.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::usage::norm;
use super::{Installed, now_secs, run_capture, run_installer};

const CHECK_TIMEOUT: Duration = Duration::from_secs(180);
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(1800);
/// winget exit codes that mean "nothing to list" rather than a failure.
const NO_APPLICATIONS_FOUND: u32 = 0x8A15_0014;
const NO_APPLICABLE_UPGRADE: u32 = 0x8A15_002B;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppUpdate {
    pub path: String,
    pub name: String,
    pub bundle_id: Option<String>,
    pub installed_version: Option<String>,
    /// "winget".
    pub source: String,
    /// "available" (only state this module reports).
    pub state: String,
    pub latest_version: Option<String>,
    pub cask: Option<String>,
    pub store_url: Option<String>,
    pub reason: Option<String>,
    pub checked_at: i64,
}

/// What one check found.
pub struct Found {
    pub checked_at: i64,
    pub apps: Vec<AppUpdate>,
    /// App path -> (winget id, winget source name).
    pub targets: HashMap<String, (String, String)>,
}

// ---------------------------------------------------------------------------
// Running winget
// ---------------------------------------------------------------------------

/// The App Installer alias of winget in %LOCALAPPDATA%\Microsoft\WindowsApps when it is
/// there (that folder is often missing from the PATH of a Store-installed winget), else
/// `winget.exe` from PATH. The alias is an app-execution-alias reparse point, which
/// `Path::exists` can report as missing, so it is checked with `symlink_metadata`.
fn winget_path() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let alias = PathBuf::from(local)
            .join("Microsoft")
            .join("WindowsApps")
            .join("winget.exe");
        if std::fs::symlink_metadata(&alias).is_ok() {
            return alias;
        }
    }
    PathBuf::from("winget.exe")
}

/// winget's exit code (as the unsigned HRESULT) and what it printed, or an error
/// when it could not be started or did not finish in time.
fn run_winget(args: &[&str], timeout: Duration, installer: bool) -> Result<(u32, String), String> {
    let mut command = rightkit_child_process::command_std(winget_path());
    command.args(args);
    let missing = "winget is not installed. It comes with App Installer from the Microsoft Store.";
    if installer {
        run_installer(command, timeout, "winget", missing)
    } else {
        run_capture(command, timeout, "winget", missing)
    }
}

// ---------------------------------------------------------------------------
// Parsing the table
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct WingetRow {
    name: String,
    id: String,
    version: String,
    available: String,
    source: Option<String>,
}

/// One printed line without progress-spinner carriage returns and backspaces.
fn clean(line: &str) -> String {
    let last = line
        .rsplit('\r')
        .find(|segment| !segment.trim().is_empty())
        .unwrap_or("");
    last.chars()
        .filter(|c| *c != '\u{8}')
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn is_rule(line: &str) -> bool {
    let text = line.trim();
    text.matches('-').count() >= 8
        && text
            .chars()
            .all(|c| matches!(c, '-' | ' ' | '\\' | '|' | '/'))
}

/// Splits at runs of two or more spaces; single spaces stay inside a cell.
fn cells(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut spaces = 0usize;
    for c in line.trim().chars() {
        if c == ' ' {
            spaces += 1;
            continue;
        }
        if spaces >= 2 && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        } else if spaces == 1 {
            current.push(' ');
        }
        spaces = 0;
        current.push(c);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Every upgrade row of every table in `text` (winget prints a second table for
/// packages that need explicit targeting).
fn parse_upgrades(text: &str) -> Vec<WingetRow> {
    let lines: Vec<String> = text.split('\n').map(clean).collect();
    let mut rows = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !is_rule(&lines[index]) {
            index += 1;
            continue;
        }
        index += 1;
        while index < lines.len() {
            let line = &lines[index];
            if line.trim().is_empty() {
                break;
            }
            let parts = cells(line);
            // Footers ("2 upgrades available.") have one cell and end the table.
            if parts.len() < 4 || parts[1].contains(' ') {
                break;
            }
            rows.push(WingetRow {
                name: parts[0].clone(),
                id: parts[1].clone(),
                version: parts[2].clone(),
                available: parts[3].clone(),
                source: parts.get(4).cloned(),
            });
            index += 1;
        }
    }
    rows
}

// ---------------------------------------------------------------------------
// Tying rows to installed apps
// ---------------------------------------------------------------------------

fn match_row<'a>(row: &WingetRow, apps: &'a [Installed]) -> Option<&'a Installed> {
    // An id of the form ARP\Machine\X64\<Uninstall key name>.
    if row.id.contains('\\')
        && let Some(tail) = row.id.rsplit('\\').next()
    {
        let hits: Vec<&Installed> = apps
            .iter()
            .filter(|a| a.key_name.eq_ignore_ascii_case(tail))
            .collect();
        if hits.len() == 1 {
            return Some(hits[0]);
        }
    }
    let wanted = norm(row.name.trim_end_matches('\u{2026}'));
    if wanted.len() < 3 {
        return None;
    }
    let exact: Vec<&Installed> = apps
        .iter()
        .filter(|a| norm(&a.entry.name) == wanted)
        .collect();
    if exact.len() == 1 {
        return Some(exact[0]);
    }
    if !exact.is_empty() {
        return None;
    }
    // winget's name is often the product name and the key's name carries a version or "(x64)".
    let loose: Vec<&Installed> = apps
        .iter()
        .filter(|a| {
            let have = norm(&a.entry.name);
            wanted.len() >= 5
                && (have.starts_with(&wanted) || wanted.starts_with(&have) && have.len() >= 5)
        })
        .collect();
    if loose.len() == 1 {
        Some(loose[0])
    } else {
        None
    }
}

/// A target that cannot be mistaken for an option by winget.
pub fn safe_argument(text: &str) -> bool {
    !text.is_empty() && !text.starts_with('-') && !text.chars().any(char::is_control)
}

/// Runs `winget upgrade` and ties each row to an app in `installed`. `on_row` is
/// called with each update as it is found.
pub fn check(installed: &[Installed], on_row: &dyn Fn(&AppUpdate)) -> Result<Found, String> {
    let (code, text) = run_winget(
        &[
            "upgrade",
            "--include-unknown",
            "--disable-interactivity",
            "--accept-source-agreements",
        ],
        CHECK_TIMEOUT,
        false,
    )?;
    let rows = parse_upgrades(&text);
    if rows.is_empty()
        && code != 0
        && code != NO_APPLICATIONS_FOUND
        && code != NO_APPLICABLE_UPGRADE
    {
        return Err(format!(
            "winget could not list updates (exit code {code:#010X})."
        ));
    }
    let now = now_secs();
    let mut found = Found {
        checked_at: now,
        apps: Vec::new(),
        targets: HashMap::new(),
    };
    for row in rows {
        let Some(app) = match_row(&row, installed) else {
            continue;
        };
        let path = app.entry.path.clone();
        if found.targets.contains_key(&path) || !safe_argument(&row.id) {
            continue;
        }
        let unknown_installed = row.version.eq_ignore_ascii_case("unknown");
        let update = AppUpdate {
            path: path.clone(),
            name: app.entry.name.clone(),
            bundle_id: None,
            installed_version: app
                .entry
                .version
                .clone()
                .or_else(|| (!unknown_installed).then(|| row.version.clone())),
            source: "winget".into(),
            state: "available".into(),
            latest_version: Some(row.available.clone()),
            cask: None,
            store_url: None,
            reason: unknown_installed
                .then(|| "winget could not read the installed version.".to_string()),
            checked_at: now,
        };
        on_row(&update);
        found.targets.insert(
            path,
            (row.id.clone(), row.source.clone().unwrap_or_default()),
        );
        found.apps.push(update);
    }
    Ok(found)
}

/// The last line of winget's output that says something, for a failure message.
fn last_message(text: &str) -> String {
    text.split('\n')
        .map(clean)
        .rfind(|line| !line.trim().is_empty() && !is_rule(line))
        .unwrap_or_default()
}

/// Upgrades one app with winget (blocks until it ends, up to 30 minutes). `id` and
/// `source` are a `Found::targets` value. Err carries what to tell the person.
pub fn upgrade(id: &str, source: &str) -> Result<(), String> {
    if !safe_argument(id) {
        return Err("winget gave an id that cannot be used.".into());
    }
    let mut args: Vec<&str> = vec![
        "upgrade",
        "--id",
        id,
        "--exact",
        "--silent",
        "--disable-interactivity",
        "--accept-source-agreements",
        "--accept-package-agreements",
    ];
    if !source.is_empty() && safe_argument(source) {
        args.extend(["--source", source]);
    }
    match run_winget(&args, UPGRADE_TIMEOUT, true)? {
        (0, _) => Ok(()),
        (code, text) => {
            let detail = last_message(&text);
            Err(if detail.is_empty() {
                format!("winget exit code {code:#010X}.")
            } else {
                detail
            })
        }
    }
}

// ---------------------------------------------------------------------------
// The saved result, its freshness, and the policy around an upgrade
// ---------------------------------------------------------------------------

/// A saved check is reused for this long unless a forced check is asked for (six hours).
pub const FRESH_SECS: i64 = 6 * 3600;

/// The report an Apps page shows: when the check ran and the apps with an update.
#[derive(Clone, Serialize, Deserialize)]
pub struct UpdateReport {
    pub checked_at: Option<i64>,
    pub apps: Vec<AppUpdate>,
}

/// What a host keeps between launches: the last check and its upgrade targets.
#[derive(Clone, Serialize, Deserialize)]
pub struct Saved {
    pub checked_at: i64,
    pub apps: Vec<AppUpdate>,
    /// App path -> (winget id, winget source name).
    pub targets: HashMap<String, (String, String)>,
}

impl From<Found> for Saved {
    fn from(found: Found) -> Self {
        Saved {
            checked_at: found.checked_at,
            apps: found.apps,
            targets: found.targets,
        }
    }
}

/// The report for a saved check; an empty one, with no time, when there is none.
pub fn report_of(saved: Option<&Saved>) -> UpdateReport {
    match saved {
        Some(saved) => UpdateReport {
            checked_at: Some(saved.checked_at),
            apps: saved.apps.clone(),
        },
        None => UpdateReport {
            checked_at: None,
            apps: Vec::new(),
        },
    }
}

/// True when `saved` is younger than [`FRESH_SECS`] at `now`.
pub fn is_fresh(saved: &Saved, now: i64) -> bool {
    now - saved.checked_at < FRESH_SECS
}

/// Why an automatic update did not run for an app (#176).
pub const HELD: &str = "Automatic update held: no verified previous package to restore with Undo.";

/// Whether a verified previous package exists for winget id `id`, so an upgrade could be undone
/// by reinstalling it. Nothing records or tests one yet (#176 acceptance), so this is false for
/// every app; an automatic upgrade never runs until it is built and tested. Policy follows
/// RightKit's `@rightkit/app-update`: nothing installs on its own unless the policy says so.
pub fn previous_package_verified(_id: &str) -> bool {
    false
}

/// The version the app's Uninstall key shows now, when it is newer than `before` or equals
/// `latest`. `read_version` reads the app's current version by path (a fresh inventory read);
/// `host_name` words the refusal. Err says what to tell the person; the app is not called
/// updated then (#176: an update is reported done only once the new version is read back).
pub fn verify_new_version(
    read_version: &dyn Fn(&str) -> Option<String>,
    host_name: &str,
    path: &str,
    before: Option<&str>,
    latest: Option<&str>,
) -> Result<String, String> {
    let now = read_version(path).ok_or_else(|| {
        format!(
            "winget finished, but {host_name} could not read the app's version afterwards, so it does not say it was updated."
        )
    })?;
    let reached = latest.is_some_and(|l| l == now)
        || before.is_some_and(|b| {
            crate::apps::compare_versions(b, &now) == crate::apps::VersionComparison::Newer
        });
    if reached {
        Ok(now)
    } else {
        Err(format!(
            "winget finished, but the app still shows version {now}, so it is not marked updated."
        ))
    }
}

/// The check of an upgraded app: (package id, expected version, expected publisher) to the
/// installed version, or why it does not match.
pub type VerifyFn = dyn Fn(&str, Option<&str>, Option<&str>) -> Result<String, String>;

/// After a check with automatic updates switched on (`enabled`): upgrades each eligible app and
/// verifies it; every other app keeps its row with the reason it was held ([`HELD`]). Returns
/// whether `saved` changed. `upgrade` runs one winget upgrade (id, source); `verify` is
/// [`verify_new_version`] with the host's reader.
pub fn auto_update(
    saved: &mut Saved,
    enabled: bool,
    upgrade: &dyn Fn(&str, &str) -> Result<(), String>,
    verify: &VerifyFn,
) -> bool {
    if !enabled {
        return false;
    }
    let mut changed = false;
    let mut done: Vec<String> = Vec::new();
    for row in saved.apps.iter_mut() {
        let Some((id, source)) = saved.targets.get(&row.path).cloned() else {
            continue;
        };
        if !previous_package_verified(&id) {
            if row.reason.as_deref() != Some(HELD) {
                row.reason = Some(HELD.to_string());
                changed = true;
            }
            continue;
        }
        if !safe_argument(&id) || upgrade(&id, &source).is_err() {
            continue;
        }
        if verify(
            &row.path,
            row.installed_version.as_deref(),
            row.latest_version.as_deref(),
        )
        .is_ok()
        {
            done.push(row.path.clone());
        }
    }
    if !done.is_empty() {
        saved.apps.retain(|row| !done.contains(&row.path));
        for path in &done {
            saved.targets.remove(path);
        }
        changed = true;
    }
    changed
}

/// The upgrade target and the versions around it for one app of a saved check, or what to tell
/// the person when there is none. `Ok((id, source, before, latest))`.
#[allow(clippy::type_complexity)]
pub fn upgrade_target(
    saved: Option<&Saved>,
    path: &str,
) -> Result<(String, String, Option<String>, Option<String>), String> {
    let saved = saved.ok_or("Check for updates first.")?;
    let (id, source) = saved.targets.get(path).cloned().ok_or_else(|| {
        "winget lists no update for this app. Check for updates again.".to_string()
    })?;
    let (before, latest) = saved
        .apps
        .iter()
        .find(|row| row.path == path)
        .map(|row| (row.installed_version.clone(), row.latest_version.clone()))
        .unwrap_or((None, None));
    if !safe_argument(&id) {
        return Err("winget gave an id that cannot be used.".into());
    }
    Ok((id, source, before, latest))
}

/// Drops an updated app from a saved check, so the old answer is not shown again.
pub fn forget_updated(saved: &mut Saved, path: &str) {
    saved.apps.retain(|row| row.path != path);
    saved.targets.remove(path);
}
