//! A cross-platform snapshot of the process table: pid, parent pid, command line and working set.
//!
//! It replaces callers that shell out to `ps` or to PowerShell `Get-CimInstance Win32_Process`,
//! which is slow, spawns a console host on Windows and parses text that differs between hosts.
//! Nothing is kept between calls: each call builds a fresh `Vec` and releases every handle.
//!
//! * Linux: `/proc/<pid>/stat` (parent), `/proc/<pid>/statm` (resident pages) and
//!   `/proc/<pid>/cmdline` (arguments joined by one space).
//! * macOS: `proc_listpids`, `proc_pidinfo` (`PROC_PIDTBSDINFO`, `PROC_PIDTASKINFO`) and the
//!   `KERN_PROCARGS2` sysctl.
//! * Windows: a Toolhelp snapshot (pid, parent), `GetProcessMemoryInfo` (working set) and
//!   `NtQueryInformationProcess(ProcessCommandLineInformation)` (command line, Windows 8.1+).
//!
//! The table is a racy snapshot: a process can exit, or its pid be reused, while it is read.
//! Use `AdoptedIdentity` before signalling anything found here. A field the caller may not read
//! (another user's, protected or exiting process, a kernel thread) is `None`, never a guess, and
//! one unreadable process never fails the scan. A process whose parent cannot be read on Linux or
//! macOS (it exited between the listing and the read) is left out; on Windows the parent comes
//! from the snapshot itself, so every listed process has one. Other hosts return
//! `ErrorKind::Unsupported`.

use std::io;

/// One row of the process table.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ProcessEntry {
    /// The process id.
    pub pid: u32,
    /// The parent process id as the OS reports it (0 when it has none).
    pub parent_pid: u32,
    /// The full command line as one string. Linux and macOS join the arguments with a single
    /// space (the original quoting is not recoverable); Windows returns the raw command line.
    /// `None` when it cannot be read or is empty.
    pub command_line: Option<String>,
    /// Resident (working set) memory in bytes. `None` when it cannot be read.
    pub working_set_bytes: Option<u64>,
}

/// Snapshot every process the OS lets this process see.
///
/// Returns `ErrorKind::Unsupported` on hosts other than Linux, macOS and Windows.
pub fn process_table() -> io::Result<Vec<ProcessEntry>> {
    imp::table()
}

/// Parse `/proc/<pid>/stat` and return the parent pid. The command name (field 2) may contain
/// spaces and parentheses, so fields are counted after the last `)`.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn parse_stat_parent(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // After the name: state, ppid, ...
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Join NUL-separated `/proc/<pid>/cmdline` bytes into one space-separated string.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn join_cmdline(bytes: &[u8]) -> Option<String> {
    let trimmed = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    if trimmed.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(trimmed);
    let joined = text.split('\0').collect::<Vec<_>>().join(" ");
    (!joined.trim().is_empty()).then_some(joined)
}

