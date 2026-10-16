//! Finding, and optionally stopping, processes by executable path and command line.
//!
//! [`ProcessQuery`] describes which processes are wanted: the exact executable path, exact
//! argument tokens, substrings of single arguments, or any combination (all criteria must hold).
//! [`find_processes`] lists the matches without touching them; [`sweep_matching`] lists and then
//! stops them through [`sweep`](super::sweep), so every signal is gated by the pid-plus-start-time
//! identity of [`AdoptedIdentity`].
//!
//! # Safety rules
//!
//! * A query with no criterion is refused (`InvalidInput`), as is an empty path, token or
//!   substring: nothing may match "everything".
//! * Only processes owned by the current user are considered (Unix: real and effective uid;
//!   Windows: the process token's user SID). A process whose owner cannot be read is skipped.
//! * This process and every ancestor of it are never matched.
//! * A candidate is described twice, before and after its command line is read, and is kept only
//!   if both reads agree on the start time. The identity it carries was therefore read from one
//!   process, and a later signal is refused if the pid has changed hands since.
//! * A process whose executable or command line cannot be read does not match (an unreadable
//!   field is never treated as a wildcard).
//! * [`MatchSweepOptions::dry_run`] returns the matches without sending anything.
//!
//! # Limits
//!
//! * The command line is what the operating system reports. A program that rewrites its own
//!   argument area (Linux and macOS `setproctitle` style) is matched on the rewritten text.
//! * The executable path is compared as the OS reports it (Linux: the `/proc/<pid>/exe` target, so
//!   a deleted or replaced binary reads as `... (deleted)` and does not match; macOS:
//!   `proc_pidpath`; Windows: `QueryFullProcessImageNameW`, compared case-insensitively). On Unix
//!   the wanted path is matched both as given and as canonicalized.
//! * A process that `exec`s a different program between the match and the signal keeps its pid and
//!   start time, so the identity check cannot see it. The window is the time between the scan and
//!   the sweep.
//! * Unix: the identity check and `kill` are two system calls (see the module documentation of
//!   [`foreign`](super)). Windows closes that gap with one handle.
//! * Supported on Linux, macOS and Windows. Other hosts return `ErrorKind::Unsupported`.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    io,
    num::NonZeroU32,
    path::{Path, PathBuf},
    time::Duration,
};

use super::{sweep, sys, AdoptedIdentity, SweepReport};

/// Which processes to find. All criteria that are set must hold. Built with the methods below;
/// at least one criterion is required.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProcessQuery {
    exe: Option<PathBuf>,
    args_exact: Vec<OsString>,
    args_containing: Vec<String>,
}

impl ProcessQuery {
    pub fn new() -> Self {
        Self::default()
    }

    /// The process runs exactly this executable (compared case-insensitively on Windows).
    pub fn exe(mut self, path: impl Into<PathBuf>) -> Self {
        self.exe = Some(path.into());
        self
    }

    /// Some element of the command line (the program name included) equals `token` exactly.
    /// May be given more than once: each token must be present.
    pub fn arg(mut self, token: impl Into<OsString>) -> Self {
        self.args_exact.push(token.into());
        self
    }

    /// Some single element of the command line contains `needle` (case-sensitive; the program
    /// name included). May be given more than once: each needle must be found, in any element.
    pub fn arg_containing(mut self, needle: impl Into<String>) -> Self {
        self.args_containing.push(needle.into());
        self
    }

    fn validate(&self) -> io::Result<()> {
        let invalid = |message: &'static str| io::Error::new(io::ErrorKind::InvalidInput, message);
        if self.exe.is_none() && self.args_exact.is_empty() && self.args_containing.is_empty() {
            return Err(invalid("a process query needs at least one criterion"));
        }
        if self.exe.as_ref().is_some_and(|path| path.as_os_str().is_empty()) {
            return Err(invalid("the executable path of a process query is empty"));
        }
        if self.args_exact.iter().any(|token| token.is_empty()) {
            return Err(invalid("an argument token of a process query is empty"));
        }
        if self.args_containing.iter().any(String::is_empty) {
            return Err(invalid("an argument substring of a process query is empty"));
        }
        Ok(())
    }

