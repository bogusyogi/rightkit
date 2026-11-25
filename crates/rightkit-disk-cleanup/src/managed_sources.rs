//! Cleanup items the rule pack cannot describe, found by the host app itself (moved out of Pulse's
//! `cleanup_sources.rs`): RustRight
//! build-control targets, agent scratch folders, Tart virtual machines and RightKit
//! release builds.
//!
//! A RustRight target is live or stale by the rules RustRight's own GC applies to its
//! state file (`ws/<key>/state.json`): a generation a build holds or pins is never
//! offered; the current generation is offered only after the stale-idle window; a
//! superseded generation is offered after the GC grace. Judging runs again at apply
//! time, and each moved item goes through core's identity, in-use, held-open, journal
//! and Undo checks under a literal rule built for its path.

use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cleanup_scan::{self as cs, CleanupRule, Finding, LivenessCheck, Measurement, RuleRisk};
use serde_json::Value;

/// Rule-id prefix of host-sourced items: `src:<path>`.
pub const PREFIX: &str = "src:";
/// Categories the "developer items" setting controls.
const DEVELOPER_CATEGORIES: &[&str] = &[
    "Developer",
    "Build outputs",
    "Build targets",
    "Agent scratch",
    "Virtual machines",
    "Release builds",
];
const DAY: u64 = 86_400;
/// RustRight's stale-idle rule (`rustright prune`, DEFAULT_IDLE_DAYS).
const IDLE_STALE_SECS: u64 = 14 * DAY;
/// RustRight's GC grace after a change.
const GRACE_SECS: u64 = 30 * 60;
const SCRATCH_QUIET_SECS: u64 = 7 * DAY;
/// A `.keep` marker in a folder keeps it this long after the marker was last written.
const KEEP_MARKER_SECS: u64 = 14 * DAY;
/// Most entries walked to prove a folder quiet; a bigger one is unknown, never quiet.
const QUIET_WALK_CAP: usize = 1_000_000;
/// RustRight's `state.json` schema this judge understands (`SCHEMA_VERSION` in rustright-state).
const STATE_SCHEMA: u64 = 1;
/// Items that are not offered are still listed when at least this big.
const SHOW_BYTES: u64 = 1 << 30;
/// Newest release builds always kept.
const RELEASES_KEPT: usize = 2;

/// One judged item. Judging reads only small files and metadata, so apply can repeat it.
struct Judged {
    path: PathBuf,
    name: String,
    category: &'static str,
    kind: &'static str,
    safety: &'static str,
    owner: String,
    risk: RuleRisk,
    eligible: bool,
    reason: String,
    last_used: u64,
    /// Bytes RustRight recorded for this target; live targets are never walked.
    measured: Option<u64>,
    /// Walk the item for its size.
    walk: bool,
}

/// What only the host knows: its name (shown in two reasons), a managed root it was told to use
/// instead of discovering one (a test build's), and what to do when RustRight's state file has a
/// shape this judge does not understand (the host logs it).
pub struct SourceConfig<'a> {
    pub app_name: &'a str,
    pub managed_root_override: Option<PathBuf>,
    pub on_state_unrecognised: &'a dyn Fn(),
}

const TART_HOME_ENV: &str = "TART_HOME";

fn env_absolute_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// RightKit's build-control config files, most specific first. Its `broker.externalRoot.path`
/// is the managed root.
fn build_control_configs() -> Vec<PathBuf> {
    let bases: Vec<PathBuf> = if cfg!(windows) {
        ["LOCALAPPDATA", "APPDATA"]
            .iter()
            .filter_map(|key| env_absolute_path(key))
            .collect()
    } else {
        env_absolute_path("HOME")
            .map(|home| home.join("Library").join("Application Support"))
            .into_iter()
            .collect()
    };
    bases
        .into_iter()
        .map(|base| {
            base.join("RightKit")
                .join("build-control")
                .join("config.json")
        })
        .collect()
}

