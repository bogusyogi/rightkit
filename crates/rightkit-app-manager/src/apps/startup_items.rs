//! Startup and login items: what starts by itself. Read only; nothing is enabled, disabled or
//! removed here.
//!
//! macOS: launch agents and daemons, read from the plist files in `<home>/Library/LaunchAgents`,
//! `/Library/LaunchAgents` and `/Library/LaunchDaemons` with `/usr/bin/plutil`. Login items set in
//! System Settings are not listed.
//! Windows: the Run keys (HKCU, HKLM and the WOW6432Node copy) with their StartupApproved state, and
//! the per-user and common Startup folders, by shortcut file name only (targets are not resolved).
//! Scheduled tasks and services are not listed. The registry is read through the host's
//! `RegistryRead`, so this crate needs no registry dependency.
//!
//! The whole read is bounded: 2 seconds overall and 500 rows at most.

use std::path::Path;
#[cfg(any(target_os = "macos", windows))]
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::apps::registry::RegistryRead;

/// The whole read stops after this long; sources not yet read are noted.
#[cfg(any(target_os = "macos", windows))]
const BUDGET: Duration = Duration::from_secs(2);
/// One `plutil` run may take at most this long.
#[cfg(target_os = "macos")]
const PER_FILE: Duration = Duration::from_millis(400);
const MAX_ROWS: usize = 500;

/// One thing that starts by itself.
#[derive(Serialize)]
pub struct StartupItem {
    pub name: String,
    pub kind: &'static str,
    /// "user", "all users" or "system".
    pub scope: &'static str,
    /// The plist, shortcut or executable; null when there is no file.
    pub path: Option<String>,
    /// What it runs; null when unknown.
    pub program: Option<String>,
    /// Null when it cannot be told.
    pub enabled: Option<bool>,
    /// Bundle id or publisher when cheaply known; null otherwise.
    pub vendor: Option<String>,
}

/// The rows and the notes about what could not be read.
#[derive(Serialize)]
pub struct StartupReport {
    pub rows: Vec<StartupItem>,
    pub notes: Vec<String>,
}

/// What the read needs from the host: the account's home folder (macOS launch agents) and the
/// registry (Windows).
pub struct StartupSources<'a> {
    pub home: &'a Path,
    pub registry: &'a dyn RegistryRead,
}

/// What one read collects. It stops adding rows at the cap and records when time runs out.
struct Gather {
    rows: Vec<StartupItem>,
    notes: Vec<String>,
    #[cfg(any(target_os = "macos", windows))]
    deadline: Instant,
    late: bool,
    capped: bool,
}