    fn needs_args(&self) -> bool {
        !self.args_exact.is_empty() || !self.args_containing.is_empty()
    }
}

/// A process that satisfied a [`ProcessQuery`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchedProcess {
    identity: AdoptedIdentity,
    args: Option<Vec<OsString>>,
}

impl MatchedProcess {
    pub fn pid(&self) -> u32 {
        self.identity.pid()
    }

    /// The pid with the start time it had when matched: pass it to [`sweep`](super::sweep) or
    /// record it.
    pub fn identity(&self) -> &AdoptedIdentity {
        &self.identity
    }

    /// The executable path as the OS reported it, when it could be read.
    pub fn exe(&self) -> Option<&Path> {
        self.identity.exe()
    }

    /// The command line elements (program name first), present only when the query had an
    /// argument criterion.
    pub fn args(&self) -> Option<&[OsString]> {
        self.args.as_deref()
    }
}

/// How [`sweep_matching`] behaves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchSweepOptions {
    grace: Duration,
    dry_run: bool,
}

impl Default for MatchSweepOptions {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(2),
            dry_run: false,
        }
    }
}

impl MatchSweepOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// How long matches get to leave after the graceful signal before the forced one (Unix; on
    /// Windows there is one way to stop a process). Default 2 s.
    pub fn grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Return the matches and send nothing.
    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }
}

/// The result of [`sweep_matching`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchSweepReport {
    /// Everything the query matched, newest process first.
    pub matched: Vec<MatchedProcess>,
    /// Whether this was a dry run (then `sweep` is empty and nothing was sent).
    pub dry_run: bool,
    /// What happened to the matches, as pids. Empty for a dry run.
    pub sweep: SweepReport,
}

/// Every process that satisfies `query`, newest first (children tend to come before the
/// parents that started them). Nothing is signalled. Processes that cannot be inspected
/// (they exited, or belong to another user, or deny access) are skipped, never an error.
pub fn find_processes(query: &ProcessQuery) -> io::Result<Vec<MatchedProcess>> {
    query.validate()?;
    let wanted_exe = query.exe.as_deref().map(exe_candidates);
    let me = os::Me::current()?;
    let pairs = sys::process_pairs()?;
    let this = std::process::id();
    let excluded = ancestors(&pairs, this);
    let mut found = Vec::new();
    for &(pid, _) in &pairs {
        if pid == 0 || pid == this || excluded.contains(&pid) {
            continue;
        }
        if let Ok(Some(matched)) = inspect(query, wanted_exe.as_deref(), &me, pid) {
            found.push(matched);
        }
    }
    found.sort_by_key(|matched| std::cmp::Reverse(matched.identity.start_token()));
    Ok(found)
}

/// [`find_processes`], then stop what was found with the identity-checked [`sweep`] (unless
/// `options` ask for a dry run).
pub fn sweep_matching(
    query: &ProcessQuery,
    options: &MatchSweepOptions,
) -> io::Result<MatchSweepReport> {
    let matched = find_processes(query)?;
    if options.dry_run || matched.is_empty() {
        return Ok(MatchSweepReport {
            matched,
            dry_run: options.dry_run,
            sweep: SweepReport::default(),
        });
    }
    let targets: Vec<AdoptedIdentity> = matched
        .iter()
        .map(|found| found.identity.clone())
        .collect();
    let report = sweep(&targets, options.grace)?;
    Ok(MatchSweepReport {
        matched,
        dry_run: false,
        sweep: report,
    })
}

/// The ancestors of `this` according to the (pid, parent) table, with a guard against a table
/// that loops because a parent pid was recycled.
fn ancestors(pairs: &[(u32, u32)], this: u32) -> HashSet<u32> {
    let parents: HashMap<u32, u32> = pairs.iter().copied().collect();
    let mut found = HashSet::new();
    let mut current = this;
    while let Some(&parent) = parents.get(&current) {
        if parent == 0 || parent == this || !found.insert(parent) {
            break;
        }
        current = parent;
    }
    found
}