/// The RustRight/RightKit managed root (`.rightkit-managed`): the host's override, else
/// `broker.externalRoot.path` in the build-control config. `None` when nothing is discovered,
/// and then no developer item is offered. The broker socket is not used: it may live in the
/// internal control folder rather than the managed root.
fn managed_root(config: &SourceConfig<'_>) -> Option<PathBuf> {
    if let Some(root) = &config.managed_root_override {
        return Some(root.clone());
    }
    build_control_configs().iter().find_map(|path| {
        read_json(path)?["broker"]["externalRoot"]["path"]
            .as_str()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    })
}

fn build_root(managed: &Path) -> PathBuf {
    managed.join("rightkit-build-control").join("ws")
}

/// The agent scratch folder: `scratch` beside the managed root on its volume.
fn scratch_root(managed: &Path) -> Option<PathBuf> {
    managed.parent().map(|volume| volume.join("scratch"))
}

/// Tart's VM folder: `TART_HOME/vms` when set, else `tart-home/vms` beside the managed root.
fn tart_root(managed: Option<&Path>) -> Option<PathBuf> {
    match std::env::var(TART_HOME_ENV) {
        Ok(home) if !home.is_empty() => Some(PathBuf::from(home).join("vms")),
        _ => managed?
            .parent()
            .map(|volume| volume.join("tart-home").join("vms")),
    }
}

/// RightKit's release folder: `rightkit-publish` beside the managed root on its volume.
fn publish_root(managed: &Path) -> Option<PathBuf> {
    managed
        .parent()
        .map(|volume| volume.join("rightkit-publish"))
}

fn platform() -> &'static str {
    if cfg!(windows) { "windows" } else { "macos" }
}

