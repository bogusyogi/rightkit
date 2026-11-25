//! Uninstalling an app and ending its processes on Windows.
//!
//! Uninstall: starts the app's own registered uninstaller, which asks for its own confirmation
//! and elevation. Selected leftover folders go to the Recycle Bin only after the uninstaller's
//! process tree has finished and the app's registration is gone, and only while each folder is
//! still the one found (volume, file id and ancestors unchanged, see `super::leftovers`). The
//! registry is never modified.
//!
//! Processes: grouped by executable name; Quit sends the graceful close request (`taskkill`),
//! Force Quit ends the process tree one verified process at a time.
//!
//! The host supplies what only it knows: the inventory read, the registry view, the way a folder
//! reaches the Recycle Bin and the process list (`UninstallHost`, `ProcessPolicy`).

use std::collections::{HashMap, HashSet};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rightkit_system_monitor::process_details_windows::ProcessDetail;
use rightkit_system_monitor::{ProcessDiskIo, ProcessIdentity, ProcessInfo};
use serde::Serialize;

use super::leftovers::{self, Pinned, ScanRegistry};
use super::{AppEntry, Installed};

/// The macOS page's detail shape. Windows lists no related items in the detail itself (the
/// leftovers stream separately).
#[derive(Serialize)]
pub struct AppDetail {
    pub app: AppEntry,
    pub items: Vec<serde_json::Value>,
    pub background: Vec<serde_json::Value>,
    pub receipts: Vec<String>,
}

impl AppDetail {
    /// The detail of `app` with no related items.
    pub fn of(app: AppEntry) -> Self {
        AppDetail {
            app,
            items: Vec::new(),
            background: Vec::new(),
            receipts: Vec::new(),
        }
    }
}

#[derive(Serialize)]
pub struct MovedLeftover {
    pub path: String,
    pub bytes: u64,
}

#[derive(Serialize)]
pub struct FailedLeftover {
    pub path: String,
    pub error: String,
}

#[derive(Serialize)]
pub struct UninstallResult {
    pub moved: Vec<MovedLeftover>,
    pub failed: Vec<FailedLeftover>,
    pub moved_bytes: u64,
    /// Same value as `moved_bytes`: leftovers sent to the Recycle Bin. That frees nothing
    /// until the Recycle Bin is emptied, so it is not an observed gain.
    pub bytes_moved: u64,
    pub activity_id: Option<String>,
}

/// How long to wait for an uninstaller to remove the app's registration before leftover folders
/// are left alone.
const UNINSTALL_WAIT: Duration = Duration::from_secs(300);

/// What an uninstall needs from the host app.
pub struct UninstallHost<'a> {
    /// A fresh read of the installed apps.
    pub inventory: &'a dyn Fn() -> Vec<Installed>,
    /// The registry view and wording for the leftovers scan.
    pub registry: ScanRegistry<'a>,
    /// Sends a folder to the Recycle Bin.
    pub move_to_trash: &'a dyn Fn(&Path) -> Result<(), String>,
    /// The live process list.
    pub processes: &'a dyn Fn() -> Vec<ProcessInfo>,
}

fn find(host: &UninstallHost<'_>, path: &str) -> Result<Installed, String> {
    (host.inventory)()
        .into_iter()
        .find(|app| app.entry.path.eq_ignore_ascii_case(path))
        .ok_or_else(|| "That app is no longer installed.".to_string())
}