fn inspect(
    query: &ProcessQuery,
    wanted_exe: Option<&[PathBuf]>,
    me: &os::Me,
    pid: u32,
) -> io::Result<Option<MatchedProcess>> {
    let Some(nonzero) = NonZeroU32::new(pid) else {
        return Ok(None);
    };
    let Some(first) = sys::read(pid)? else {
        return Ok(None);
    };
    // Cheap rejection first: the executable is already in hand.
    if let Some(wanted) = wanted_exe {
        if !first.exe.as_deref().is_some_and(|seen| exe_matches(seen, wanted)) {
            return Ok(None);
        }
    }
    if !me.owns(pid)? {
        return Ok(None);
    }
    let args = if query.needs_args() {
        let Some(args) = os::command_line(pid)? else {
            return Ok(None);
        };
        if !args_match(query, &args) {
            return Ok(None);
        }
        Some(args)
    } else {
        None
    };
    // The process must still be the one first described, so the executable, the owner and the
    // command line all belong to one process; the executable is judged on this last read.
    let Some(last) = sys::read(pid)? else {
        return Ok(None);
    };
    if last.start != first.start {
        return Ok(None);
    }
    if let Some(wanted) = wanted_exe {
        if !last.exe.as_deref().is_some_and(|seen| exe_matches(seen, wanted)) {
            return Ok(None);
        }
    }
    Ok(Some(MatchedProcess {
        identity: AdoptedIdentity {
            pid: nonzero,
            start: last.start,
            exe: last.exe,
        },
        args,
    }))
}

fn args_match(query: &ProcessQuery, args: &[OsString]) -> bool {
    query
        .args_exact
        .iter()
        .all(|token| args.iter().any(|arg| arg == token))
        && query.args_containing.iter().all(|needle| {
            args.iter()
                .any(|arg| arg.to_string_lossy().contains(needle.as_str()))
        })
}

/// The wanted path as given, and on Unix also resolved (the OS reports resolved paths).
#[cfg(unix)]
fn exe_candidates(wanted: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![wanted.to_path_buf()];
    if let Ok(resolved) = std::fs::canonicalize(wanted) {
        if resolved != wanted {
            candidates.push(resolved);
        }
    }
    candidates
}

#[cfg(not(unix))]
fn exe_candidates(wanted: &Path) -> Vec<PathBuf> {
    vec![wanted.to_path_buf()]
}

fn exe_matches(seen: &Path, wanted: &[PathBuf]) -> bool {
    wanted.iter().any(|path| same_path(seen, path))
}

#[cfg(windows)]
fn same_path(a: &Path, b: &Path) -> bool {
    fn normal(path: &Path) -> String {
        let text = path.to_string_lossy().to_lowercase().replace('/', "\\");
        text.strip_prefix("\\\\?\\").unwrap_or(&text).to_owned()
    }
    normal(a) == normal(b)
}

#[cfg(not(windows))]
fn same_path(a: &Path, b: &Path) -> bool {
    a == b
}

/// Splits a Windows command line the way the C runtime does (backslash runs before a quote,
/// doubled quotes inside quotes, the program name without escapes).
#[cfg(any(windows, test))]
fn split_windows_command_line(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let blank = |c: char| c == ' ' || c == '\t';
    let mut i = 0;
    let mut out = Vec::new();
    while i < n && blank(chars[i]) {
        i += 1;
    }
    if i >= n {
        return out;
    }
    let mut program = String::new();
    if chars[i] == '"' {
        i += 1;
        while i < n && chars[i] != '"' {
            program.push(chars[i]);
            i += 1;
        }
        if i < n {
            i += 1;
        }
    } else {
        while i < n && !blank(chars[i]) {
            program.push(chars[i]);
            i += 1;
        }
    }
    out.push(program);
    loop {
        while i < n && blank(chars[i]) {
            i += 1;
        }
        if i >= n {
            break;
        }
        let mut arg = String::new();
        let mut quoted = false;
        while i < n {
            let c = chars[i];
            if c == '\\' {
                let mut run = 0;
                while i < n && chars[i] == '\\' {
                    run += 1;
                    i += 1;
                }
                if i < n && chars[i] == '"' {
                    arg.extend(std::iter::repeat_n('\\', run / 2));
                    if run % 2 == 1 {
                        arg.push('"');
                        i += 1;
                    }
                } else {
                    arg.extend(std::iter::repeat_n('\\', run));
                }
            } else if c == '"' {
                if quoted && i + 1 < n && chars[i + 1] == '"' {
                    arg.push('"');
                    i += 2;
                } else {
                    quoted = !quoted;
                    i += 1;
                }
            } else if blank(c) && !quoted {
                break;
            } else {
                arg.push(c);
                i += 1;
            }
        }
        out.push(arg);
    }
    out
}