pub fn is_developer(category: &str) -> bool {
    DEVELOPER_CATEGORIES.contains(&category)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn secs(time: std::io::Result<SystemTime>) -> u64 {
    time.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

#[cfg(unix)]
fn identity(md: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (md.dev(), md.ino())
}

#[cfg(not(unix))]
fn identity(_md: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Unix seconds of an ISO-8601 UTC time such as `2026-10-10T21:00:04.914Z`.
fn iso_secs(text: &str) -> Option<u64> {
    let instant = rightkit_time::DateTime::parse_rfc3339(text)?;
    u64::try_from(instant.unix_seconds()).ok()
}

fn last_path_part(text: &str) -> &str {
    text.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(text)
}

/// A generation directory's number: RustRight names them `002`, `003`, ...
fn generation_number(name: &std::ffi::OsStr) -> Option<u64> {
    let text = name.to_str()?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Newest write anywhere inside `path`, in Unix seconds, or `None` when that cannot be
/// proven (a deep write is invisible to the folder's own time).
fn newest_inside(path: &Path, stop_after: Option<u64>) -> Option<u64> {
    cs::newest_modified(path, QUIET_WALK_CAP, stop_after)
}

/// Whether RustRight's state file has the schema and field shapes this judge reads.
/// Anything else fails closed: the workspace offers nothing.
fn state_understood(state: &Value) -> bool {
    let generation = |item: &Value| item["generation"].as_u64().is_some();
    let list_ok = |field: &str| match &state[field] {
        Value::Null => true,
        Value::Array(items) => items.iter().all(generation),
        _ => false,
    };
    state["schemaVersion"].as_u64() == Some(STATE_SCHEMA)
        && state["currentGeneration"].as_u64().is_some()
        && (state["active"].is_null() || generation(&state["active"]))
        && list_ok("residents")
        && list_ok("pins")
}

fn generations_of(state: &Value, field: &str) -> Vec<u64> {
    state[field]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["generation"].as_u64())
                .collect()
        })
        .unwrap_or_default()
}

/// RustRight build-control targets, one per workspace generation.
fn judge_builds(
    now: u64,
    managed: Option<&Path>,
    config: &SourceConfig<'_>,
    out: &mut Vec<Judged>,
) {
    let Some(managed) = managed else {
        return;
    };
    let Ok(workspaces) = fs::read_dir(build_root(managed)) else {
        return;
    };
    for workspace in workspaces.flatten() {
        let ws_dir = workspace.path();
        let Some(state) = read_json(&ws_dir.join("state.json")) else {
            continue;
        };
        let key = workspace.file_name().to_string_lossy().into_owned();
        if !state_understood(&state) {
            (config.on_state_unrecognised)();
            continue;
        }
        let owner = read_json(&ws_dir.join("identity.json"))
            .and_then(|identity| {
                identity["canonicalRoot"]
                    .as_str()
                    .map(|root| last_path_part(root).to_string())
            })
            .unwrap_or_else(|| key.clone());
        let current = state["currentGeneration"].as_u64();
        let active = state["active"]["generation"].as_u64();
        let residents = generations_of(&state, "residents");
        let pins = generations_of(&state, "pins");
        let last_used = state["lastUsedAt"].as_str().and_then(iso_secs).unwrap_or(0);
        let measured_generation = state["lastMeasuredGeneration"].as_u64();
        let measured_bytes = state["lastMeasuredBytes"].as_u64();
        let Ok(generations) = fs::read_dir(ws_dir.join("g")) else {
            continue;
        };
        for entry in generations.flatten() {
            let Some(number) = generation_number(&entry.file_name()) else {
                continue;
            };
            let path = entry.path();
            let Ok(md) = path.symlink_metadata() else {
                continue;
            };
            if !md.is_dir() || md.file_type().is_symlink() {
                continue;
            }
            // Superseded generations: the newest write anywhere inside, not the top
            // folder's time. Unknown counts as just changed.
            let mtime = newest_inside(&path, Some(now.saturating_sub(GRACE_SECS))).unwrap_or(now);
            let is_current = current == Some(number);
            let used = if is_current { last_used } else { mtime };
            let measured = (measured_generation == Some(number))
                .then_some(measured_bytes)
                .flatten();
            let mut item = Judged {
                name: format!("{owner} · build {}", entry.file_name().to_string_lossy()),
                path,
                category: "Build targets",
                kind: "RustRight build target",
                safety: "rebuildable cache",
                owner: owner.clone(),
                risk: RuleRisk::Review,
                eligible: false,
                reason: String::new(),
                last_used: used,
                measured,
                walk: false,
            };
            if active == Some(number) || residents.contains(&number) {
                item.safety = "live build target, never removed";
                item.reason = "In use: RustRight is building in this target.".into();
            } else if pins.contains(&number) {
                item.safety = "live build target, never removed";
                item.reason = "Pinned: a running tool still uses this target.".into();
            } else if is_current {
                if last_used == 0 {
                    item.reason = "Last use unknown; kept.".into();
                } else if now.saturating_sub(last_used) >= IDLE_STALE_SECS {
                    item.eligible = true;
                    item.walk = true;
                    item.reason = format!(
                        "Not used for {} days. The next build there starts cold.",
                        now.saturating_sub(last_used) / DAY
                    );
                } else {
                    item.reason = "Used recently; kept.".into();
                }
            } else if now.saturating_sub(mtime) < GRACE_SECS {
                item.reason = "Changed in the last 30 minutes; kept.".into();
            } else {
                item.eligible = true;
                item.walk = true;
                item.reason = "Superseded by a newer target of this workspace.".into();
            }
            out.push(item);
        }
    }
}

/// Top-level folders in the agent scratch area.
fn judge_scratch(now: u64, managed: Option<&Path>, out: &mut Vec<Judged>) {
    let Some(scratch) = managed.and_then(scratch_root) else {
        return;
    };
    let Ok(entries) = fs::read_dir(scratch) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(md) = path.symlink_metadata() else {
            continue;
        };
        if md.file_type().is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let top = secs(md.modified());
        // The newest write anywhere inside; unknown (unreadable, too big) is not quiet.
        let deep = if md.is_dir() {
            newest_inside(&path, Some(now.saturating_sub(SCRATCH_QUIET_SECS)))
        } else {
            Some(top)
        };
        let mtime = deep.unwrap_or(now).max(top);
        let mut quiet = deep.is_some() && now.saturating_sub(mtime) >= SCRATCH_QUIET_SECS;
        let mut kept_by_marker = false;
        if md.is_dir() {
            match path.join(".keep").symlink_metadata() {
                Ok(marker) => {
                    // A marker keeps the folder 14 days from its last write; an
                    // unreadable marker time keeps it too.
                    let written = marker.modified().ok().map(|t| secs(Ok(t)));
                    if written.is_none_or(|w| now.saturating_sub(w) < KEEP_MARKER_SECS) {
                        quiet = false;
                        kept_by_marker = true;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    quiet = false;
                    kept_by_marker = true;
                }
            }
        }
        let owner = name.split('-').next().unwrap_or(name.as_str()).to_string();
        out.push(Judged {
            name: name.clone(),
            path,
            category: "Agent scratch",
            kind: "Agent scratch",
            safety: "experiment output",
            owner,
            risk: RuleRisk::Review,
            eligible: quiet,
            reason: if kept_by_marker {
                "Marked to keep (.keep); kept.".into()
            } else if quiet {
                format!(
                    "Nothing inside written for {} days.",
                    now.saturating_sub(mtime) / DAY
                )
            } else if deep.is_none() {
                "Can't tell whether anything inside is still written; kept.".into()
            } else {
                "Written in the last 7 days; kept.".into()
            },
            last_used: mtime,
            measured: None,
            walk: true,
        });
    }
}

/// Tart virtual machines are listed for their size, never moved: a VM is removed with `tart delete`.
fn judge_vms(managed: Option<&Path>, config: &SourceConfig<'_>, out: &mut Vec<Judged>) {
    let Some(tart) = tart_root(managed) else {
        return;
    };
    let Ok(entries) = fs::read_dir(tart) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(md) = path.symlink_metadata() else {
            continue;
        };
        if !md.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        out.push(Judged {
            name: name.clone(),
            path,
            category: "Virtual machines",
            kind: "Tart virtual machine",
            safety: "virtual machine",
            owner: "Tart".into(),
            risk: RuleRisk::Review,
            eligible: false,
            reason: format!(
                "Virtual machine: remove it with tart delete, not from {}.",
                config.app_name
            ),
            last_used: secs(md.modified()),
            measured: None,
            walk: true,
        });
    }
}