/// Starts the app's own uninstaller (from its Uninstall key). The uninstaller shows its own
/// prompts and asks for administrator rights itself.
///
/// With no leftover selected this returns at once. With leftover folders selected, the folders
/// are first checked against the leftovers found for this app (anything else is refused) and
/// pinned by volume and file identity, the uninstaller is given up to five minutes to finish (its
/// process tree has exited with success and the registration is gone), and only then is each
/// folder checked again against its pinned identity and ancestors and moved to the Recycle Bin.
/// If the uninstaller fails, is cancelled or is still running, or a folder cannot be proven the
/// same one, it is not touched. The registry is never modified.
pub fn uninstall(
    host: &UninstallHost<'_>,
    path: &str,
    items: Vec<String>,
) -> Result<UninstallResult, String> {
    let installed = find(host, path)?;
    if let Some(reason) = installed.entry.protected.clone() {
        return Err(reason);
    }
    let command = installed
        .uninstall
        .clone()
        .ok_or_else(|| "This app registered no uninstaller.".to_string())?;
    // The app's own row stands for the uninstaller itself; only other paths are leftovers.
    let wanted: Vec<String> = items
        .into_iter()
        .filter(|item| !item.eq_ignore_ascii_case(path))
        .collect();
    // What counts as this app's leftover, and each folder's identity, is decided before the
    // uninstaller runs and removes its registration.
    let allowed: Vec<Pinned> = if wanted.is_empty() {
        Vec::new()
    } else {
        leftovers::scan(&installed, &(host.inventory)(), &host.registry, &|_| {})
    };
    // The registered string is a full command line; cmd runs it as the app's installer wrote it.
    // Detached: the uninstaller is the app's own program and must outlive this call (an early
    // return below leaves it running); dropping an owned child would end its tree.
    let mut uninstaller = rightkit_child_process::OwnedCommand::new("cmd.exe");
    uninstaller
        .command_mut()
        .raw_arg(format!("/S /C \"{command}\""));
    let mut child = uninstaller
        .spawn_uncontained_detached()
        .map_err(|e| format!("Could not start the uninstaller: {e}"))?;
    let mut result = UninstallResult {
        moved: Vec::new(),
        failed: Vec::new(),
        moved_bytes: 0,
        bytes_moved: 0,
        activity_id: None,
    };
    if wanted.is_empty() {
        return Ok(result);
    }
    let refuse_all = |result: &mut UninstallResult, why: &str| {
        for item in &wanted {
            result.failed.push(FailedLeftover {
                path: item.clone(),
                error: why.to_string(),
            });
        }
    };
    if let Err(why) = await_uninstall(host, &mut child, path) {
        refuse_all(&mut result, &why);
        return Ok(result);
    }
    // Fresh inventory: another app may have taken one of these folders since the scan.
    let current = (host.inventory)();
    for item in wanted {
        let Some(pinned) = allowed
            .iter()
            .find(|pinned| pinned.path().eq_ignore_ascii_case(&item))
        else {
            result.failed.push(FailedLeftover {
                path: item,
                error: "Not a leftover of this app any more.".into(),
            });
            continue;
        };
        if let Err(error) = pinned.still_same() {
            result.failed.push(FailedLeftover { path: item, error });
            continue;
        }
        if pinned.collides_with(&installed, &current) {
            result.failed.push(FailedLeftover {
                path: item,
                error: "Another installed app uses this folder now; left alone.".into(),
            });
            continue;
        }
        let target = PathBuf::from(&item);
        let bytes = leftovers::size_now(&target);
        // Checked again as close to the move as the Recycle Bin call allows.
        if let Err(error) = pinned.still_same() {
            result.failed.push(FailedLeftover { path: item, error });
            continue;
        }
        match (host.move_to_trash)(&target) {
            Ok(()) => {
                result.moved_bytes += bytes;
                result.bytes_moved = result.moved_bytes;
                result.moved.push(MovedLeftover { path: item, bytes });
            }
            Err(error) => result.failed.push(FailedLeftover { path: item, error }),
        }
    }
    Ok(result)
}

