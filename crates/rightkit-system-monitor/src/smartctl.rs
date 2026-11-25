//! Reading drives through smartctl for a host that has no drive map of its own (the Windows
//! notch), and filling the drives smartctl cannot reach from another process's saved readings
//! (Pulse #325, #335).
//!
//! * [`locate`]: `smartctl.exe` beside the host, under its `Helpers` folder, then in the
//!   smartmontools install folder.
//! * [`scan`]: lists drives with `--scan-open` (the device type smartctl detected comes with each
//!   name), falling back to `--scan`. The pinned `drivedb.h` beside smartctl is passed with `-B`
//!   when one ships.
//! * [`read_device`]: `-a -j` on one device with its detected type; a drive that gives no SMART
//!   status and is not plain NVMe is retried as an NVMe-over-USB bridge
//!   ([`USB_NVME_BRIDGES`]). Parsing is [`crate::drive_health::parse`].
//! * [`engine_readings`] and [`engine_merge`]: the readings another process saved with
//!   [`crate::drive_health::refresh`] (`drive-health-history.json`, unelevated NVMe log page,
//!   storage counters, temperature). A reading is attached to a drive only when the model name
//!   matches exactly on both sides and one engine disk has that model ([`model_key`]): neither
//!   source records a serial, so two drives of one model are never conflated (Codex review of
//!   Pulse e946ff28 and 4427d8ad). A drive smartctl reached keeps its own reading; the saved one
//!   replaces a stale one only when it is as new or newer; a reading older than one sampling
//!   interval is kept with its date but is not current ([`engine_fresh`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_child_process::RunOptions;

use crate::drive_health::{self, History, Reading};

/// Most drives read or merged in one pass.
pub const MAX_DRIVES: usize = 16;
/// Largest smartctl output read.
pub const OUTPUT_MAX: usize = 4 * 1024 * 1024;
/// Largest saved-history file read.
pub const HISTORY_MAX: u64 = 64 * 1024 * 1024;
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// NVMe-over-USB bridge types smartctl can be told to speak when auto-detection gets no SMART.
pub const USB_NVME_BRIDGES: [&str; 3] = ["sntjmicron", "sntasmedia", "sntrealtek"];

/// The smartctl a host shipped or the user installed: `<exe dir>/smartctl.exe`,
/// `<exe dir>/Helpers/smartctl.exe`, then `<program_files>/smartmontools/bin/smartctl.exe` (only
/// an absolute `program_files` counts). The first that is a file.
pub fn locate(exe: Option<&Path>, program_files: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = exe.and_then(Path::parent) {
        candidates.push(dir.join("smartctl.exe"));
        candidates.push(dir.join("Helpers").join("smartctl.exe"));
    }
    if let Some(files) = program_files.filter(|files| files.is_absolute()) {
        candidates.push(files.join("smartmontools").join("bin").join("smartctl.exe"));
    }
    drive_health::find_tool(&candidates)
}

/// The pinned drive database beside smartctl, when one ships (`-B`).
fn drivedb_for(tool: &Path) -> Option<String> {
    let path = tool.parent()?.join("drivedb.h");
    path.is_file().then(|| path.to_string_lossy().into_owned())
}

/// smartctl's arguments for one invocation: the drive database, then `extra`.
fn tool_args(db: Option<&str>, extra: &[&str]) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(db) = db {
        args.push("-B".to_string());
        args.push(db.to_string());
    }
    args.extend(extra.iter().map(|a| (*a).to_string()));
    args
}

fn run_json(tool: &Path, args: &[String]) -> Option<serde_json::Value> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let options = RunOptions::new(TOOL_TIMEOUT)
        .stdout_cap(OUTPUT_MAX)
        .stderr_cap(0);
    // smartctl's exit status is a bit mask of findings; the output decides.
    let done = rightkit_child_process::run(tool, &refs, &options)
        .ok()
        .filter(|done| !done.timed_out)?;
    serde_json::from_slice(&done.stdout.bytes).ok()
}

/// One drive smartctl listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The device name smartctl takes (`/dev/sda`, `/dev/pd0`).
    pub name: String,
    /// The type `--scan-open` detected (`nvme`, `sat`, ...); `None` from the plain `--scan`.
    pub kind: Option<String>,
}

/// The drives smartctl lists (at most [`MAX_DRIVES`]); `None` when it listed nothing usable.
pub fn scan(tool: &Path) -> Option<Vec<Device>> {
    let db = drivedb_for(tool);
    for (flag, typed) in [("--scan-open", true), ("--scan", false)] {
        let Some(found) = run_json(tool, &tool_args(db.as_deref(), &[flag, "-j"])) else {
            continue;
        };
        let list: Vec<Device> = found
            .get("devices")
            .and_then(serde_json::Value::as_array)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|d| {
                let name = d
                    .get("name")
                    .and_then(serde_json::Value::as_str)?
                    .to_string();
                let kind = d
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .filter(|_| typed)
                    .map(str::to_string);
                Some(Device { name, kind })
            })
            .take(MAX_DRIVES)
            .collect();
        if !list.is_empty() {
            return Some(list);
        }
    }
    None
}

