//! Network volumes (SMB, AFP, NFS and WebDAV shares on the Mac; mapped network drives on Windows):
//! reading a mount table or drive type to tell whether a volume is remote, and asking about one
//! without letting a dead share stall the caller. Moved out of Pulse's `volumes.rs`.
//!
//! Detection: on the Mac, each line of `/sbin/mount` carries a mount's file system type
//! (`f_fstypename`) and source (`f_mntfromname`); smbfs, afpfs, nfs and webdav are network volumes
//! and the source is the share ([`network_share_line`]). On Windows, `GetDriveTypeW` returning
//! `DRIVE_REMOTE` marks a lettered network drive and `WNetGetConnectionW` gives its UNC name
//! ([`share_of`]). Running `mount` or `df` is the caller's: this module parses their text.
//!
//! Time limit: every query about a volume that may be remote runs on a worker thread through
//! [`answer_in_time`]. A late answer is dropped, so one dead share cannot hold the caller.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{OnceLock, mpsc};
use std::time::Duration;

/// How long a query about a possibly remote volume may take before the volume is "not responding".
pub const LIMIT: Duration = Duration::from_secs(2);
/// Workers allowed to be running at once. A hung share blocks its `statfs` with no timeout, so
/// every page open used to leave one more thread behind for as long as the share stayed hung
/// (Pulse #501); past this many, a query is answered "not responding" without a new thread.
pub const MAX_WORKERS: usize = 8;
/// Workers running now, including ones whose answer came too late.
static WORKERS: AtomicUsize = AtomicUsize::new(0);
/// Queries refused because `MAX_WORKERS` were still running, since the process started.
static REFUSED: AtomicUsize = AtomicUsize::new(0);
/// The refusal report is made at most once per `FULL_REPORT_EVERY_MS`; this is when it last was
/// (epoch ms, 0 = not yet), and `UNREPORTED` the refusals since, which the next report carries.
static FULL_REPORTED_MS: AtomicU64 = AtomicU64::new(0);
static UNREPORTED: AtomicUsize = AtomicUsize::new(0);
const FULL_REPORT_EVERY_MS: u64 = 10 * 60 * 1000;

/// What the workers-full hook is told: the cap, the refusals since the last report and since start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkersFull {
    pub running: usize,
    pub refused_since_last: usize,
    pub refused_total: usize,
}

type FullHook = Box<dyn Fn(&WorkersFull) + Send + Sync>;
static FULL_HOOK: OnceLock<FullHook> = OnceLock::new();

/// Sets the function called when a query is refused at the worker cap: the first time and then at
/// most once per ten minutes, with the refusals since the last call, so a share that stays hung is
/// visible for as long as it lasts without a line per page open. The first hook set stays.
pub fn on_workers_full(hook: impl Fn(&WorkersFull) + Send + Sync + 'static) {
    let _ = FULL_HOOK.set(Box::new(hook));
}

/// Workers running now and queries refused at the cap since start.
pub fn worker_counts() -> (usize, usize) {
    (
        WORKERS.load(Ordering::Relaxed),
        REFUSED.load(Ordering::Relaxed),
    )
}

/// Ends a worker's count however the worker ends.
struct Running;

impl Drop for Running {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn report_workers_full(total: usize) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .max(1);
    let last = FULL_REPORTED_MS.load(Ordering::Relaxed);
    let due = last == 0 || now.saturating_sub(last) >= FULL_REPORT_EVERY_MS;
    if due
        && FULL_REPORTED_MS
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    {
        let since = UNREPORTED.swap(0, Ordering::AcqRel) + 1;
        if let Some(hook) = FULL_HOOK.get() {
            hook(&WorkersFull {
                running: MAX_WORKERS,
                refused_since_last: since,
                refused_total: total,
            });
        }
    } else {
        UNREPORTED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Runs `query` on a worker thread and returns its answer if it comes within [`LIMIT`].
/// `None` means no answer in time; the worker finishes later and its result is dropped. With
/// `MAX_WORKERS` still running (hung shares) it is `None` at once and no thread is started.
pub fn answer_in_time<T: Send + 'static>(query: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let claimed = WORKERS
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_WORKERS).then_some(n + 1)
        })
        .is_ok();
    if !claimed {
        let total = REFUSED.fetch_add(1, Ordering::Relaxed) + 1;
        report_workers_full(total);
        return None;
    }
    let running = Running;
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("rightkit-volume-query".into())
        .spawn(move || {
            let _running = running;
            let _ = tx.send(query());
        });
    // A thread that could not start dropped its closure, and with it the count.
    if spawned.is_err() {
        return None;
    }
    rx.recv_timeout(LIMIT).ok()
}

/// File system types a Mac mounts from a server.
pub const NETWORK_TYPES: [&str; 4] = ["smbfs", "afpfs", "nfs", "webdav"];