impl Gather {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            notes: Vec::new(),
            #[cfg(any(target_os = "macos", windows))]
            deadline: Instant::now() + BUDGET,
            late: false,
            capped: false,
        }
    }

    /// Time left for the next source, or None once the budget is spent.
    #[cfg(any(target_os = "macos", windows))]
    fn time_left(&mut self) -> Option<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            self.late = true;
            None
        } else {
            Some(left)
        }
    }

    /// Adds a row. False once the cap is reached, so the caller stops reading.
    #[cfg(any(target_os = "macos", windows))]
    fn push(&mut self, row: StartupItem) -> bool {
        if self.rows.len() >= MAX_ROWS {
            self.capped = true;
            return false;
        }
        self.rows.push(row);
        true
    }

    fn note(&mut self, text: impl Into<String>) {
        self.notes.push(text.into());
    }

    fn finish(mut self) -> StartupReport {
        if self.late {
            self.notes.push(
                "The read stopped after 2 seconds, so some sources were not read.".to_string(),
            );
        }
        if self.capped {
            self.notes.push(format!(
                "More than {MAX_ROWS} items were found; the first {MAX_ROWS} are shown."
            ));
        }
        self.rows.sort_by(|a, b| {
            scope_rank(a.scope)
                .cmp(&scope_rank(b.scope))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        StartupReport {
            rows: self.rows,
            notes: self.notes,
        }
    }
}

fn scope_rank(scope: &str) -> u8 {
    match scope {
        "user" => 0,
        "all users" => 1,
        _ => 2,
    }
}

/// The startup and login items, read within the budget.
pub fn read_startup_items(sources: &StartupSources<'_>) -> StartupReport {
    let mut gather = Gather::new();
    platform::gather(&mut gather, sources);
    gather.finish()
}

#[cfg(target_os = "macos")]
mod platform {
    use std::fs;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use rightkit_child_process::{RunOptions, run};

    use super::{Gather, PER_FILE, StartupItem, StartupSources};

    const PLUTIL: &str = "/usr/bin/plutil";

    pub fn gather(g: &mut Gather, sources: &StartupSources<'_>) {
        // Directory, label shown in notes, scope and kind.
        let dirs: Vec<(PathBuf, &'static str, &'static str, &'static str)> = vec![
            (
                sources.home.join("Library/LaunchAgents"),
                "~/Library/LaunchAgents",
                "user",
                "launch agent",
            ),
            (
                PathBuf::from("/Library/LaunchAgents"),
                "/Library/LaunchAgents",
                "all users",
                "launch agent",
            ),
            (
                PathBuf::from("/Library/LaunchDaemons"),
                "/Library/LaunchDaemons",
                "system",
                "launch daemon",
            ),
        ];

        for (dir, label, scope, kind) in dirs {
            if g.time_left().is_none() {
                return;
            }
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(_) => {
                    g.note(format!("{label} could not be read."));
                    continue;
                }
            };
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|x| x.to_str()) == Some("plist"))
                .collect();
            files.sort();

            let mut unread = 0usize;
            let mut on_demand = 0usize;
            for file in files {
                let Some(left) = g.time_left() else { return };
                let value = match plist_json(&file, left.min(PER_FILE)) {
                    Ok(value) => value,
                    Err(_) => {
                        unread += 1;
                        continue;
                    }
                };
                if !starts_itself(&value) {
                    on_demand += 1;
                    continue;
                }
                let stem = file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                let row = StartupItem {
                    name: text_of(&value, "Label").unwrap_or(stem),
                    kind,
                    scope,
                    path: Some(file.display().to_string()),
                    program: program_of(&value),
                    enabled: enabled_of(&value),
                    vendor: None,
                };
                if !g.push(row) {
                    return;
                }
            }
            if unread > 0 {
                g.note(format!(
                    "{unread} plist file(s) in {label} could not be read."
                ));
            }
            if on_demand > 0 {
                g.note(format!(
                    "{on_demand} plist file(s) in {label} have neither RunAtLoad nor KeepAlive, so they start only when used and are not listed."
                ));
            }
        }

        g.note(
            "Enabled comes from each plist's Disabled key. launchd keeps its own on or off record, which needs an administrator to read, so a plist without that key shows Unknown.",
        );
        g.note("Login items set in System Settings are not listed here. Apple's own items in /System are not read.");
    }

    /// Runs `plutil` on one plist within a time limit and reads its JSON form.
    fn plist_json(path: &Path, limit: Duration) -> Result<serde_json::Value, String> {
        let output = run(
            PLUTIL,
            [
                std::ffi::OsStr::new("-convert"),
                std::ffi::OsStr::new("json"),
                std::ffi::OsStr::new("-o"),
                std::ffi::OsStr::new("-"),
                path.as_os_str(),
            ],
            &RunOptions::new(limit).stdout_cap(16 * 1024 * 1024),
        )
        .map_err(|e| e.to_string())?;
        if output.timed_out {
            return Err("plutil took too long".to_string());
        }
        if !output.status.success() || output.stdout.truncated {
            return Err("plutil could not read the file".to_string());
        }
        serde_json::from_slice(&output.stdout.bytes).map_err(|e| e.to_string())
    }

    /// launchd starts a job at load or keeps it alive; anything else runs only when used.
    fn starts_itself(value: &serde_json::Value) -> bool {
        let Some(dict) = value.as_object() else {
            return false;
        };
        dict.get("RunAtLoad").and_then(|v| v.as_bool()) == Some(true)
            || dict
                .get("KeepAlive")
                .is_some_and(|v| v.as_bool() != Some(false))
    }

    fn non_empty(text: &str) -> Option<String> {
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    fn text_of(value: &serde_json::Value, key: &str) -> Option<String> {
        value.get(key)?.as_str().and_then(non_empty)
    }

    /// The Program key, or the first of ProgramArguments.
    fn program_of(value: &serde_json::Value) -> Option<String> {
        text_of(value, "Program").or_else(|| {
            let first = value.get("ProgramArguments")?.as_array()?.first()?;
            non_empty(first.as_str()?)
        })
    }

    /// Disabled in the plist when present; absent means unknown.
    fn enabled_of(value: &serde_json::Value) -> Option<bool> {
        value.get("Disabled")?.as_bool().map(|disabled| !disabled)
    }
}