/// What reading one device gave.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceRead {
    /// The model smartctl named, else the device name.
    pub name: String,
    /// `None` when this connection gives no SMART status.
    pub reading: Option<Reading>,
}

/// Reads one scanned device at Unix time `at`.
pub fn read_device(tool: &Path, device: &Device, at: u64) -> DeviceRead {
    let db = drivedb_for(tool);
    let db = db.as_deref();
    let mut args = tool_args(db, &["-a", "-j"]);
    if let Some(kind) = &device.kind {
        args.push("-d".to_string());
        args.push(kind.clone());
    }
    args.push(device.name.clone());
    let mut json = run_json(tool, &args);
    let mut reading = json
        .as_ref()
        .and_then(|j| drive_health::parse(j, at))
        .map(|(reading, _)| reading);
    // No SMART status through this connection: try the USB NVMe bridges, unless the scan already
    // knows the drive is plain NVMe.
    if reading.is_none() && device.kind.as_deref() != Some("nvme") {
        for bridge in USB_NVME_BRIDGES {
            let mut retry = tool_args(db, &["-a", "-j", "-d", bridge]);
            retry.push(device.name.clone());
            let Some(found) = run_json(tool, &retry) else {
                continue;
            };
            if let Some((read, _)) = drive_health::parse(&found, at) {
                json = Some(found);
                reading = Some(read);
                break;
            }
        }
    }
    let name = json
        .as_ref()
        .and_then(|j| j.get("model_name"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| device.name.clone(), str::to_string);
    DeviceRead { name, reading }
}

/// A model name compared case- and spacing-insensitively; never by containment, so "SSD 1" is not
/// "SSD 10".
pub fn model_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A saved reading counts as current for one sampling interval after it was taken.
pub fn engine_fresh(at: u64, now: u64) -> bool {
    now.saturating_sub(at) <= drive_health::SAMPLE_INTERVAL_SECS
}

/// The newest reading of each reachable disk in the history `dir` holds (the one
/// [`crate::drive_health::refresh`] saves), as `(model or key, reading)`. Only disks whose model
/// is non-empty and unique among the history's disks are given (see the module note). Absent,
/// oversized or unreadable file: none.
pub fn engine_readings(dir: &Path) -> Vec<(String, Reading)> {
    let path = dir.join(drive_health::HISTORY_FILE);
    if std::fs::metadata(&path).map_or(true, |meta| meta.len() > HISTORY_MAX) {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let Ok(root) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    if root.get("version").and_then(serde_json::Value::as_f64) != Some(1.0) {
        return Vec::new();
    }
    let Some(history) = root
        .get("data")
        .and_then(|data| serde_json::from_value::<History>(data.clone()).ok())
    else {
        return Vec::new();
    };
    let name_of = |key: &String, disk: &drive_health::DiskHistory| {
        disk.model.clone().unwrap_or_else(|| key.clone())
    };
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for (key, disk) in history.disks.iter().take(MAX_DRIVES) {
        *counts.entry(model_key(&name_of(key, disk))).or_default() += 1;
    }
    let mut out = Vec::new();
    for (key, disk) in history.disks.iter().take(MAX_DRIVES) {
        if !disk.reachable {
            continue;
        }
        let Some(reading) = disk.readings.last() else {
            continue;
        };
        let name = name_of(key, disk);
        let wanted = model_key(&name);
        if wanted.is_empty() || counts.get(&wanted).copied() != Some(1) {
            continue;
        }
        out.push((name, reading.clone()));
    }
    out
}

/// Whether a saved reading taken at `engine_at` replaces what a host holds for a drive
/// smartctl did not reach in this pass, and what `reachable` the drive then has: `Some(reachable)`
/// to replace, `None` to keep. A drive smartctl reached (`reachable`) always keeps its own
/// reading; otherwise the saved one wins when the held reading is absent or no newer (equal wins
/// too, so the two sources never trade places). `reachable` after replacing is
/// [`engine_fresh`].
pub fn engine_merge(
    reachable: bool,
    held_at: Option<u64>,
    engine_at: u64,
    now: u64,
) -> Option<bool> {
    let newer = held_at.is_none_or(|held| held <= engine_at);
    (!reachable && newer).then(|| engine_fresh(engine_at, now))
}