/// RightKit release builds (`v<N>` folders and `archive-*`). The newest releases are kept.
fn judge_releases(now: u64, managed: Option<&Path>, out: &mut Vec<Judged>) {
    let Some(publish) = managed.and_then(publish_root) else {
        return;
    };
    let Ok(entries) = fs::read_dir(publish) else {
        return;
    };
    let mut numbered: Vec<(u64, Judged)> = Vec::new();
    let mut archives: Vec<Judged> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(md) = path.symlink_metadata() else {
            continue;
        };
        if !md.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let number = name.strip_prefix('v').and_then(|n| n.parse::<u64>().ok());
        let is_archive = name.starts_with("archive-");
        if number.is_none() && !is_archive {
            continue;
        }
        // An old build is offered only when nothing inside was written in the grace
        // window (a publish in progress) and that can be proven.
        let settled = newest_inside(&path, Some(now.saturating_sub(GRACE_SECS)))
            .is_some_and(|newest| now.saturating_sub(newest) >= GRACE_SECS);
        let item = |eligible: bool, reason: &str| Judged {
            name: name.clone(),
            path: path.clone(),
            category: "Release builds",
            kind: "Release build",
            safety: "old release build",
            owner: "RightKit release".into(),
            risk: RuleRisk::Review,
            eligible: eligible && settled,
            reason: if eligible && !settled {
                "Written in the last 30 minutes, or can't tell; kept.".into()
            } else {
                reason.into()
            },
            last_used: secs(md.modified()),
            measured: None,
            walk: true,
        };
        match number {
            Some(n) => {
                numbered.push((n, item(true, "Old release build. The newest two are kept.")))
            }
            None => archives.push(item(true, "Archived release build.")),
        }
    }
    numbered.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (index, (_, mut item)) in numbered.into_iter().enumerate() {
        if index < RELEASES_KEPT {
            item.eligible = false;
            item.reason = "Newest release, kept.".into();
        }
        out.push(item);
    }
    out.extend(archives);
}