/// The layout of the macOS `KERN_PROCARGS2` buffer: an `int` argc, the executable path, padding
/// NULs, then argc NUL-terminated arguments, then the environment. `None` when truncated.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn parse_procargs2(buffer: &[u8]) -> Option<Vec<OsString>> {
    use std::os::unix::ffi::OsStringExt;
    let count = i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?);
    let count = usize::try_from(count).ok()?;
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
            return None; // the last argument has no terminator: the buffer was cut short
        }
        args.push(OsString::from_vec(buffer[start..i].to_vec()));
        i += 1;
    }
    Some(args)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod os {
    use std::{ffi::OsString, io, os::unix::ffi::OsStringExt};

    pub(super) struct Me {
        uid: u32,
        euid: u32,
    }

    impl Me {
        pub(super) fn current() -> io::Result<Self> {
            // SAFETY: getuid and geteuid take no arguments and cannot fail.
            Ok(Self {
                uid: unsafe { libc::getuid() },
                euid: unsafe { libc::geteuid() },
            })
        }

        /// Whether `pid` has this user as real and effective uid. Not readable: `false`.
        pub(super) fn owns(&self, pid: u32) -> io::Result<bool> {
            let text = match std::fs::read_to_string(format!("/proc/{pid}/status")) {
                Ok(text) => text,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            };
            let Some(line) = text.lines().find_map(|line| line.strip_prefix("Uid:")) else {
                return Ok(false);
            };
            let mut ids = line.split_whitespace().map(|id| id.parse::<u32>().ok());
            Ok(ids.next().flatten() == Some(self.uid) && ids.next().flatten() == Some(self.euid))
        }
    }

    pub(super) fn command_line(pid: u32) -> io::Result<Option<Vec<OsString>>> {
        let mut bytes = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if bytes.is_empty() {
            return Ok(None); // a zombie or a kernel thread has no command line
        }
        if bytes.last() == Some(&0) {
            bytes.pop();
        }
        Ok(Some(
            bytes
                .split(|byte| *byte == 0)
                .map(|part| OsString::from_vec(part.to_vec()))
                .collect(),
        ))
    }
}

#[cfg(target_os = "macos")]
mod os {
    use std::{ffi::OsString, io};

    pub(super) struct Me {
        uid: libc::uid_t,
        euid: libc::uid_t,
    }

    impl Me {
        pub(super) fn current() -> io::Result<Self> {
            // SAFETY: getuid and geteuid take no arguments and cannot fail.
            Ok(Self {
                uid: unsafe { libc::getuid() },
                euid: unsafe { libc::geteuid() },
            })
        }

        /// Whether `pid` has this user as real and effective uid. Not readable: `false`.
        pub(super) fn owns(&self, pid: u32) -> io::Result<bool> {
            // SAFETY: an all-zero `proc_bsdinfo` is a valid value (integers and char arrays).
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
            // SAFETY: `info` is a writable buffer of exactly `size` bytes.
            let written = unsafe {
                libc::proc_pidinfo(
                    pid as libc::pid_t,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    size,
                )
            };
            if written != size || info.pbi_pid != pid {
                return Ok(false);
            }
            Ok(info.pbi_uid == self.euid && info.pbi_ruid == self.uid)
        }
    }

    pub(super) fn command_line(pid: u32) -> io::Result<Option<Vec<OsString>>> {
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
            return gone_or_error();
        }
        // Room for an argument area that grows between the two calls.
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
            return gone_or_error();
        }
        buffer.truncate(length);
        Ok(super::parse_procargs2(&buffer))
    }

    /// A process that exited, a zombie (`EINVAL`) or one this user may not read (`EPERM`)
    /// has no command line to match; anything else is reported.
    fn gone_or_error() -> io::Result<Option<Vec<OsString>>> {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) | Some(libc::EINVAL) | Some(libc::EPERM) => Ok(None),
            _ => Err(error),
        }
    }
}

#[cfg(windows)]
mod os {
    use std::{ffi::OsString, ffi::c_void, io};