#[cfg(windows)]
mod platform {
    use std::collections::HashMap;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};

    use crate::apps::registry::{Hive, RegistryRead, ValueKind, View};

    use super::{Gather, StartupItem, StartupSources};

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_WOW: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
    const APPROVED_RUN: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    const APPROVED_RUN_WOW: &str =
        r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    const APPROVED_FOLDER: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder";
    const STARTUP: &str = r"Microsoft\Windows\Start Menu\Programs\Startup";

    pub fn gather(g: &mut Gather, sources: &StartupSources<'_>) {
        let registry = sources.registry;
        let runs: [(Hive, &str, &str, &'static str); 3] = [
            (Hive::CurrentUser, RUN, APPROVED_RUN, "user"),
            (Hive::LocalMachine, RUN, APPROVED_RUN, "all users"),
            (Hive::LocalMachine, RUN_WOW, APPROVED_RUN_WOW, "all users"),
        ];
        for (hive, run, approved, scope) in runs {
            if g.time_left().is_none() {
                return;
            }
            read_run(g, registry, hive, run, approved, scope);
        }

        let mut folders: Vec<(PathBuf, &'static str, Hive)> = Vec::new();
        if let Some(appdata) = std::env::var_os("APPDATA") {
            folders.push((
                PathBuf::from(appdata).join(STARTUP),
                "user",
                Hive::CurrentUser,
            ));
        }
        if let Some(common) = std::env::var_os("ProgramData") {
            folders.push((
                PathBuf::from(common).join(STARTUP),
                "all users",
                Hive::LocalMachine,
            ));
        }
        for (dir, scope, hive) in folders {
            if g.time_left().is_none() {
                return;
            }
            read_folder(g, registry, &dir, scope, hive);
        }

        g.note("Startup folder shortcuts are listed by file name; their targets are not resolved.");
        g.note("Scheduled tasks and services are not listed.");
    }

    /// One Run key, with its StartupApproved state. A key that is absent is skipped.
    fn read_run(
        g: &mut Gather,
        registry: &dyn RegistryRead,
        hive: Hive,
        run: &str,
        approved: &str,
        scope: &'static str,
    ) {
        let Some(entries) = registry.values(hive, View::Default, run) else {
            return;
        };
        let states = approvals(registry, hive, approved);
        let mut unread = 0usize;
        for entry in entries {
            let value = match entry {
                Ok(value) => value,
                Err(_) => {
                    unread += 1;
                    continue;
                }
            };
            if !matches!(value.kind, ValueKind::String | ValueKind::ExpandString) {
                continue;
            }
            let command = value.text().unwrap_or_default().trim().to_string();
            let row = StartupItem {
                kind: "run key",
                scope,
                path: executable(&command),
                program: (!command.is_empty()).then(|| command.clone()),
                enabled: states.get(&value.name.to_lowercase()).copied(),
                name: value.name,
                vendor: None,
            };
            if !g.push(row) {
                return;
            }
        }
        if unread > 0 {
            g.note(format!("{unread} entries in a Run key could not be read."));
        }
    }

    /// The StartupApproved state by value name: first byte 02 is on, 03 is off.
    fn approvals(registry: &dyn RegistryRead, hive: Hive, path: &str) -> HashMap<String, bool> {
        let mut states = HashMap::new();
        let Some(values) = registry.values(hive, View::Default, path) else {
            return states;
        };
        for value in values.into_iter().flatten() {
            match value.bytes.first().copied() {
                Some(2) => {
                    states.insert(value.name.to_lowercase(), true);
                }
                Some(3) => {
                    states.insert(value.name.to_lowercase(), false);
                }
                _ => {}
            }
        }
        states
    }

    /// A Startup folder: its shortcuts by file name. The folder may be absent.
    fn read_folder(
        g: &mut Gather,
        registry: &dyn RegistryRead,
        dir: &Path,
        scope: &'static str,
        hive: Hive,
    ) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return,
            Err(_) => {
                g.note(format!("The {scope} Startup folder could not be read."));
                return;
            }
        };
        let states = approvals(registry, hive, APPROVED_FOLDER);
        let mut shortcuts: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x.eq_ignore_ascii_case("lnk"))
            })
            .collect();
        shortcuts.sort();
        for path in shortcuts {
            let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let row = StartupItem {
                name: path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or(file)
                    .to_string(),
                kind: "startup folder",
                scope,
                path: Some(path.display().to_string()),
                program: None,
                enabled: states.get(&file.to_lowercase()).copied(),
                vendor: None,
            };
            if !g.push(row) {
                return;
            }
        }
    }

    /// The program a Run command line starts, when that is a file that exists.
    fn executable(command: &str) -> Option<String> {
        let trimmed = command.trim_start();
        let exe = match trimmed.strip_prefix('"') {
            Some(rest) => rest.split('"').next().unwrap_or(""),
            None => trimmed.split_whitespace().next().unwrap_or(""),
        };
        (!exe.is_empty() && Path::new(exe).is_file()).then(|| exe.to_string())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::{Gather, StartupSources};

    pub fn gather(g: &mut Gather, sources: &StartupSources<'_>) {
        let _ = (sources.home, sources.registry);
        g.note("Startup items are read on macOS and Windows only.");
    }
}