fn judge_all(now: u64, config: &SourceConfig<'_>) -> Vec<Judged> {
    let mut out = Vec::new();
    let managed = managed_root(config);
    judge_builds(now, managed.as_deref(), config, &mut out);
    judge_scratch(now, managed.as_deref(), &mut out);
    judge_vms(managed.as_deref(), config, &mut out);
    judge_releases(now, managed.as_deref(), &mut out);
    out
}

fn finding_id(rule_id: &str, path: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    rule_id.hash(&mut hasher);
    path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn finding(item: Judged, bytes: u64) -> Finding {
    let path = item.path.to_string_lossy().into_owned();
    let (dev, ino) = item
        .path
        .symlink_metadata()
        .map(|md| identity(&md))
        .unwrap_or((0, 0));
    let rule_id = format!("{PREFIX}{path}");
    Finding {
        id: finding_id(&rule_id, &path),
        rule_id,
        rule_name: item.kind.into(),
        category: item.category.into(),
        name: item.name,
        path,
        bytes,
        apparent_bytes: bytes,
        partial: false,
        upper_bound: true,
        risk: item.risk,
        eligible: item.eligible,
        preselected: false,
        reason: item.reason,
        action: "Move to Trash".into(),
        dev: dev.to_string(),
        ino: ino.to_string(),
        kind: item.kind.into(),
        safety: item.safety.into(),
        owner: item.owner,
        last_used: item.last_used,
    }
}

/// The host's own items for the cleanup report. Items too small to matter and not
/// offered are left out; a live target is listed only when it is big.
pub fn scan(config: &SourceConfig<'_>) -> Vec<Finding> {
    let now = now_secs();
    let mut findings = Vec::new();
    for item in judge_all(now, config) {
        let bytes = if item.walk {
            cs::size_of(&item.path)
        } else {
            item.measured.unwrap_or(0)
        };
        if bytes == 0 || (!item.eligible && bytes < SHOW_BYTES) {
            continue;
        }
        findings.push(finding(item, bytes));
    }
    findings
}

/// The rule that lets core move one host-sourced path, built only for that path.
fn rule_for(item: &Judged) -> CleanupRule {
    let path = item.path.to_string_lossy().into_owned();
    CleanupRule {
        id: format!("{PREFIX}{path}"),
        name: item.kind.into(),
        category: item.category.into(),
        risk: item.risk,
        paths: vec![path],
        exclude_names: Vec::new(),
        liveness: LivenessCheck::Age,
        owners: Vec::new(),
        min_age_days: None,
        quiet_days: None,
        keep_newest: 0,
        reason: item.reason.clone(),
        action: "Move to Trash".into(),
        measurement: Measurement::Allocated,
        discover: None,
        platforms: vec![platform().into()],
        label_parent: false,
    }
}

/// Judges each host-sourced request again, now. Returns the requests to forward,
/// the literal rules core needs for them, and the requests refused here.
pub fn prepare(
    requests: Vec<cs::Request>,
    config: &SourceConfig<'_>,
) -> (Vec<cs::Request>, Vec<CleanupRule>, Vec<cs::Skipped>) {
    let mut judged: Option<Vec<Judged>> = None;
    let mut kept = Vec::new();
    let mut rules = Vec::new();
    let mut refused = Vec::new();
    for request in requests {
        if !request.rule_id.starts_with(PREFIX) {
            kept.push(request);
            continue;
        }
        let items = judged.get_or_insert_with(|| judge_all(now_secs(), config));
        let verdict = items
            .iter()
            .find(|item| item.path == Path::new(&request.path))
            .map(|item| {
                if item.eligible {
                    Ok(rule_for(item))
                } else {
                    Err(item.reason.clone())
                }
            })
            .unwrap_or_else(|| {
                Err(format!(
                    "Not a location {} offers for cleanup",
                    config.app_name
                ))
            });
        match verdict {
            Ok(rule) if rule.id == request.rule_id => {
                rules.push(rule);
                kept.push(request);
            }
            Ok(_) => refused.push(cs::Skipped {
                path: request.path,
                reason: "Changed since the scan".into(),
            }),
            Err(reason) => refused.push(cs::Skipped {
                path: request.path,
                reason,
            }),
        }
    }
    (kept, rules, refused)
}