    use windows::Win32::{
        Foundation::{
            CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE,
        },
        Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER},
        System::Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };

    use crate::process_windows::os_error;

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
            // SAFETY: the handle came from a successful open and is closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// `None` when the process is gone or may not be opened.
    fn open(pid: u32) -> io::Result<Option<Owned>> {
        // SAFETY: OpenProcess takes plain values and returns a new handle.
        match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(handle) => Ok(Some(Owned(handle))),
            Err(error)
                if error.code() == ERROR_INVALID_PARAMETER.to_hresult()
                    || error.code() == ERROR_ACCESS_DENIED.to_hresult() =>
            {
                Ok(None)
            }
            Err(error) => Err(os_error(error)),
        }
    }

    /// The bytes of the user SID of `process`'s token. `None` when the token is not readable
    /// (another user's or an elevated process).
    fn user_sid(process: HANDLE) -> io::Result<Option<Vec<u8>>> {
        let mut token = HANDLE::default();
        // SAFETY: `process` is a valid handle with query access; `token` is a valid output.
        match unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => return Ok(None),
            Err(error) => return Err(os_error(error)),
        }
        let token = Owned(token);
        let mut needed = 0u32;
        // SAFETY: a null buffer of length 0 asks only for the size; the call is expected to fail.
        let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut needed) };
        if needed == 0 {
            return Err(io::Error::other("the token user size could not be read"));
        }
        // u64 elements keep the buffer aligned for TOKEN_USER.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: the buffer is writable for `needed` bytes.
        unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                Some(buffer.as_mut_ptr().cast::<c_void>()),
                needed,
                &mut needed,
            )
        }
        .map_err(os_error)?;
        // SAFETY: the buffer now holds a TOKEN_USER; its SID pointer is checked to lie inside
        // the buffer before it is read.
        let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
        let sid = user.User.Sid.0 as *const u8;
        let base = buffer.as_ptr() as usize;
        let end = base + buffer.len() * 8;
        if sid.is_null() || (sid as usize) < base || (sid as usize) + 8 > end {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        // SAFETY: the SID header (8 bytes) lies inside the buffer; byte 1 is its sub-authority count.
        let length = 8 + 4 * usize::from(unsafe { *sid.add(1) });
        if (sid as usize) + length > end {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        // SAFETY: the whole SID lies inside the buffer.
        Ok(Some(unsafe { std::slice::from_raw_parts(sid, length) }.to_vec()))
    }

    pub(super) struct Me {
        sid: Vec<u8>,
    }

    impl Me {
        pub(super) fn current() -> io::Result<Self> {
            // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing.
            let sid = user_sid(unsafe { GetCurrentProcess() })?
                .ok_or_else(|| io::Error::other("the current user could not be read"))?;
            Ok(Self { sid })
        }

        /// Whether `pid`'s token names this user. Not readable: `false`.
        pub(super) fn owns(&self, pid: u32) -> io::Result<bool> {
            let Some(process) = open(pid)? else {
                return Ok(false);
            };
            Ok(user_sid(process.0)?.is_some_and(|sid| sid == self.sid))
        }
    }

    pub(super) fn command_line(pid: u32) -> io::Result<Option<Vec<OsString>>> {
        let Some(process) = open(pid)? else {
            return Ok(None);
        };
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
                0 => return Ok(read_unicode_string(&buffer)),
                STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_OVERFLOW | STATUS_BUFFER_TOO_SMALL => {
                    size = (returned as usize).max(size * 2);
                }
                _ => return Ok(None), // denied, protected or exiting: unknown, never a match
            }
        }
        Ok(None)
    }

    fn read_unicode_string(buffer: &[u64]) -> Option<Vec<OsString>> {
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
            return Some(Vec::new());
        }
        // The text must lie inside the buffer, after the header, and be properly aligned.
        if text < base + std::mem::size_of::<UnicodeString>()
            || text % 2 != 0
            || bytes % 2 != 0
            || text + bytes > end
        {
            return None;
        }
        // SAFETY: the range was checked to lie inside the buffer and to be 2-byte aligned.
        let units = unsafe { std::slice::from_raw_parts(header.buffer, bytes / 2) };
        let line = String::from_utf16_lossy(units);
        Some(
            super::split_windows_command_line(&line)
                .into_iter()
                .map(OsString::from)
                .collect(),
        )
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", windows)))]
mod os {
    use std::{ffi::OsString, io};