/// Join a `KERN_PROCARGS2` buffer (argc, executable path, padding NULs, then argv) into one
/// space-separated string of the arguments, without the environment.
#[cfg(any(target_os = "macos", test))]
fn join_procargs2(buffer: &[u8]) -> Option<String> {
    let count = usize::try_from(i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?)).ok()?;
    let mut i = 4;
    while i < buffer.len() && buffer[i] != 0 {
        i += 1;
    }
    while i < buffer.len() && buffer[i] == 0 {
        i += 1;
    }
    let mut args = Vec::with_capacity(count);
    for _ in 0..count {
        if i >= buffer.len() {
            return None;
        }
        let start = i;
        while i < buffer.len() && buffer[i] != 0 {
            i += 1;
        }
        if i >= buffer.len() {
            return None; // cut short: the last argument has no terminator
        }
        args.push(String::from_utf8_lossy(&buffer[start..i]).into_owned());
        i += 1;
    }
    let joined = args.join(" ");
    (!joined.is_empty()).then_some(joined)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use super::{join_cmdline, parse_stat_parent, ProcessEntry};
    use std::io;

    pub(super) fn table() -> io::Result<Vec<ProcessEntry>> {
        // SAFETY: sysconf takes a plain name and has no memory effects.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
        let mut rows = Vec::new();
        for entry in std::fs::read_dir("/proc")?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            // A process that exited between the listing and this read is skipped.
            // Bytes, not a string: the command name in `stat` is not always valid UTF-8.
            let Some(parent_pid) = std::fs::read(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| parse_stat_parent(&String::from_utf8_lossy(&stat)))
            else {
                continue;
            };
            let working_set_bytes = std::fs::read_to_string(format!("/proc/{pid}/statm"))
                .ok()
                .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<u64>().ok())
                .map(|pages| pages.saturating_mul(page));
            let command_line = std::fs::read(format!("/proc/{pid}/cmdline"))
                .ok()
                .and_then(|bytes| join_cmdline(&bytes));
            rows.push(ProcessEntry {
                pid,
                parent_pid,
                command_line,
                working_set_bytes,
            });
        }
        Ok(rows)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{join_procargs2, ProcessEntry};
    use std::io;

    pub(super) fn table() -> io::Result<Vec<ProcessEntry>> {
        /// `PROC_ALL_PIDS` from `<libproc.h>`.
        const PROC_ALL_PIDS: u32 = 1;
        let size = std::mem::size_of::<libc::pid_t>();
        // SAFETY: a null buffer asks only for the size needed.
        let needed = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Err(io::Error::last_os_error());
        }
        let mut pids = vec![0 as libc::pid_t; needed as usize / size + 64];
        // SAFETY: the buffer holds `pids.len()` pids and the byte size passed says so.
        let written = unsafe {
            libc::proc_listpids(
                PROC_ALL_PIDS,
                0,
                pids.as_mut_ptr().cast(),
                (pids.len() * size) as libc::c_int,
            )
        };
        if written <= 0 {
            return Err(io::Error::last_os_error());
        }
        pids.truncate(written as usize / size);
        let mut rows = Vec::new();
        for pid in pids.into_iter().filter(|pid| *pid > 0) {
            let Some(parent_pid) = parent_of(pid) else {
                continue; // exited, or not readable
            };
            rows.push(ProcessEntry {
                pid: pid as u32,
                parent_pid,
                command_line: command_line(pid as u32),
                working_set_bytes: resident(pid),
            });
        }
        Ok(rows)
    }

    fn parent_of(pid: libc::pid_t) -> Option<u32> {
        // SAFETY: an all-zero `proc_bsdinfo` is a valid value (integers and char arrays).
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: `info` is a writable buffer of exactly `size` bytes.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        (written == size && info.pbi_pid == pid as u32).then_some(info.pbi_ppid)
    }

    fn resident(pid: libc::pid_t) -> Option<u64> {
        // SAFETY: an all-zero `proc_taskinfo` is a valid value (plain integers).
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: `info` is a writable buffer of exactly `size` bytes.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                (&mut info as *mut libc::proc_taskinfo).cast(),
                size,
            )
        };
        (written == size).then_some(info.pti_resident_size)
    }

    fn command_line(pid: u32) -> Option<String> {
        let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut capacity: usize = 0;
        // SAFETY: a null buffer asks only for the size needed; `name` has three entries.
        let probed = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut capacity,
                std::ptr::null_mut(),
                0,
            )
        };
        if probed != 0 {
            return None;
        }
        let mut buffer = vec![0u8; capacity + 4096];
        let mut length = buffer.len();
        // SAFETY: the buffer is writable for `length` bytes and `length` says so.
        let read = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                3,
                buffer.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        if read != 0 {
            return None;
        }
        buffer.truncate(length);
        join_procargs2(&buffer)
    }
}