/// One line of `mount`: `//user@nas/Media on /Volumes/Media (smbfs, nodev, nosuid)`. The share
/// (`nas/Media`, protocol prefix and user name dropped) when the line is `mount` and a network
/// file system, else `None`.
pub fn network_share_line(line: &str, mount: &str) -> Option<String> {
    let (source, rest) = line.split_once(" on ")?;
    let (target, options) = rest.rsplit_once(" (")?;
    if target != mount {
        return None;
    }
    let kind = options.trim_end_matches(')').split(',').next()?.trim();
    if !NETWORK_TYPES.contains(&kind) {
        return None;
    }
    // Drop the protocol prefix and the user name: `//user@nas/Media` is shown as `nas/Media`.
    let source = source.strip_prefix("//").unwrap_or(source);
    let host = source.split_once('@').map_or(source, |(_, host)| host);
    Some(host.to_string())
}

/// The share `mount` is served from, found in the whole text `/sbin/mount` printed.
pub fn share_in_mount_table(text: &str, mount: &str) -> Option<String> {
    text.lines()
        .find_map(|line| network_share_line(line, mount))
}

/// `(total, available)` bytes from `df -Pk <mount>` output. The second line is
/// `filesystem total used available capacity% mount`; the mount point may hold spaces, so the
/// numbers are read from the columns before the first percentage.
pub fn parse_df_capacity(text: &str) -> Option<(u64, u64)> {
    let fields: Vec<&str> = text.lines().nth(1)?.split_whitespace().collect();
    let at = fields.iter().position(|field| field.ends_with('%'))?;
    let total: u64 = fields.get(at.checked_sub(3)?)?.parse().ok()?;
    let available: u64 = fields.get(at.checked_sub(1)?)?.parse().ok()?;
    Some((total.checked_mul(1024)?, available.checked_mul(1024)?))
}

/// `(total, free)` bytes from `df -Pk <path>` output. POSIX columns: filesystem, 1024-blocks, used,
/// available, capacity, mount. The filesystem and mount names may hold spaces, so the run of three
/// numbers followed by a percentage is searched for rather than counted from the left.
pub fn parse_df_figures(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().nth(1)?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    for start in 0..fields.len() {
        let Some(window) = fields.get(start..start + 3) else {
            break;
        };
        let numbers: Option<Vec<u64>> = window.iter().map(|f| f.parse::<u64>().ok()).collect();
        let capacity = fields.get(start + 3).is_some_and(|f| f.ends_with('%'));
        if let (Some(numbers), true) = (numbers, capacity) {
            let total = numbers[0].checked_mul(1024)?;
            let free = numbers[2].checked_mul(1024)?;
            return Some((total, free));
        }
    }
    None
}

/// Whether a file name is an app installer, judged by its extension and, for a program, its name:
/// an MSI, MSIX or APPX package, or an `.exe` whose name says setup or install (never uninstall).
pub fn is_installer_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some((stem, extension)) = lower.rsplit_once('.') else {
        return false;
    };
    match extension {
        "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" => true,
        "exe" => {
            let setup = stem.contains("setup") || stem.contains("install");
            setup && !stem.contains("uninstall")
        }
        _ => false,
    }
}

/// Entries read from a volume's root folder when looking for its installer.
pub const ROOT_ENTRIES: usize = 512;

/// The installer at the root of a mounted disk image (`E:\`): the first file by name that is an
/// installer ([`is_installer_name`]). Reads the root folder only, at most [`ROOT_ENTRIES`] entries.
pub fn installer_inside(root: &str) -> Option<String> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut names: Vec<String> = entries
        .take(ROOT_ENTRIES)
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| is_installer_name(name))
        .collect();
    names.sort();
    names.into_iter().next()
}

#[cfg(windows)]
const DRIVE_REMOTE: u32 = 4;

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn GetDiskFreeSpaceExW(
        root: *const u16,
        to_caller: *mut u64,
        total: *mut u64,
        free: *mut u64,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "mpr")]
unsafe extern "system" {
    fn WNetGetConnectionW(local: *const u16, remote: *mut u16, length: *mut u32) -> u32;
}

/// The UNC name of a mapped network drive (`mount` is `E:` or `E:\`), or `None` for a drive that
/// is not remote. A remote drive whose name cannot be read is named by its letter.
#[cfg(windows)]
pub fn share_of(mount: &str) -> Option<String> {
    let letter = mount.trim_end_matches(['\\', '/']);
    let root = wide(&format!("{letter}\\"));
    // SAFETY: `root` is NUL-terminated and lives for the call.
    if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_REMOTE {
        return None;
    }
    let local = wide(letter);
    let mut buffer = vec![0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: `local` is NUL-terminated and `buffer` holds `length` UTF-16 units.
    let code = unsafe { WNetGetConnectionW(local.as_ptr(), buffer.as_mut_ptr(), &mut length) };
    if code != 0 {
        return Some(letter.to_string());
    }
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

/// Total and available bytes of a drive: `(total, available)`.
#[cfg(windows)]
pub fn capacity(mount: &str) -> Option<(u64, u64)> {
    let root = wide(&format!("{}\\", mount.trim_end_matches(['\\', '/'])));
    let (mut to_caller, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `root` is NUL-terminated; the three out pointers are live u64 values.
    let ok = unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut to_caller, &mut total, &mut free) };
    (ok != 0).then_some((total, to_caller))
}