    pub(super) struct Me;

    impl Me {
        pub(super) fn current() -> io::Result<Self> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }

        pub(super) fn owns(&self, _pid: u32) -> io::Result<bool> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }

    pub(super) fn command_line(_pid: u32) -> io::Result<Option<Vec<OsString>>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(OsString::from).collect()
    }

    #[test]
    fn an_empty_query_and_empty_criteria_are_refused() {
        for query in [
            ProcessQuery::new(),
            ProcessQuery::new().exe(""),
            ProcessQuery::new().arg(""),
            ProcessQuery::new().arg_containing(""),
        ] {
            let error = find_processes(&query).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{query:?}");
        }
    }

    #[test]
    fn arguments_match_exactly_and_by_substring_and_all_criteria_must_hold() {
        let args = strings(&["chrome", "--user-data-dir=/p/rk-profile-1", "--flag"]);
        let both = ProcessQuery::new()
            .arg("--flag")
            .arg_containing("rk-profile-1");
        assert!(args_match(&both, &args));
        assert!(!args_match(&ProcessQuery::new().arg("--fla"), &args));
        assert!(!args_match(
            &ProcessQuery::new().arg_containing("rk-profile-2"),
            &args
        ));
        assert!(!args_match(&both.arg("--missing"), &args));
    }

    #[test]
    fn a_substring_is_found_inside_one_argument_not_across_two() {
        let args = strings(&["tool", "ab", "cd"]);
        assert!(args_match(&ProcessQuery::new().arg_containing("ab"), &args));
        assert!(!args_match(&ProcessQuery::new().arg_containing("b c"), &args));
        assert!(!args_match(&ProcessQuery::new().arg_containing("bcd"), &args));
    }

    #[test]
    fn ancestors_follow_the_parent_chain_and_stop_on_a_loop() {
        let table = [(10, 5), (5, 2), (2, 1), (1, 0), (7, 9), (9, 7)];
        assert_eq!(ancestors(&table, 10), HashSet::from([5, 2, 1]));
        assert_eq!(ancestors(&table, 7), HashSet::from([9]));
        assert!(ancestors(&table, 99).is_empty());
    }

    #[test]
    fn windows_command_lines_split_like_the_c_runtime() {
        assert_eq!(
            split_windows_command_line(r#""C:\Program Files\x.exe" a "b c" d\"e "f\\" g"#),
            vec![r"C:\Program Files\x.exe", "a", "b c", "d\"e", r"f\", "g"]
        );
        assert_eq!(
            split_windows_command_line("prog   one\ttwo"),
            vec!["prog", "one", "two"]
        );
        assert_eq!(split_windows_command_line(r#"p "" x"#), vec!["p", "", "x"]);
        assert_eq!(split_windows_command_line(r#"p "a""b" c"#), vec!["p", "a\"b", "c"]);
        assert_eq!(split_windows_command_line(r"p a\\b"), vec!["p", r"a\\b"]);
        assert!(split_windows_command_line("   ").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn procargs2_yields_the_arguments_and_not_the_environment() {
        let mut buffer = 3i32.to_ne_bytes().to_vec();
        buffer.extend_from_slice(b"/bin/sh\0\0\0sh\0-c\0read x\0HOME=/root\0");
        assert_eq!(
            parse_procargs2(&buffer).unwrap(),
            strings(&["sh", "-c", "read x"])
        );
        // Cut short in the middle of the last argument: not a partial match.
        let mut short = 3i32.to_ne_bytes().to_vec();
        short.extend_from_slice(b"/bin/sh\0\0sh\0-c\0rea");
        assert!(parse_procargs2(&short).is_none());
        assert!(parse_procargs2(&[1, 0]).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_compare_without_case_or_slash_style() {
        assert!(same_path(
            Path::new(r"C:\Program Files\App\x.exe"),
            Path::new("c:/program files/app/X.EXE")
        ));
        assert!(!same_path(Path::new(r"C:\a\x.exe"), Path::new(r"C:\b\x.exe")));
    }
}