#[cfg(windows)]
mod imp {
    use super::ProcessEntry;
    use crate::process_windows::os_error;
    use std::{ffi::c_void, io};
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            process: *mut c_void,
            class: u32,
            information: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }

    /// `ProcessCommandLineInformation`: a `UNICODE_STRING` followed by its text, Windows 8.1+.
    const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
    const STATUS_INFO_LENGTH_MISMATCH: u32 = 0xC000_0004;
    const STATUS_BUFFER_OVERFLOW: u32 = 0x8000_0005;
    const STATUS_BUFFER_TOO_SMALL: u32 = 0xC000_0023;

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *const u16,
    }

    struct Owned(HANDLE);
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful open or snapshot and is closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    pub(super) fn table() -> io::Result<Vec<ProcessEntry>> {
        // SAFETY: a snapshot request takes plain values.
        let snapshot =
            Owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(os_error)?);
        // SAFETY: PROCESSENTRY32W is plain data; dwSize is set below as the API requires.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: the snapshot is open and `entry` is a valid, sized entry.
        unsafe { Process32FirstW(snapshot.0, &mut entry) }.map_err(os_error)?;
        let mut rows = Vec::new();
        loop {
            let pid = entry.th32ProcessID;
            let (command_line, working_set_bytes) = if pid == 0 {
                (None, None) // the System Idle Process cannot be opened
            } else {
                read_process(pid)
            };
            rows.push(ProcessEntry {
                pid,
                parent_pid: entry.th32ParentProcessID,
                command_line,
                working_set_bytes,
            });
            // SAFETY: as above.
            if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
                break;
            }
        }
        Ok(rows)
    }

    /// Command line and working set of `pid`; each is `None` when the process is gone or may not
    /// be opened with limited query access (protected and other users' processes).
    fn read_process(pid: u32) -> (Option<String>, Option<u64>) {
        // SAFETY: OpenProcess takes plain values and returns a new handle.
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            return (None, None);
        };
        let process = Owned(handle);
        (command_line(&process), working_set(&process))
    }

    fn working_set(process: &Owned) -> Option<u64> {
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        counters.cb = size;
        // SAFETY: `counters` is a valid, sized structure and the handle has query access.
        unsafe { GetProcessMemoryInfo(process.0, &mut counters, size) }.ok()?;
        Some(counters.WorkingSetSize as u64)
    }

    fn command_line(process: &Owned) -> Option<String> {
        let mut size: usize = 1024;
        for _ in 0..4 {
            let mut buffer = vec![0u64; size.div_ceil(8)];
            let mut returned = 0u32;
            // SAFETY: the buffer is writable for the length passed; the handle has query access.
            let status = unsafe {
                NtQueryInformationProcess(
                    process.0 .0,
                    PROCESS_COMMAND_LINE_INFORMATION,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() * 8) as u32,
                    &mut returned,
                )
            } as u32;
            match status {
                0 => return read_unicode_string(&buffer),
                STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_OVERFLOW | STATUS_BUFFER_TOO_SMALL => {
                    size = (returned as usize).max(size * 2);
                }
                _ => return None, // denied, protected or exiting
            }
        }
        None
    }

    fn read_unicode_string(buffer: &[u64]) -> Option<String> {
        let base = buffer.as_ptr() as usize;
        let end = base + buffer.len() * 8;
        if buffer.len() * 8 < std::mem::size_of::<UnicodeString>() {
            return None;
        }
        // SAFETY: the buffer is aligned for u64 and large enough for the header.
        let header = unsafe { &*(buffer.as_ptr().cast::<UnicodeString>()) };
        let text = header.buffer as usize;
        let bytes = usize::from(header.length);
        if bytes == 0 {
            return None;
        }
        // The text must lie inside the buffer, after the header, and be 2-byte aligned.
        if text < base + std::mem::size_of::<UnicodeString>()
            || !text.is_multiple_of(2)
            || !bytes.is_multiple_of(2)
            || text + bytes > end
        {
            return None;
        }
        // SAFETY: the range was checked to lie inside the buffer and to be 2-byte aligned.
        let units = unsafe { std::slice::from_raw_parts(header.buffer, bytes / 2) };
        Some(String::from_utf16_lossy(units))
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
mod imp {
    use super::ProcessEntry;
    use std::io;

    pub(super) fn table() -> io::Result<Vec<ProcessEntry>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_parent_survives_spaces_and_parens_in_the_name() {
        assert_eq!(parse_stat_parent("42 (bash) S 7 42 42 0 -1"), Some(7));
        assert_eq!(parse_stat_parent("42 (a b) c) R 9 1 1"), Some(9));
        assert_eq!(parse_stat_parent("garbage"), None);
        assert_eq!(parse_stat_parent("42 (x) S"), None);
    }

    #[test]
    fn cmdline_joins_arguments_and_empty_is_none() {
        assert_eq!(
            join_cmdline(b"ls\0-l\0/tmp\0").as_deref(),
            Some("ls -l /tmp")
        );
        assert_eq!(join_cmdline(b"sd-server\0").as_deref(), Some("sd-server"));
        assert_eq!(join_cmdline(b""), None);
        assert_eq!(join_cmdline(b"\0"), None);
    }

    #[test]
    fn procargs2_yields_arguments_without_environment() {
        let mut buffer = 2i32.to_ne_bytes().to_vec();
        buffer.extend_from_slice(b"/bin/tool\0\0\0tool\0--flag\0HOME=/x\0");
        assert_eq!(join_procargs2(&buffer).as_deref(), Some("tool --flag"));
        assert_eq!(join_procargs2(&3i32.to_ne_bytes()), None);
        let mut cut = 1i32.to_ne_bytes().to_vec();
        cut.extend_from_slice(b"/bin/tool\0tool");
        assert_eq!(join_procargs2(&cut), None);
    }

    /// The parent of this test process, read independently of the table under test.
    #[cfg(unix)]
    fn own_parent() -> u32 {
        // SAFETY: getppid takes no arguments and cannot fail.
        unsafe { libc::getppid() as u32 }
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn the_table_contains_this_process_with_its_parent_and_memory() {
        let rows = process_table().expect("process table");
        let me = rows
            .iter()
            .find(|row| row.pid == std::process::id())
            .expect("own pid is listed");
        assert_ne!(me.parent_pid, std::process::id());
        #[cfg(unix)]
        assert_eq!(me.parent_pid, own_parent());
        assert!(me.working_set_bytes.unwrap_or(0) > 0);
        let line = me.command_line.as_deref().expect("own command line");
        assert!(!line.is_empty());
        #[cfg(unix)]
        {
            let argv: Vec<String> = std::env::args_os()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert_eq!(line, argv.join(" "));
        }
        let exe = std::env::current_exe().unwrap();
        let stem = exe.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(line.contains(&stem), "{line} should name {stem}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn a_spawned_child_is_listed_under_this_parent_with_its_arguments() {
        use std::process::{Command, Stdio};
        // A distinctive argument, so a forked child that has not exec'd yet (still showing this
        // process's command line) is never mistaken for the finished one.
        const MARK: &str = "7654";
        let mut command = if cfg!(windows) {
            let mut c = Command::new("ping.exe");
            c.args(["-n", MARK, "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg(MARK);
            c
        };
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let mut found = None;
        for _ in 0..50 {
            found = process_table().unwrap().into_iter().find(|row| {
                row.pid == pid
                    && row
                        .command_line
                        .as_deref()
                        .is_some_and(|line| line.contains(MARK))
            });
            if found.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        let row = found.expect("child listed with its command line");
        assert_eq!(row.parent_pid, std::process::id());
        let line = row.command_line.unwrap();
        let expected = if cfg!(windows) {
            format!("-n {MARK} 127.0.0.1")
        } else {
            format!("sleep {MARK}")
        };
        assert!(line.contains(&expected), "{line} should contain {expected}");
    }
}
