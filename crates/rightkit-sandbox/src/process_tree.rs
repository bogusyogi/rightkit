//! Process-tree enumeration and bounded kill (ported from CodeRight
//! `process_tree.rs`). Unix only; on Windows the Job Object owns the tree.

use std::io;
use std::time::Duration;

#[cfg(unix)]
use std::time::Instant;

/// Hard bound on tree size we will enumerate or signal.
pub const MAX_TREE_NODES: usize = 4096;

#[cfg(unix)]
mod imp {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[cfg(target_os = "macos")]
    fn parent_map() -> io::Result<BTreeMap<i32, i32>> {
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        let mut pids = vec![0 as libc::pid_t; count as usize + 256];
        let n = unsafe {
            libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as libc::c_int)
        };
        if n <= 0 {
            return Err(io::Error::last_os_error());
        }
        pids.truncate(n as usize);
        let mut map = BTreeMap::new();
        for pid in pids.into_iter().filter(|p| *p > 0) {
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
            let r = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    size,
                )
            };
            if r == size {
                map.insert(pid, info.pbi_ppid as i32);
            }
        }
        Ok(map)
    }

    #[cfg(target_os = "linux")]
    fn parent_map() -> io::Result<BTreeMap<i32, i32>> {
        let mut map = BTreeMap::new();
        for e in std::fs::read_dir("/proc")?.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            // comm may contain spaces/parens: parse after the last ')'.
            if let Some(rest) = stat.rfind(')').map(|i| &stat[i + 1..]) {
                let mut f = rest.split_whitespace();
                if let Some(ppid) = f.nth(1).and_then(|s| s.parse::<i32>().ok()) {
                    map.insert(pid, ppid);
                }
            }
        }
        Ok(map)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn parent_map() -> io::Result<BTreeMap<i32, i32>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process enumeration unsupported on this unix",
        ))
    }

    pub fn descendants(root: u32) -> io::Result<Vec<u32>> {
        let parents = parent_map()?;
        let mut found: BTreeSet<i32> = BTreeSet::new();
        let mut frontier = vec![root as i32];
        while let Some(p) = frontier.pop() {
            for (&child, &ppid) in &parents {
                if ppid == p && child != root as i32 && found.insert(child) {
                    if found.len() > MAX_TREE_NODES {
                        return Err(io::Error::other("process tree exceeds MAX_TREE_NODES"));
                    }
                    frontier.push(child);
                }
            }
        }
        Ok(found.into_iter().map(|p| p as u32).collect())
    }

    #[cfg(target_os = "macos")]
    fn is_zombie(pid: u32) -> bool {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let r = unsafe {
            libc::proc_pidinfo(
                pid as libc::pid_t,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        r == size && info.pbi_status == 5 // SZOMB
    }

    #[cfg(target_os = "linux")]
    fn is_zombie(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| {
                s.rfind(')')
                    .map(|i| s[i + 1..].trim_start().starts_with('Z'))
            })
            .unwrap_or(false)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn is_zombie(_pid: u32) -> bool {
        false
    }

    pub fn is_alive(pid: u32) -> bool {
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        (r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
            && !is_zombie(pid)
    }

    pub fn signal(pid: u32, sig: i32) {
        unsafe { libc::kill(pid as libc::pid_t, sig) };
    }

    pub fn signal_group_if_leader(root: u32, sig: i32) {
        let pg = unsafe { libc::getpgid(root as libc::pid_t) };
        if pg == root as libc::pid_t {
            unsafe { libc::killpg(pg, sig) };
        }
    }
}

/// All transitive children of `root` (excluding `root`), bounded by [`MAX_TREE_NODES`].
pub fn descendants(root: u32) -> io::Result<Vec<u32>> {
    #[cfg(unix)]
    return imp::descendants(root);
    #[cfg(not(unix))]
    {
        let _ = root;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "use the Job Object on Windows",
        ))
    }
}

/// Whether `pid` exists and is not a zombie.
pub fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    return imp::is_alive(pid);
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Outcome of [`kill_tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeKillReport {
    /// Descendants that were signalled.
    pub signalled: Vec<u32>,
    /// Descendants still alive after SIGKILL + settle time (should be empty).
    pub survivors: Vec<u32>,
}

/// Terminate every descendant of `root` (and its process group if `root` leads
/// one): SIGTERM, wait up to `grace`, then SIGKILL. `root` itself is signalled
/// too but is the caller's to reap, so it is not reported as a survivor.
pub fn kill_tree(root: u32, grace: Duration) -> io::Result<TreeKillReport> {
    #[cfg(unix)]
    {
        let kids = imp::descendants(root)?;
        imp::signal_group_if_leader(root, libc::SIGTERM);
        for &p in kids.iter().chain(std::iter::once(&root)) {
            imp::signal(p, libc::SIGTERM);
        }
        let start = Instant::now();
        while start.elapsed() < grace && kids.iter().any(|&p| imp::is_alive(p)) {
            std::thread::sleep(Duration::from_millis(20));
        }
        // Re-enumerate: children forked during the grace window must not escape.
        let mut all = kids.clone();
        if let Ok(more) = imp::descendants(root) {
            for p in more {
                if !all.contains(&p) {
                    all.push(p);
                }
            }
        }
        imp::signal_group_if_leader(root, libc::SIGKILL);
        for &p in all.iter().chain(std::iter::once(&root)) {
            imp::signal(p, libc::SIGKILL);
        }
        let settle = Instant::now();
        while settle.elapsed() < Duration::from_secs(2) && all.iter().any(|&p| imp::is_alive(p)) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let survivors = all.iter().copied().filter(|&p| imp::is_alive(p)).collect();
        Ok(TreeKillReport {
            signalled: all,
            survivors,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (root, grace);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "use the Job Object on Windows",
        ))
    }
}