/// Waits until the uninstaller has finished, or says why it cannot be shown to have.
///
/// Finished means all of: the process that was started exited with success; every process that
/// descends from it (found by parent id and start time, so a recycled id is not mistaken for one)
/// has exited, including ones that outlive their parent, such as a self-copied stub or an
/// elevated child; and the app's registration is gone. The three must hold on two reads about a
/// second and a half apart. A vanished registration alone proves nothing, since an uninstaller
/// removes it before it has finished deleting files. Bounded by `UNINSTALL_WAIT`; any uncertainty
/// is a refusal.
fn await_uninstall(
    host: &UninstallHost<'_>,
    child: &mut rightkit_child_process::OwnedChild,
    path: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + UNINSTALL_WAIT;
    let root = child.id();
    // Every (pid, start time) seen in the uninstaller's tree so far, with its start time as read.
    let mut tree: HashMap<u32, u64> = HashMap::new();
    let mut exit_ok = false;
    let mut quiet_reads = 0u32;
    let mut still_registered = 0u32;
    let mut polls = 0u32;
    loop {
        if !exit_ok {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => exit_ok = true,
                Ok(Some(_)) => {
                    return Err(
                        "The uninstaller did not finish; leftovers were not touched.".into(),
                    );
                }
                Ok(None) => {}
                Err(_) => {
                    return Err(
                        "The uninstaller's state could not be read; leftovers were not touched."
                            .into(),
                    );
                }
            }
        }
        let alive = (host.processes)();
        let live: HashMap<u32, u64> = alive
            .iter()
            .map(|p| (p.identity.pid, p.identity.start_time))
            .collect();
        // A process joins the tree when its parent is in the tree and it started no earlier
        // than that parent; repeat until nothing new joins.
        if exit_ok && tree.is_empty() {
            // Exited before it was ever read: nothing of it is left to see; its children are
            // still found below by parent id and start time.
            tree.insert(root, 0);
        } else if let Some(start) = live.get(&root) {
            tree.entry(root).or_insert(*start);
        }
        loop {
            let mut grew = false;
            for process in &alive {
                let pid = process.identity.pid;
                if tree.get(&pid) == Some(&process.identity.start_time) {
                    continue;
                }
                let Some(parent) = process.parent_pid else {
                    continue;
                };
                if let Some(parent_start) = tree.get(&parent)
                    && process.identity.start_time >= *parent_start
                    && pid != root
                {
                    tree.insert(pid, process.identity.start_time);
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        let running = tree.iter().any(|(pid, start)| live.get(pid) == Some(start));
        let registered = find(host, path).is_ok();
        if exit_ok && !running && !registered {
            quiet_reads += 1;
            if quiet_reads >= 2 {
                return Ok(());
            }
        } else {
            quiet_reads = 0;
        }
        if exit_ok && !running && registered {
            // Exited cleanly, nothing left running, still registered: cancelled, or finished
            // without removing itself. A few seconds' grace covers a late registry write.
            still_registered += 1;
            if still_registered >= 10 {
                return Err(
                    "The app is still registered after its uninstaller ended; leftovers were not touched."
                        .into(),
                );
            }
        } else {
            still_registered = 0;
        }
        if Instant::now() >= deadline {
            return Err("The uninstaller is still running; leftovers were not touched.".into());
        }
        polls = polls.saturating_add(1);
        std::thread::sleep(Duration::from_millis(if polls < 20 { 300 } else { 1000 }));
    }
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
pub struct ProcessMember {
    pub identity: ProcessIdentity,
    pub name: String,
    pub cpu_usage_percent: f32,
    pub memory_bytes: u64,
    pub threads: Option<u32>,
    pub gpu_percent: Option<f32>,
    pub started_at: Option<u64>,
    /// Pipe, network and file transfer counts (#324 item 10); omitted when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_io: Option<ProcessDiskIo>,
}

#[derive(Clone, Serialize)]
pub struct ProcessRow {
    /// `name:<lower-case executable name>` for a group of processes.
    pub key: String,
    pub name: String,
    pub bundle_id: Option<String>,
    pub app_path: Option<String>,
    /// The main process of the group; identity for Quit.
    pub lead: ProcessIdentity,
    pub cpu_usage_percent: f32,
    pub memory_bytes: u64,
    pub threads: Option<u32>,
    pub gpu_percent: Option<f32>,
    pub started_at: Option<u64>,
    pub executable: Option<String>,
    /// The members' disk counters added up (#324 item 10); omitted when no member has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_io: Option<ProcessDiskIo>,
    pub members: Vec<ProcessMember>,
    pub can_act: bool,
    pub refusal: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuitOutcome {
    Quit,
    StillRunning,
}

/// The host app, which Quit must never be offered for, and the name its refusal gives it.
pub struct ProcessPolicy<'a> {
    /// A process whose lower-cased name starts with this is the host's (`pulse`).
    pub host_name_prefix: &'a str,
    /// The host's name in the refusal ("Part of Pulse.").
    pub host_display_name: &'a str,
}

/// Windows processes that Quit must never be offered for: the kernel and session managers, the
/// shell and its host processes, and the security stack.
const SYSTEM_NAMES: &[&str] = &[
    "system",
    "system idle process",
    "registry",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "services.exe",
    "lsass.exe",
    "winlogon.exe",
    "svchost.exe",
    "dwm.exe",
    "fontdrvhost.exe",
    "memory compression",
    "explorer.exe",
    "sihost.exe",
    "ctfmon.exe",
    "taskhostw.exe",
    "audiodg.exe",
    "spoolsv.exe",
    "msmpeng.exe",
    "securityhealthservice.exe",
    "wudfhost.exe",
    "lsaiso.exe",
    "searchindexer.exe",
    // Kernel-level processes Task Manager shows by these names: nothing can end them.
    "secure system",
    "system interrupts",
    "vmmem",
    "vmmemwsl",
];

fn refusal_for(policy: &ProcessPolicy<'_>, name: &str, pid: u32) -> Option<String> {
    let lower = name.to_lowercase();
    if pid <= 4 || SYSTEM_NAMES.contains(&lower.as_str()) {
        Some("A Windows system process.".into())
    } else if pid == std::process::id()
        || (!policy.host_name_prefix.is_empty() && lower.starts_with(policy.host_name_prefix))
    {
        Some(format!("Part of {}.", policy.host_display_name))
    } else {
        None
    }
}

/// The process rows: `procs` grouped by lower-cased executable name, largest memory first.
/// `details` has the thread counts, executable paths and GPU use by pid.
pub fn grouped_rows(
    policy: &ProcessPolicy<'_>,
    procs: Vec<ProcessInfo>,
    details: &HashMap<u32, ProcessDetail>,
) -> Vec<ProcessRow> {
    let mut groups: HashMap<String, Vec<ProcessInfo>> = HashMap::new();
    for process in procs {
        groups
            .entry(process.name.to_lowercase())
            .or_default()
            .push(process);
    }
    let mut rows: Vec<ProcessRow> = groups
        .into_iter()
        .map(|(lower, members)| {
            let pids: HashSet<u32> = members.iter().map(|m| m.identity.pid).collect();
            // The lead is a member whose parent is outside the group; the largest if several.
            let lead = members
                .iter()
                .filter(|m| m.parent_pid.is_none_or(|parent| !pids.contains(&parent)))
                .max_by_key(|m| m.memory.value.unwrap_or(0))
                .or_else(|| members.iter().max_by_key(|m| m.memory.value.unwrap_or(0)))
                .map(|m| (m.identity.clone(), m.name.clone()))
                .unwrap_or_else(|| {
                    (
                        ProcessIdentity {
                            pid: 0,
                            start_time: 0,
                        },
                        lower.clone(),
                    )
                });
            let refusal = refusal_for(policy, &lead.1, lead.0.pid);
            let started_at = (lead.0.start_time > 0).then_some(lead.0.start_time);
            let executable = details
                .get(&lead.0.pid)
                .and_then(|d| d.executable.as_ref())
                .map(|path| path.to_string_lossy().into_owned());
            ProcessRow {
                key: format!("name:{lower}"),
                name: lead.1.clone(),
                bundle_id: None,
                app_path: None,
                lead: lead.0,
                cpu_usage_percent: members.iter().map(|m| m.cpu_usage_percent).sum(),
                memory_bytes: members.iter().map(|m| m.memory.value.unwrap_or(0)).sum(),
                threads: members
                    .iter()
                    .map(|m| details.get(&m.identity.pid).and_then(|d| d.threads))
                    .sum::<Option<u32>>(),
                // The members' readings added up; None only when no member has one.
                gpu_percent: members
                    .iter()
                    .filter_map(|m| details.get(&m.identity.pid).and_then(|d| d.gpu_percent))
                    .fold(None, |sum: Option<f32>, v| Some(sum.unwrap_or(0.0) + v)),
                started_at,
                executable,
                disk_io: members
                    .iter()
                    .filter_map(|m| m.disk_io)
                    .reduce(ProcessDiskIo::plus),
                members: members
                    .iter()
                    .map(|m| ProcessMember {
                        identity: m.identity.clone(),
                        name: m.name.clone(),
                        cpu_usage_percent: m.cpu_usage_percent,
                        memory_bytes: m.memory.value.unwrap_or(0),
                        threads: details.get(&m.identity.pid).and_then(|d| d.threads),
                        gpu_percent: details.get(&m.identity.pid).and_then(|d| d.gpu_percent),
                        started_at: (m.identity.start_time > 0).then_some(m.identity.start_time),
                        disk_io: m.disk_io,
                    })
                    .collect(),
                can_act: refusal.is_none(),
                refusal,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.memory_bytes
            .cmp(&a.memory_bytes)
            .then_with(|| a.name.cmp(&b.name))
    });
    rows
}

/// The group's row, only if `identity` is still its lead (same pid and start time).
fn verified(
    rows: &dyn Fn() -> Vec<ProcessRow>,
    key: &str,
    identity: &ProcessIdentity,
) -> Result<ProcessRow, String> {
    let row = rows()
        .into_iter()
        .find(|row| row.key == key)
        .ok_or_else(|| "That app has already quit.".to_string())?;
    if row.lead.pid != identity.pid || row.lead.start_time != identity.start_time {
        return Err("That process changed since the list was read. Nothing was closed.".into());
    }
    if let Some(reason) = &row.refusal {
        return Err(reason.clone());
    }
    Ok(row)
}

/// Ask the app to close (`taskkill /PID`, a close request its windows can answer with a save
/// prompt). RightKit's `AdoptedSignal::Graceful` is `TerminateProcess` on Windows (foreign.rs
/// `windows_impl::signal`), so it cannot stand in for this.
fn close_request(pid: u32) -> Result<(), String> {
    let output = rightkit_child_process::run(
        "taskkill.exe",
        ["/PID", &pid.to_string()],
        &rightkit_child_process::RunOptions::new(Duration::from_secs(15)),
    )
    .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&output.stderr.bytes)
        .trim()
        .to_string();
    let text = if text.is_empty() {
        String::from_utf8_lossy(&output.stdout.bytes)
            .trim()
            .to_string()
    } else {
        text
    };
    Err(if text.is_empty() {
        "taskkill failed".into()
    } else {
        text
    })
}

/// Force the app and everything it started to end. Each process is stopped only while it is still
/// the process that was read (pid and start time), leaves first, so a recycled pid is never
/// killed. `refusal` words the failure ("Windows did not let <host> close it.").
fn force_tree(
    rows: &dyn Fn() -> Vec<ProcessRow>,
    policy: &ProcessPolicy<'_>,
    key: &str,
    identity: &ProcessIdentity,
    pid: u32,
) -> Result<(), String> {
    let Some(pid) = std::num::NonZeroU32::new(pid) else {
        return Err("That process cannot be closed.".into());
    };
    let Some(root) =
        rightkit_child_process::AdoptedIdentity::capture(pid).map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    // Read again after the capture, so a pid reused in between is refused.
    verified(rows, key, identity)?;
    let mut targets = root.descendants().map_err(|e| e.to_string())?;
    targets.reverse();
    targets.push(root);
    let report =
        rightkit_child_process::sweep(&targets, Duration::ZERO).map_err(|e| e.to_string())?;
    if report.failed.is_empty() && report.survivors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Windows did not let {} close it.",
            policy.host_display_name
        ))
    }
}

fn still_running(rows: &dyn Fn() -> Vec<ProcessRow>, key: &str, pid: u32) -> bool {
    rows()
        .iter()
        .any(|row| row.key == key && row.members.iter().any(|m| m.identity.pid == pid))
}

/// Quit (`force` false: the close request) or Force Quit (the process tree) for the group `key`,
/// after checking that `identity` is still its lead. Gives the process about two seconds to go;
/// a graceful request may be answered with a save prompt. `rows` re-reads the process rows.
pub fn close(
    rows: &dyn Fn() -> Vec<ProcessRow>,
    policy: &ProcessPolicy<'_>,
    key: &str,
    identity: &ProcessIdentity,
    force: bool,
) -> Result<QuitOutcome, String> {
    let row = verified(rows, key, identity)?;
    if force {
        force_tree(rows, policy, key, identity, row.lead.pid)?;
    } else {
        close_request(row.lead.pid)?;
    }
    // Give the process a moment; a graceful request may be answered with a save prompt.
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(250));
        if !still_running(rows, key, row.lead.pid) {
            return Ok(QuitOutcome::Quit);
        }
    }
    Ok(QuitOutcome::StillRunning)
}
