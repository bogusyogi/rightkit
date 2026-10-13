//! Windows AppContainer confinement for [`crate::Sandbox::spawn`], ported from CodeRight
//! `engine/crates/tools/src/sandbox/windows_appcontainer.rs` (synchronous core; CodeRight's
//! tokio pipe adapter becomes blocking `File` pipes here).
//!
//! A Job Object bounds how much a process may consume; it does not bound what it may reach.
//! The boundary here is an AppContainer (lowbox token), deny-by-default for files and the
//! registry. The policy's paths are ACLed to the container SID for exactly as long as a
//! command that needs them is alive (the last one to end revokes the grant), and network is
//! a capability, not a filter: without `internetClient` the container has no egress at all.
//!
//! An AppContainer can only be established at process creation through a
//! `SECURITY_CAPABILITIES` attribute, so this path calls `CreateProcessW` directly. The child
//! is created suspended, assigned to a kill-on-close Job Object, and only then resumed, so it
//! never runs (or spawns) outside the job.

use crate::windows_job::{WindowsJob, DEFAULT_ACTIVE_PROCESS_LIMIT};
use crate::{EnvPolicy, NetworkPolicy, SandboxCommand, SandboxError, SandboxPolicy};
#[path = "windows_ancestors.rs"]
mod ancestors;
pub use ancestors::{
    sweep_stale_appcontainer_ancestor_aces, sweep_stale_appcontainer_ancestor_aces_for_owner,
    AncestorLeasePolicy, AppContainerAncestorLease, UnleasableAncestor,
};
use ancestors::{AncestorLease, PROFILE_PREFIX};
use std::collections::{BTreeMap, HashMap};
use std::ffi::{c_void, OsStr};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
const SE_FILE_OBJECT: u32 = 1;
const GRANT_ACCESS: i32 = 1;
const REVOKE_ACCESS: i32 = 4;
/// `SE_GROUP_ENABLED`: attribute for a capability SID in the token.
const SE_GROUP_ENABLED: u32 = 0x0000_0004;
/// Well-known `internetClient` capability SID: outbound network for an AppContainer
/// (no inbound, no private-network servers).
const INTERNET_CLIENT_CAPABILITY_SID: &str = "S-1-15-3-1";
/// ALL APPLICATION PACKAGES: every lowbox token satisfies it.
const ALL_APP_PACKAGES_SID: &str = "S-1-15-2-1";
const NO_MULTIPLE_TRUSTEE: i32 = 0;
const TRUSTEE_IS_SID: i32 = 0;
const TRUSTEE_IS_UNKNOWN: i32 = 0;
const SUB_CONTAINERS_AND_OBJECTS_INHERIT: u32 = 0x3;
const FILE_GENERIC_READ: u32 = 0x0012_0089;
const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
const FILE_GENERIC_EXECUTE: u32 = 0x0012_00a0;
const DELETE: u32 = 0x0001_0000;
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const ACL_REVISION: u8 = 2;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0x0;
const OBJECT_INHERIT_ACE: u8 = 0x1;
const CONTAINER_INHERIT_ACE: u8 = 0x2;
const NO_PROPAGATE_INHERIT_ACE: u8 = 0x4;
const INHERIT_ONLY_ACE: u8 = 0x8;
const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
const PIPE_ACCESS_INBOUND: u32 = 0x0000_0001;
const PIPE_ACCESS_OUTBOUND: u32 = 0x0000_0002;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
const ERROR_PIPE_CONNECTED: u32 = 535;
const ERROR_ALREADY_EXISTS_HRESULT: i32 = 0x8007_00B7_u32 as i32;
const INVALID_HANDLE: *mut c_void = (-1isize) as *mut c_void;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x102;

#[repr(C)]
struct TrusteeW {
    multiple_trustee: *mut TrusteeW,
    multiple_trustee_operation: i32,
    trustee_form: i32,
    trustee_type: i32,
    name: *mut u16,
}

#[repr(C)]
struct ExplicitAccessW {
    access_permissions: u32,
    access_mode: i32,
    inheritance: u32,
    trustee: TrusteeW,
}

/// Win32 `ACL` header.
#[repr(C)]
struct Acl {
    acl_revision: u8,
    _sbz1: u8,
    acl_size: u16,
    ace_count: u16,
    _sbz2: u16,
}

/// Win32 `ACE_HEADER`.
#[repr(C)]
struct AceHeader {
    ace_type: u8,
    ace_flags: u8,
    ace_size: u16,
}

/// Win32 `ACCESS_ALLOWED_ACE`; `sid_start` is the first DWORD of the inline SID.
#[repr(C)]
struct AccessAllowedAce {
    header: AceHeader,
    mask: u32,
    sid_start: u32,
}

#[repr(C)]
struct SidAndAttributes {
    sid: *mut c_void,
    attributes: u32,
}

/// Win32 `SECURITY_CAPABILITIES`.
#[repr(C)]
struct SecurityCapabilities {
    app_container_sid: *mut c_void,
    capabilities: *mut SidAndAttributes,
    capability_count: u32,
    reserved: u32,
}

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit_handle: i32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLogicalDrives() -> u32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetLastError() -> u32;
    fn GetCurrentProcess() -> *mut c_void;
    fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    fn CreateFileW(
        name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security: *const c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn DeviceIoControl(
        device: *mut c_void,
        control_code: u32,
        in_buffer: *const c_void,
        in_buffer_size: u32,
        out_buffer: *mut c_void,
        out_buffer_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buffer_size: u32,
        in_buffer_size: u32,
        default_timeout: u32,
        attributes: *const SecurityAttributes,
    ) -> *mut c_void;
    fn ConnectNamedPipe(handle: *mut c_void, overlapped: *mut c_void) -> i32;
}

#[link(name = "userenv")]
unsafe extern "system" {
    fn DeleteAppContainerProfile(name: *const u16) -> i32;
    fn CreateAppContainerProfile(
        name: *const u16,
        display_name: *const u16,
        description: *const u16,
        capabilities: *const SidAndAttributes,
        capability_count: u32,
        sid: *mut *mut c_void,
    ) -> i32;
    fn DeriveAppContainerSidFromAppContainerName(name: *const u16, sid: *mut *mut c_void) -> i32;
    fn GetAppContainerFolderPath(sid: *const u16, path: *mut *mut u16) -> i32;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(value: *const c_void);
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn FreeSid(sid: *mut c_void) -> *mut c_void;
    fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    fn ConvertStringSidToSidW(text: *const u16, sid: *mut *mut c_void) -> i32;
    fn GetAce(acl: *mut c_void, index: u32, ace: *mut *mut c_void) -> i32;
    fn EqualSid(sid1: *mut c_void, sid2: *mut c_void) -> i32;
    fn GetNamedSecurityInfoW(
        path: *mut u16,
        object_type: u32,
        information: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;
    fn SetEntriesInAclW(
        count: u32,
        entries: *mut ExplicitAccessW,
        old_acl: *mut c_void,
        new_acl: *mut *mut c_void,
    ) -> u32;
    fn SetNamedSecurityInfoW(
        path: *mut u16,
        object_type: u32,
        information: u32,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        sacl: *mut c_void,
    ) -> u32;
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(
        token: *mut c_void,
        information_class: u32,
        information: *mut c_void,
        information_length: u32,
        return_length: *mut u32,
    ) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_security_descriptor: *const u16,
        string_sd_revision: u32,
        security_descriptor: *mut *mut c_void,
        security_descriptor_size: *mut u32,
    ) -> i32;
}

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Read a NUL-terminated UTF-16 string.
///
/// SAFETY: `text` must point at a live NUL-terminated UTF-16 string.
unsafe fn read_wide(text: *const u16) -> String {
    let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) })
}

fn last_error(step: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{step}: {e}"))
}

/// Prefix an error with the failing step, keeping its kind.
fn step_error(step: impl std::fmt::Display, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{step}: {error}"))
}

fn close_raw(handle: RawHandle) {
    // SAFETY: each raw handle is closed exactly once by its owner.
    unsafe { CloseHandle(handle) };
}

/// Strip the `\\?\` verbatim prefix `canonicalize` adds; ACL APIs and junction targets
/// want plain drive-letter spellings.
fn plain_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix("\\\\?\\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Case-insensitive, separator-normalized key for a path.
fn lexical_path_key(path: &Path) -> String {
    plain_path(path)
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

fn fnv1a(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for unit in text.encode_utf16() {
        for byte in unit.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// Append one argument using the `CommandLineToArgvW` quoting rules. Quote only when the
/// argument is empty or contains whitespace or a quote (`cmd.exe` reads a quoted `"/c"` as a
/// file name), and double any backslashes that run into a quote.
fn append_quoted_arg(out: &mut String, arg: &str) {
    if !(arg.is_empty() || arg.contains([' ', '\t', '"'])) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                for _ in 0..=backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push('"');
            }
            _ => {
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // Trailing backslashes would otherwise escape the closing quote.
    for _ in 0..backslashes {
        out.push('\\');
    }
    out.push('"');
}

fn system_root() -> String {
    std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())
}

/// Confined `Path`: system directories first (so they cannot be shadowed), then the caller's
/// PATH so user-installed tools resolve as in the user's own shell.
fn confined_path(system32: &Path, system_root: &str) -> String {
    let mut dirs: Vec<String> = vec![
        system32.display().to_string(),
        system_root.to_string(),
        system32.join("Wbem").display().to_string(),
        system32
            .join("WindowsPowerShell")
            .join("v1.0")
            .display()
            .to_string(),
    ];
    if let Some(inherited) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&inherited) {
            let dir = dir.display().to_string();
            if dir.is_empty() || dirs.iter().any(|d| d.eq_ignore_ascii_case(&dir)) {
                continue;
            }
            dirs.push(dir);
        }
    }
    dirs.join(";")
}

/// Non-secret machine/user facts that command resolution and common tooling depend on
/// (`PATHEXT` lets cmd/PowerShell resolve `git` to `git.exe`). Always forwarded, like the
/// always-readable system locations on the other backends.
const PLATFORM_ENV: &[&str] = &[
    "USERPROFILE",
    "APPDATA",
    "HOMEDRIVE",
    "HOMEPATH",
    "PATHEXT",
    "PSModulePath",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "ProgramData",
    "PUBLIC",
    "ALLUSERSPROFILE",
    "USERNAME",
    "USERDOMAIN",
    "COMPUTERNAME",
    "OS",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

/// Build the child's Unicode environment block from the policy. Precedence, lowest first:
/// inherited (`inherit`), `keep`, platform facts, the container-reachable baseline (temp and
/// local app data live in the container's own folder), `set`, then the command's own vars.
fn confined_environment(
    cwd: &Path,
    profile_folder: &Path,
    policy: &EnvPolicy,
    extra: &[(String, String)],
) -> Vec<u16> {
    let mut vars: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut put = |key: &str, value: String| {
        vars.insert(key.to_ascii_uppercase(), (key.to_string(), value));
    };
    if policy.inherit {
        for (key, value) in std::env::vars_os() {
            put(&key.to_string_lossy(), value.to_string_lossy().into_owned());
        }
    }
    for key in policy
        .keep
        .iter()
        .map(String::as_str)
        .chain(PLATFORM_ENV.iter().copied())
    {
        if let Some(value) = std::env::var_os(key) {
            put(key, value.to_string_lossy().into_owned());
        }
    }
    let system_root = system_root();
    let system32 = Path::new(&system_root).join("System32");
    let system_drive = system_root.get(..2).unwrap_or("C:").to_string();
    let temp = profile_folder.join("Temp").display().to_string();
    put("ComSpec", system32.join("cmd.exe").display().to_string());
    put("Path", confined_path(&system32, &system_root));
    put("SystemDrive", system_drive.clone());
    put("SystemRoot", system_root.clone());
    put("windir", system_root.clone());
    put("LOCALAPPDATA", profile_folder.display().to_string());
    put("TEMP", temp.clone());
    put("TMP", temp);
    for (key, value) in policy.set.iter().chain(extra.iter().map(|(k, v)| (k, v))) {
        put(key, value.clone());
    }
    // Node resolves modules through realpath, which lstat()s the drive root; a lowbox token
    // cannot read `C:\` attributes, so a confined Node child dies with EPERM. Keep paths as
    // given (symlinked node_modules then resolve by link path).
    const NODE_SANDBOX_FLAGS: &str = "--preserve-symlinks --preserve-symlinks-main";
    let node = match vars.get("NODE_OPTIONS") {
        Some((_, value)) => format!("{value} {NODE_SANDBOX_FLAGS}"),
        None => NODE_SANDBOX_FLAGS.to_string(),
    };
    vars.insert("NODE_OPTIONS".into(), ("NODE_OPTIONS".into(), node));
    // A custom block needs the hidden per-drive current-directory entries: the cwd for its
    // drive, the root for every other logical drive.
    let cwd_text = cwd.display().to_string();
    let cwd_drive = cwd_text
        .get(..2)
        .filter(|d| d.ends_with(':'))
        .map(str::to_ascii_uppercase)
        .unwrap_or(system_drive);
    // SAFETY: no arguments; returns a bitmask.
    let logical = unsafe { GetLogicalDrives() };
    let mut drives: Vec<String> = (0..26u8)
        .filter(|i| logical & (1 << i) != 0)
        .map(|i| format!("{}:", (b'A' + i) as char))
        .collect();
    if !drives.contains(&cwd_drive) {
        drives.push(cwd_drive.clone());
    }
    for drive in drives {
        let value = if drive == cwd_drive {
            cwd_text.clone()
        } else {
            format!("{drive}\\")
        };
        let key = format!("={drive}");
        vars.insert(key.clone(), (key, value));
    }
    let mut block = Vec::new();
    for (key, value) in vars.values() {
        block.extend(OsStr::new(&format!("{key}={value}")).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// Resolve `program` to a full image path: `CreateProcessW` is given an explicit image, so
/// PATH lookup happens here (PATH, then System32 and Windows PowerShell).
fn resolve_program(program: &OsStr) -> io::Result<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let with_exe = |dir: &Path| -> Option<PathBuf> {
        let candidate = dir.join(path);
        if candidate.is_file() {
            return Some(candidate);
        }
        if path.extension().is_none() {
            let mut name = program.to_os_string();
            name.push(".exe");
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    };
    let system_root = system_root();
    let system32 = Path::new(&system_root).join("System32");
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .chain([
            system32.clone(),
            system32.join("WindowsPowerShell").join("v1.0"),
        ])
        .find_map(|dir| with_exe(&dir))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "resolve program '{}' for AppContainer",
                    program.to_string_lossy()
                ),
            )
        })
}

// ---------------------------------------------------------------------------------------
// ACL grants
// ---------------------------------------------------------------------------------------

/// `SetNamedSecurityInfoW` is a read-modify-write of the DACL, so concurrent grants in this
/// process must serialize or they silently lose each other's ACEs.
static ACL_GRANT_LOCK: Mutex<()> = Mutex::new(());

/// A SID allocated by `ConvertStringSidToSidW`, freed with `LocalFree`.
struct LocalSid(*mut c_void);

impl LocalSid {
    fn parse(text: &str) -> io::Result<Self> {
        let wide = wide(text);
        let mut sid = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated and `sid` a valid out-pointer.
        if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } == 0 {
            return Err(last_error("parse SID"));
        }
        Ok(Self(sid))
    }
}

/// Capability SIDs derived from a capability name; freed on drop.
struct DerivedCapabilitySids {
    group_array: *mut *mut c_void,
    group_count: u32,
    sid_array: *mut *mut c_void,
    sid_count: u32,
}

impl DerivedCapabilitySids {
    fn derive(name: &str) -> io::Result<Self> {
        let wname = wide(name);
        let mut derived = Self {
            group_array: std::ptr::null_mut(),
            group_count: 0,
            sid_array: std::ptr::null_mut(),
            sid_count: 0,
        };
        // SAFETY: NUL-terminated name and valid out-pointers; results freed in Drop.
        // Bound via the `windows` crate (api-ms-win-security-base), not a kernelbase.lib link.
        let result = unsafe {
            windows::Win32::Security::DeriveCapabilitySidsFromName(
                PCWSTR(wname.as_ptr()),
                (&mut derived.group_array as *mut *mut *mut c_void).cast(),
                &mut derived.group_count,
                (&mut derived.sid_array as *mut *mut *mut c_void).cast(),
                &mut derived.sid_count,
            )
        };
        if result.is_err() {
            return Err(last_error(&format!("derive capability SID {name}")));
        }
        Ok(derived)
    }

    fn sids(&self) -> Vec<*mut c_void> {
        if self.sid_array.is_null() {
            return Vec::new();
        }
        // SAFETY: the API returned `sid_count` entries.
        (0..self.sid_count as usize)
            .map(|i| unsafe { *self.sid_array.add(i) })
            .collect()
    }
}

impl Drop for DerivedCapabilitySids {
    fn drop(&mut self) {
        // SAFETY: every array and SID was allocated by the derive call (LocalAlloc).
        unsafe {
            for (array, count) in [
                (self.group_array, self.group_count),
                (self.sid_array, self.sid_count),
            ] {
                if array.is_null() {
                    continue;
                }
                for i in 0..count as usize {
                    LocalFree(*array.add(i));
                }
                LocalFree(array.cast());
            }
        }
    }
}

impl Drop for LocalSid {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSidToSidW; freed once.
        unsafe { LocalFree(self.0) };
    }
}

/// Whether an allow ACE's flags/mask already grant everything a request for `permissions`
/// with `inheritance` would produce: it must reach the object itself, hold every requested
/// bit, and propagate at least as deep as requested.
fn ace_covers_grant(ace_flags: u8, ace_mask: u32, permissions: u32, inheritance: u32) -> bool {
    if ace_flags & INHERIT_ONLY_ACE != 0 || ace_mask & permissions != permissions {
        return false;
    }
    let scope = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE;
    let requested_scope = (inheritance as u8) & scope;
    if ace_flags & scope & requested_scope != requested_scope {
        return false;
    }
    requested_scope == 0
        || (inheritance as u8) & NO_PROPAGATE_INHERIT_ACE != 0
        || ace_flags & NO_PROPAGATE_INHERIT_ACE == 0
}

/// Scan a live DACL for an allow ACE that already covers a grant to `sid`.
///
/// SAFETY: `dacl` must be null or point at a DACL inside a descriptor that outlives the
/// call; `sid` must be a valid SID.
unsafe fn dacl_covers_grant(
    dacl: *mut c_void,
    sid: *mut c_void,
    permissions: u32,
    inheritance: u32,
) -> bool {
    if dacl.is_null() {
        return false;
    }
    // SAFETY: only the fixed ACL header is read.
    let (revision, acl_size, ace_count) = unsafe {
        let acl = dacl as *const Acl;
        ((*acl).acl_revision, (*acl).acl_size, (*acl).ace_count)
    };
    if revision != ACL_REVISION {
        return false;
    }
    for index in 0..u32::from(ace_count) {
        let mut ace: *mut c_void = std::ptr::null_mut();
        // SAFETY: index < AceCount; the returned pointer borrows from the ACL.
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
            continue;
        }
        let offset = (ace as usize).wrapping_sub(dacl as usize);
        if offset >= usize::from(acl_size) {
            continue;
        }
        // SAFETY: ACEs are stored contiguously and DWORD-aligned inside the DACL; the size
        // is checked against the ACL bounds before any field beyond the header is trusted.
        let allowed = unsafe { &*(ace as *const AccessAllowedAce) };
        if allowed.header.ace_type != ACCESS_ALLOWED_ACE_TYPE
            || usize::from(allowed.header.ace_size) < std::mem::size_of::<AccessAllowedAce>()
            || offset + usize::from(allowed.header.ace_size) > usize::from(acl_size)
            || !ace_covers_grant(
                allowed.header.ace_flags,
                allowed.mask,
                permissions,
                inheritance,
            )
        {
            continue;
        }
        // SAFETY: the inline SID lies inside the ACE; `sid` is live.
        if unsafe { EqualSid(&allowed.sid_start as *const u32 as *mut c_void, sid) } != 0 {
            return true;
        }
    }
    false
}

/// Merge one explicit entry for `sid` into `path`'s DACL. A grant already covered by the live
/// DACL is skipped (no tree-wide inheritance propagation on the hot path); a revoke always
/// rewrites so inherited copies leave the whole tree.
fn apply_sid_path_entry(
    path: &Path,
    sid: *mut c_void,
    access_mode: i32,
    permissions: u32,
    inheritance: u32,
) -> io::Result<()> {
    let _guard = ACL_GRANT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut wide_path = wide(path);
    // SAFETY: wide_path is NUL-terminated; out-pointers are valid; every allocation is
    // freed exactly once below.
    unsafe {
        let mut old_dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        let got = GetNamedSecurityInfoW(
            wide_path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        );
        if got != 0 {
            return Err(io::Error::from_raw_os_error(got as i32));
        }
        if access_mode == GRANT_ACCESS && dacl_covers_grant(old_dacl, sid, permissions, inheritance)
        {
            LocalFree(descriptor);
            return Ok(());
        }
        let mut entry = ExplicitAccessW {
            access_permissions: permissions,
            access_mode,
            inheritance,
            trustee: TrusteeW {
                multiple_trustee: std::ptr::null_mut(),
                multiple_trustee_operation: NO_MULTIPLE_TRUSTEE,
                trustee_form: TRUSTEE_IS_SID,
                trustee_type: TRUSTEE_IS_UNKNOWN,
                name: sid.cast(),
            },
        };
        let mut new_dacl = std::ptr::null_mut();
        let merged = SetEntriesInAclW(1, &mut entry, old_dacl, &mut new_dacl);
        if merged != 0 {
            LocalFree(descriptor);
            return Err(io::Error::from_raw_os_error(merged as i32));
        }
        let set = SetNamedSecurityInfoW(
            wide_path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null_mut(),
        );
        LocalFree(new_dacl);
        LocalFree(descriptor);
        if set != 0 {
            return Err(io::Error::from_raw_os_error(set as i32));
        }
    }
    Ok(())
}

fn inheritance_for(path: &Path) -> u32 {
    if path.is_dir() {
        SUB_CONTAINERS_AND_OBJECTS_INHERIT
    } else {
        0
    }
}

fn permissions_for(writable: bool) -> u32 {
    if writable {
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE
    } else {
        FILE_GENERIC_READ | FILE_GENERIC_EXECUTE
    }
}

/// Whether `path` already carries a write-capable ACE for `sid` (a stale grant leaked by a
/// crashed run) that a read-only request must not inherit.
fn path_has_write_grant(path: &Path, sid: *mut c_void) -> bool {
    let mut wide_path = wide(path);
    // SAFETY: as in apply_sid_path_entry; the descriptor is freed after the DACL is read.
    unsafe {
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        if GetNamedSecurityInfoW(
            wide_path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        ) != 0
        {
            return false;
        }
        let covered = dacl_covers_grant(dacl, sid, FILE_GENERIC_WRITE, 0);
        LocalFree(descriptor);
        covered
    }
}

/// Whether `path`'s live DACL already lets ALL APPLICATION PACKAGES read and execute the
/// object itself (Program Files, System32, and anything ACLed like them). Such paths need
/// no lease, and the user usually could not re-ACL them anyway.
fn packages_can_read(path: &Path) -> bool {
    let Ok(packages) = LocalSid::parse(ALL_APP_PACKAGES_SID) else {
        return false;
    };
    let mut wide_path = wide(path);
    // SAFETY: as in apply_sid_path_entry; the descriptor is freed after the DACL is read.
    unsafe {
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        if GetNamedSecurityInfoW(
            wide_path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        ) != 0
        {
            return false;
        }
        let covered = dacl_covers_grant(
            dacl,
            packages.0,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            0,
        );
        LocalFree(descriptor);
        covered
    }
}

/// Process-wide reference counts of live ACL grants: one grant may back several concurrent
/// commands, and the ACE is revoked only when the last of them ends.
struct LeaseEntry {
    count: usize,
    writable: bool,
}

fn lease_registry() -> &'static Mutex<HashMap<String, LeaseEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, LeaseEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// One per-command allow grant for a container SID; dropping the last lease on a key revokes
/// the ACE, so grants never outlive the commands that needed them.
struct GrantLease {
    key: String,
    writable: bool,
    sid_text: String,
    path: PathBuf,
}

impl GrantLease {
    fn acquire(sid: *mut c_void, sid_text: &str, path: &Path, writable: bool) -> io::Result<Self> {
        let key = format!("{sid_text}|{}", lexical_path_key(path));
        let mut registry = lease_registry().lock().unwrap_or_else(|e| e.into_inner());
        let grant = |writable| {
            apply_sid_path_entry(
                path,
                sid,
                GRANT_ACCESS,
                permissions_for(writable),
                inheritance_for(path),
            )
            .map_err(|e| {
                let kind = if writable { "modify" } else { "read/execute" };
                step_error(format!("grant {kind} ACE on {}", path.display()), e)
            })
        };
        match registry.get_mut(&key) {
            Some(entry) => {
                if writable && !entry.writable {
                    grant(true)?;
                    entry.writable = true;
                }
                entry.count += 1;
            }
            None => {
                // No live lease owns the key, so a write-capable ACE here is stale: remove it
                // before granting narrower read/execute access.
                if !writable && path_has_write_grant(path, sid) {
                    apply_sid_path_entry(path, sid, REVOKE_ACCESS, 0, inheritance_for(path))
                        .map_err(|e| {
                            step_error(format!("revoke stale write ACE on {}", path.display()), e)
                        })?;
                }
                grant(writable)?;
                registry.insert(key.clone(), LeaseEntry { count: 1, writable });
            }
        }
        Ok(Self {
            key,
            writable,
            sid_text: sid_text.to_string(),
            path: path.to_path_buf(),
        })
    }
}

impl Drop for GrantLease {
    fn drop(&mut self) {
        let mut registry = lease_registry().lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = registry.get_mut(&self.key) else {
            return;
        };
        entry.count = entry.count.saturating_sub(1);
        if entry.count > 0 {
            return;
        }
        registry.remove(&self.key);
        // A failed revoke leaves an ACE for this container SID only; the next acquire for the
        // same key re-checks the live DACL (and strips a stale write grant).
        if let Ok(sid) = LocalSid::parse(&self.sid_text) {
            let _ = apply_sid_path_entry(
                &self.path,
                sid.0,
                REVOKE_ACCESS,
                0,
                inheritance_for(&self.path),
            );
        }
    }
}

/// The live leases of one launch, shared with the child so the grants live exactly as long
/// as it does.
#[derive(Default)]
struct LeaseSet {
    items: Mutex<Vec<GrantLease>>,
    ancestors: Mutex<Vec<AncestorLease>>,
    /// Marks the profile in use for as long as these leases live.
    holder: Mutex<Option<ancestors::ProfileHolder>>,
}

// ---------------------------------------------------------------------------------------
// Launch alias
// ---------------------------------------------------------------------------------------

/// Create a directory junction at `link` targeting `target` without spawning `mklink`.
/// `link` must not exist; on failure the empty directory is removed again.
fn create_junction(link: &Path, target: &Path) -> io::Result<()> {
    std::fs::create_dir(link)?;
    let outcome = (|| {
        let wide_link = wide(link);
        // SAFETY: opening the empty directory just created, with OPEN_REPARSE_POINT so the
        // reparse write lands on the link itself.
        let handle = unsafe {
            CreateFileW(
                wide_link.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE {
            return Err(last_error("open junction"));
        }
        let target_text = target.to_string_lossy();
        // The substitute name uses the NT namespace spelling.
        let substitute: Vec<u16> = format!("\\??\\{target_text}").encode_utf16().collect();
        let print: Vec<u16> = target_text.encode_utf16().collect();
        // REPARSE_DATA_BUFFER for a mount point: tag u32 | data-len u16 | reserved u16 |
        // subst-offset u16 | subst-len u16 | print-offset u16 | print-len u16 | names, each
        // NUL-terminated (Windows rejects packed names with ERROR_INVALID_REPARSE_DATA).
        let subst_bytes = substitute.len() * 2 + 2;
        let print_bytes = print.len() * 2 + 2;
        let data_len = (8 + subst_bytes + print_bytes) as u16;
        let mut buffer = Vec::<u8>::with_capacity(8 + usize::from(data_len));
        buffer.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buffer.extend(data_len.to_le_bytes());
        buffer.extend(0u16.to_le_bytes());
        buffer.extend(0u16.to_le_bytes());
        buffer.extend(((substitute.len() * 2) as u16).to_le_bytes());
        buffer.extend((subst_bytes as u16).to_le_bytes());
        buffer.extend(((print.len() * 2) as u16).to_le_bytes());
        for unit in substitute
            .iter()
            .chain([&0u16])
            .chain(print.iter())
            .chain([&0u16])
        {
            buffer.extend(unit.to_le_bytes());
        }
        let mut returned = 0u32;
        // SAFETY: live directory handle and a well-formed mount-point reparse buffer.
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        } != 0;
        let result = if ok {
            Ok(())
        } else {
            Err(last_error("set junction reparse point"))
        };
        // SAFETY: closed exactly once.
        unsafe { CloseHandle(handle) };
        result
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_dir(link);
    }
    outcome
}

/// Root beneath which launch junctions live. `%SystemRoot%\Temp` carries ALL APPLICATION
/// PACKAGES ACEs on stock Windows, so a container can read every ancestor of a path below it.
fn alias_root() -> PathBuf {
    PathBuf::from(system_root()).join("Temp").join("rightkit")
}

/// The alias a confined child must start beneath to observe `root`.
///
/// PowerShell normalizes its startup directory by probing attributes on every ancestor. A
/// lowbox token satisfies allow ACEs only for its container SID, ALL (RESTRICTED) APPLICATION
/// PACKAGES and capability SIDs, and an ancestor like `C:\Users` carries none of them, so the
/// probe fails and PowerShell silently falls back to the drive root. A junction under
/// `%SystemRoot%\Temp\rightkit` aliases the granted root with ancestors the token can read,
/// while the root's own DACL stays the boundary on every real object. Without a verified
/// alias the launch is refused, never moved to a different directory.
fn ensure_alias(root: &Path) -> io::Result<PathBuf> {
    ensure_alias_at(&alias_root(), root)
}

fn ensure_alias_at(base: &Path, root: &Path) -> io::Result<PathBuf> {
    let packages = LocalSid::parse(ALL_APP_PACKAGES_SID)?;
    std::fs::create_dir_all(base)?;
    let read = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
    apply_sid_path_entry(
        base,
        packages.0,
        GRANT_ACCESS,
        read,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    )?;
    let link = base.join(format!("ws-{:016x}", fnv1a(&lexical_path_key(root))));
    let target = plain_path(root);
    let canonical_root = std::fs::canonicalize(root)?;
    // Concurrent first use races on one name: losers re-check until the link resolves to
    // `root`; a foreign or stale entry is removed and recreated; a link that never settles
    // is an error.
    for _ in 0..40 {
        if link.symlink_metadata().is_ok() {
            match std::fs::read_link(&link) {
                Ok(current)
                    if std::fs::canonicalize(&current).is_ok_and(|r| r == canonical_root) =>
                {
                    apply_sid_path_entry(&link, packages.0, GRANT_ACCESS, read, 0)?;
                    return Ok(link);
                }
                Ok(_) => {
                    std::fs::remove_dir(&link)?;
                    continue;
                }
                Err(_) => {
                    if std::fs::remove_dir(&link).is_ok() {
                        continue;
                    }
                }
            }
        } else {
            match create_junction(&link, &target) {
                Ok(()) => {
                    apply_sid_path_entry(&link, packages.0, GRANT_ACCESS, read, 0)?;
                    return Ok(link);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(io::Error::other(format!(
        "launch alias {} did not settle to {}",
        link.display(),
        root.display()
    )))
}

// ---------------------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------------------

/// An AppContainer profile plus its SID. Profiles persist across runs by design and are
/// keyed by the policy's grant set, so a command confined for one set of paths never
/// inherits ACEs granted to another.
struct AppContainerProfile {
    name: String,
    sid: *mut c_void,
    sid_text: String,
    leases: Arc<LeaseSet>,
}

impl AppContainerProfile {
    /// Create the named profile, or adopt it when a previous run already made one. Only
    /// `ALREADY_EXISTS` adopts; other failures (userenv reports a concurrent-creation race
    /// under varying HRESULTs) retry briefly and are then returned unchanged.
    fn ensure(name: &str) -> io::Result<Self> {
        let wname = wide(name);
        let display = wide("RightKit sandbox");
        let mut attempt = 0;
        let sid = loop {
            let mut sid = std::ptr::null_mut();
            // SAFETY: NUL-terminated strings; no capabilities at creation.
            let hr = unsafe {
                CreateAppContainerProfile(
                    wname.as_ptr(),
                    display.as_ptr(),
                    display.as_ptr(),
                    std::ptr::null(),
                    0,
                    &mut sid,
                )
            };
            if hr >= 0 {
                break sid;
            }
            if hr == ERROR_ALREADY_EXISTS_HRESULT {
                // SAFETY: as above.
                let hr =
                    unsafe { DeriveAppContainerSidFromAppContainerName(wname.as_ptr(), &mut sid) };
                if hr < 0 {
                    return Err(io::Error::other(format!(
                        "adopt AppContainer profile {name}: HRESULT 0x{hr:08x}"
                    )));
                }
                break sid;
            }
            attempt += 1;
            if attempt >= 8 {
                return Err(io::Error::other(format!(
                    "create AppContainer profile {name}: HRESULT 0x{hr:08x}"
                )));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut raw = std::ptr::null_mut();
        // SAFETY: `sid` is a valid SID; `raw` receives a LocalAlloc string freed below.
        if unsafe { ConvertSidToStringSidW(sid, &mut raw) } == 0 {
            let e = last_error("AppContainer SID to string");
            unsafe { FreeSid(sid) };
            return Err(e);
        }
        // SAFETY: NUL-terminated LocalAlloc string, freed once.
        let sid_text = unsafe { read_wide(raw) };
        unsafe { LocalFree(raw.cast()) };
        Ok(Self {
            name: name.to_string(),
            sid,
            sid_text,
            leases: Arc::default(),
        })
    }

    /// The container's own folder (`...\Packages\<name>\AC`), always reachable by it.
    fn folder_path(&self) -> io::Result<PathBuf> {
        let wsid = wide(&self.sid_text);
        let mut raw = std::ptr::null_mut();
        // SAFETY: NUL-terminated SID string; `raw` is a CoTaskMemAlloc string freed below.
        let hr = unsafe { GetAppContainerFolderPath(wsid.as_ptr(), &mut raw) };
        if hr < 0 || raw.is_null() {
            return Err(io::Error::other(format!(
                "AppContainer folder path: HRESULT 0x{hr:08x}"
            )));
        }
        let path = PathBuf::from(unsafe { read_wide(raw) });
        unsafe { CoTaskMemFree(raw as *const c_void) };
        Ok(path)
    }

    /// Grant `path` to this profile for the lifetime of the launch's child.
    fn lease(&self, path: &Path, writable: bool) -> io::Result<()> {
        let key = format!("{}|{}", self.sid_text, lexical_path_key(path));
        let mut items = self.leases.items.lock().unwrap_or_else(|e| e.into_inner());
        if items
            .iter()
            .any(|l| l.key == key && (l.writable || !writable))
        {
            return Ok(());
        }
        items.push(GrantLease::acquire(
            self.sid,
            &self.sid_text,
            path,
            writable,
        )?);
        Ok(())
    }

    /// AppContainer does not inherit the caller's executable ACL: grant read/execute on the
    /// image's real directory unless it is a system or Program Files image, which already
    /// grant ALL APPLICATION PACKAGES (and which the user could not re-ACL anyway).
    fn lease_image_dir(&self, program: &Path) -> io::Result<()> {
        let dir = program.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "program has no containing directory",
            )
        })?;
        let dir = std::fs::canonicalize(dir)
            .map(|d| plain_path(&d))
            .unwrap_or_else(|_| dir.to_path_buf());
        let text = lexical_path_key(&dir);
        let under = |root: &str| {
            let root = lexical_path_key(Path::new(root));
            !root.is_empty() && (text == root || text.starts_with(&format!("{root}\\")))
        };
        let system32 = format!("{}\\System32", system_root());
        let program_files = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
            .iter()
            .filter_map(std::env::var_os)
            .any(|root| under(&root.to_string_lossy()));
        // The env check misses scrubbed environments; the live DACL is authoritative.
        if under(&system32) || program_files || packages_can_read(&dir) {
            return Ok(());
        }
        let target = image_lease_target(program, &dir);
        self.lease(&target, false)
            .map_err(|e| step_error(format!("lease image {}", target.display()), e))
    }
}

/// Directories larger than this get a lease on the image file alone: an inherited grant on
/// a huge tree (a cargo `target\deps`) propagates to every entry on each lease and revoke.
const IMAGE_DIR_LEASE_MAX_ENTRIES: usize = 8192;

fn image_lease_target(program: &Path, dir: &Path) -> PathBuf {
    let large = std::fs::read_dir(dir)
        .map(|entries| {
            entries.take(IMAGE_DIR_LEASE_MAX_ENTRIES + 1).count() > IMAGE_DIR_LEASE_MAX_ENTRIES
        })
        .unwrap_or(false);
    if large {
        program.to_path_buf()
    } else {
        dir.to_path_buf()
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        // SAFETY: from CreateAppContainerProfile/DeriveAppContainerSidFromAppContainerName.
        unsafe { FreeSid(self.sid) };
    }
}

// ---------------------------------------------------------------------------------------
// Pipes
// ---------------------------------------------------------------------------------------

fn set_inheritable(handle: RawHandle) -> io::Result<()> {
    // SAFETY: a live handle owned by the pipe setup.
    if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
        return Err(last_error("SetHandleInformation"));
    }
    Ok(())
}

fn current_user_sid() -> io::Result<String> {
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER: u32 = 1;
    let mut token = std::ptr::null_mut();
    // SAFETY: pseudo-handle and a valid out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error("OpenProcessToken"));
    }
    let result = (|| {
        let mut needed = 0u32;
        // SAFETY: sizing call with a null buffer.
        unsafe { GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(last_error("GetTokenInformation size"));
        }
        let mut buffer = vec![0u8; needed as usize];
        // SAFETY: buffer has the size reported above.
        if unsafe {
            GetTokenInformation(
                token,
                TOKEN_USER,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(last_error("GetTokenInformation"));
        }
        // SAFETY: TOKEN_USER starts with the SID pointer; buffer stays live.
        let sid = unsafe { std::ptr::read_unaligned(buffer.as_ptr() as *const *mut c_void) };
        let mut text = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(last_error("ConvertSidToStringSidW"));
        }
        // SAFETY: NUL-terminated LocalAlloc string, freed once.
        let value = unsafe { read_wide(text) };
        unsafe { LocalFree(text.cast()) };
        Ok(value)
    })();
    // SAFETY: token from OpenProcessToken.
    unsafe { CloseHandle(token) };
    result
}

/// Pipe DACL: the user and the container only, with a label low enough for the lowbox token
/// to write its end.
struct PipeSecurity(*mut c_void);

impl PipeSecurity {
    fn new(container_sid: &str) -> io::Result<Self> {
        let user = current_user_sid()?;
        let sddl = wide(format!(
            "D:P(A;;GA;;;{user})(A;;GA;;;{container_sid})S:(ML;;NW;;;S-1-16-0)"
        ));
        let mut descriptor = std::ptr::null_mut();
        let mut size = 0u32;
        // SAFETY: NUL-terminated SDDL and valid out-pointers.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                &mut size,
            )
        } == 0
        {
            return Err(last_error("pipe security descriptor"));
        }
        Ok(Self(descriptor))
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: allocated by the SDDL conversion; freed once.
        unsafe { LocalFree(self.0) };
    }
}

/// An inbound pipe: the parent reads `File`, the child writes the inheritable raw handle.
fn output_pipe(security: &PipeSecurity) -> io::Result<(std::fs::File, RawHandle)> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let name = wide(format!(
        r"\\.\pipe\rightkit-sandbox-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let attributes = SecurityAttributes {
        length: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor: security.0,
        inherit_handle: 0,
    };
    // SAFETY: name and attributes outlive the call.
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            &attributes,
        )
    };
    if server.is_null() || server == INVALID_HANDLE {
        return Err(last_error("CreateNamedPipeW"));
    }
    // SAFETY: opens the unique client end of the server just created.
    let client = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if client.is_null() || client == INVALID_HANDLE {
        let e = last_error("open pipe client");
        close_raw(server);
        return Err(e);
    }
    // SAFETY: synchronous connect of a server whose client is already open.
    if unsafe { ConnectNamedPipe(server, std::ptr::null_mut()) } == 0
        && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED
    {
        let e = last_error("ConnectNamedPipe");
        close_raw(client);
        close_raw(server);
        return Err(e);
    }
    if let Err(e) = set_inheritable(client) {
        close_raw(client);
        close_raw(server);
        return Err(e);
    }
    // SAFETY: the server end's ownership moves into the File.
    Ok((unsafe { std::fs::File::from_raw_handle(server) }, client))
}

/// One auxiliary pipe. Parent keeps an overlapped, non-inheritable server end;
/// the child receives only its synchronous client endpoint through the explicit
/// process handle list.
fn auxiliary_pipe(
    security: &PipeSecurity,
    to_child: bool,
) -> io::Result<(OwnedHandle, OwnedHandle)> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let name = wide(format!(
        r"\\.\pipe\rightkit-sandbox-aux-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let attributes = SecurityAttributes {
        length: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor: security.0,
        inherit_handle: 0,
    };
    let (server_access, client_access) = if to_child {
        (PIPE_ACCESS_OUTBOUND, GENERIC_READ)
    } else {
        (PIPE_ACCESS_INBOUND, GENERIC_WRITE)
    };
    // SAFETY: name and security attributes outlive the call.
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            server_access | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            &attributes,
        )
    };
    if server.is_null() || server == INVALID_HANDLE {
        return Err(last_error("CreateNamedPipeW auxiliary"));
    }
    // SAFETY: opens the unique client endpoint for this server instance.
    let client = unsafe {
        CreateFileW(
            name.as_ptr(),
            client_access,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if client.is_null() || client == INVALID_HANDLE {
        let error = last_error("open auxiliary pipe client");
        close_raw(server);
        return Err(error);
    }
    // Opening this client before `ConnectNamedPipe` establishes a good connection; calling
    // `ConnectNamedPipe` afterwards only reports `ERROR_PIPE_CONNECTED`. Do not create an
    // overlapped connect operation: this server is already ready for Tokio I/O.
    if let Err(error) = set_inheritable(client) {
        close_raw(client);
        close_raw(server);
        return Err(error);
    }
    // Explicitly preserve parent-side non-inheritance even if a caller changed
    // process defaults before this launch.
    if unsafe { SetHandleInformation(server, HANDLE_FLAG_INHERIT, 0) } == 0 {
        let error = last_error("clear auxiliary server inheritance");
        close_raw(client);
        close_raw(server);
        return Err(error);
    }
    // SAFETY: both handles were just created and ownership moves into RAII wrappers.
    Ok((unsafe { OwnedHandle::from_raw_handle(server) }, unsafe {
        OwnedHandle::from_raw_handle(client)
    }))
}

// ---------------------------------------------------------------------------------------
// Child
// ---------------------------------------------------------------------------------------

/// A process confined to an AppContainer, living in its own kill-on-close Job Object.
pub(crate) struct ConfinedChild {
    /// Dropped first: closing the job kills survivors before the leases revoke ACEs.
    job: Option<WindowsJob>,
    process: *mut c_void,
    thread: *mut c_void,
    pid: u32,
    _leases: Arc<LeaseSet>,
}

// SAFETY: owned kernel handles; Windows permits waiting on, terminating and closing them from
// any thread, and the value is never cloned.
unsafe impl Send for ConfinedChild {}

impl ConfinedChild {
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    fn wait_ms(&self, ms: u32) -> io::Result<Option<ExitStatus>> {
        // SAFETY: live process handle owned by self.
        match unsafe { WaitForSingleObject(HANDLE(self.process), ms) }.0 {
            WAIT_OBJECT_0 => {
                let mut code = 0u32;
                // SAFETY: same handle; valid out-param.
                unsafe { GetExitCodeProcess(HANDLE(self.process), &mut code) }
                    .map_err(io::Error::other)?;
                Ok(Some(ExitStatus::from_raw(code)))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(last_error("wait for confined child")),
        }
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.wait_ms(0)
    }

    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        self.wait_ms(u32::MAX)?
            .ok_or_else(|| io::Error::other("infinite wait timed out"))
    }

    pub(crate) fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let ms = left.as_millis().min(u128::from(u32::MAX - 1)) as u32;
            match self.wait_ms(ms)? {
                Some(status) => return Ok(Some(status)),
                None if Instant::now() >= deadline => return Ok(None),
                None => {}
            }
        }
    }

    /// Observe the leader's exit code without reaping or releasing AppContainer leases.
    /// `Ok(None)` while it still runs; raw NTSTATUS-style codes survive as `u32`.
    pub(crate) fn observed_exit_code(&self) -> io::Result<Option<u32>> {
        Ok(self
            .wait_ms(0)?
            .map(|status| status.code().unwrap_or(0) as u32))
    }

    /// Observe leader exit without reaping or releasing AppContainer leases.
    pub(crate) fn has_exited(&self) -> io::Result<bool> {
        Ok(self.wait_ms(0)?.is_some())
    }

    /// Terminate the retained Job even when its leader has already exited, then
    /// wait no longer than `timeout`. A timeout retains this child and its leases.
    pub(crate) fn terminate_tree_bounded(
        &mut self,
        timeout: Duration,
    ) -> io::Result<rightkit_process::ReapOutcome> {
        let started = Instant::now();
        if let Some(job) = &self.job {
            job.terminate()?;
        } else if !self.has_exited()? {
            // SAFETY: live process handle owned by self.
            unsafe { TerminateProcess(HANDLE(self.process), 1) }.map_err(io::Error::other)?;
        }
        let Some(status) = self.wait_timeout(timeout.saturating_sub(started.elapsed()))? else {
            return Ok(rightkit_process::ReapOutcome::TimedOut);
        };
        let empty = match &self.job {
            Some(job) => job.wait_empty(timeout.saturating_sub(started.elapsed()))?,
            None => true,
        };
        if empty {
            Ok(rightkit_process::ReapOutcome::Exited(status))
        } else {
            Ok(rightkit_process::ReapOutcome::TimedOut)
        }
    }

    /// Kill the child and, through its Job Object, every descendant.
    pub(crate) fn terminate_tree(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.try_wait()? {
            if let Some(job) = &self.job {
                let _ = job.terminate();
            }
            return Ok(status);
        }
        let killed = self.job.as_ref().is_some_and(|job| job.terminate().is_ok());
        if !killed {
            // SAFETY: live process handle owned by self.
            unsafe { TerminateProcess(HANDLE(self.process), 1) }.map_err(io::Error::other)?;
        }
        self.wait()
    }
}

impl Drop for ConfinedChild {
    fn drop(&mut self) {
        let unassigned = self.job.is_none();
        if let Some(job) = self.job.take() {
            let _ = job.terminate();
        } else if !self.process.is_null() && self.process != INVALID_HANDLE {
            // Job creation or assignment can fail while CreateProcessW left this child
            // suspended. Kill that owned process before its AppContainer leases release.
            unsafe {
                let _ = TerminateProcess(HANDLE(self.process), 1);
            }
        }
        let exited = self.wait_ms(1000).ok().flatten().is_some();
        if unassigned && !exited {
            // Failed Job setup cannot release AppContainer leases while its
            // suspended child is unresolved. Retain native handles + leases
            // for explicit failure-path recovery without blocking this drop.
            std::mem::forget((
                std::mem::replace(&mut self.process, std::ptr::null_mut()),
                std::mem::replace(&mut self.thread, std::ptr::null_mut()),
                std::mem::take(&mut self._leases),
            ));
            return;
        }
        for handle in [self.thread, self.process] {
            if !handle.is_null() && handle != INVALID_HANDLE {
                // SAFETY: from CreateProcessW; closed once.
                unsafe { CloseHandle(handle) };
            }
        }
    }
}

pub(crate) struct ConfinedSpawn {
    pub(crate) child: ConfinedChild,
    pub(crate) stdout: std::fs::File,
    pub(crate) stderr: std::fs::File,
}

pub(crate) struct ConfinedPipeSpawn {
    pub(crate) child: ConfinedChild,
    pub(crate) stdout: Option<std::fs::File>,
    pub(crate) stderr: Option<std::fs::File>,
    pub(crate) to_child: OwnedHandle,
    pub(crate) from_child: OwnedHandle,
}

struct ConfinedLaunch {
    child: ConfinedChild,
    stdout: Option<std::fs::File>,
    stderr: Option<std::fs::File>,
    auxiliary: Option<(OwnedHandle, OwnedHandle)>,
}

struct SuspendedLaunchParams<'a> {
    program: &'a Path,
    command_line: &'a str,
    cwd: &'a Path,
    environment: &'a [u16],
    network: bool,
    named_capabilities: &'a [String],
    stdio: [RawHandle; 3],
    auxiliary: &'a [RawHandle],
}

/// The policy's grants as canonical existing paths: (path, writable). Missing paths fail
/// closed (an ACE needs an object), and so does a filesystem root.
fn canonical_grants(policy: &SandboxPolicy) -> Result<Vec<(PathBuf, bool)>, SandboxError> {
    let mut grants: Vec<(PathBuf, bool)> = Vec::new();
    let all = policy.write_paths.iter().map(|p| (p, true)).chain(
        policy
            .read_paths
            .iter()
            .chain(&policy.exec_paths)
            .map(|p| (p, false)),
    );
    for (path, writable) in all {
        let canonical = std::fs::canonicalize(path)
            .map(|p| plain_path(&p))
            .map_err(|e| {
                SandboxError::InvalidPolicy(format!(
                    "granted path {} does not resolve: {e}",
                    path.display()
                ))
            })?;
        if canonical.parent().is_none() {
            return Err(SandboxError::InvalidPolicy(format!(
                "refusing to grant a filesystem root: {}",
                path.display()
            )));
        }
        let key = lexical_path_key(&canonical);
        match grants.iter_mut().find(|(p, _)| lexical_path_key(p) == key) {
            Some(existing) => existing.1 |= writable,
            None => grants.push((canonical, writable)),
        }
    }
    Ok(grants)
}

/// Translate the requested cwd onto the launch alias of the granted directory containing it
/// (the longest match). A cwd outside every grant is refused: the child could not use it.
fn confined_cwd(requested: &Path, grants: &[(PathBuf, bool)]) -> Result<PathBuf, SandboxError> {
    let resolved = std::fs::canonicalize(requested).map(|p| plain_path(&p))?;
    let key = lexical_path_key(&resolved);
    let root = grants
        .iter()
        .map(|(p, _)| p)
        .filter(|p| p.is_dir())
        .filter(|p| {
            let root = lexical_path_key(p);
            key == root || key.starts_with(&format!("{root}\\"))
        })
        .max_by_key(|p| p.as_os_str().len())
        .ok_or_else(|| {
            SandboxError::InvalidPolicy(format!(
                "cwd {} lies outside every granted path",
                requested.display()
            ))
        })?;
    let relative = resolved
        .strip_prefix(root)
        .map_err(|_| SandboxError::InvalidPolicy("cwd does not match its grant".into()))?;
    let alias = ensure_alias(root)
        .map_err(|e| step_error(format!("prepare launch alias for {}", root.display()), e))?;
    let candidate = alias.join(relative);
    let checked = std::fs::canonicalize(&candidate).map(|p| plain_path(&p))?;
    if lexical_path_key(&checked) != key {
        return Err(SandboxError::InvalidPolicy(format!(
            "cwd {} resolved to {} through its alias",
            requested.display(),
            checked.display()
        )));
    }
    Ok(candidate)
}

/// How long a spawn waits for another process to finish creating or deleting the same profile.
const PROFILE_GUARD_WAIT_MS: u32 = 30_000;

/// Why [`remove_unused_appcontainer_profiles`] left a profile in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileKept {
    /// A process holds it: a confined child of that process may be running in it.
    InUse,
    /// Another process was creating or deleting it at that moment.
    Busy,
    /// Its lock or its in-use mark could not be created or read, so it was not proved unused.
    CouldNotProveUnused(String),
    /// Windows refused the deletion; the text is the HRESULT.
    DeleteFailed(String),
}

/// What [`remove_unused_appcontainer_profiles`] did, by profile name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileSweepReport {
    pub removed: Vec<String>,
    pub kept: Vec<(String, ProfileKept)>,
}

/// Delete this user's `rightkit.sandbox.*` AppContainer profiles that no process is using.
///
/// A spawn creates or adopts one profile per distinct set of grants and never deletes it, so
/// that the next spawn with the same grants reuses it. Until an application calls this, those
/// profiles accumulate: one registry mapping and one folder under the user's local application
/// data for every grant set ever used.
///
/// Only a profile this crate named is considered (the exact name format, and a mapping equal to
/// the SID that name derives to). One is deleted only when no process holds it: every spawn
/// marks its profile in use before creating or adopting it, the mark lasts as long as the
/// child's leases and disappears when the holding process exits. The check and the deletion
/// happen under a per-profile lock that spawns take too, so a spawn in another process either
/// finishes first (the profile is kept) or starts after (it creates the profile afresh). It is
/// safe to call at application start and while other RightKit processes run, and two calls at
/// once do not fail: one of them reports the profile as busy or already gone.
///
/// The lock and the mark are in the machine-wide object namespace and carry the user's SID, so a
/// process of the same user in another logon session is seen. A profile whose lock or mark
/// cannot be created or read is kept and reported as [`ProfileKept::CouldNotProveUnused`].
///
/// Order. Run [`sweep_stale_appcontainer_ancestor_aces`] for the paths you grant *before* this
/// call. A deleted profile's SID is no longer recognised as this crate's, so the sweep run
/// afterwards is harmless but leaves the entries a crashed process left on those paths for it.
pub fn remove_unused_appcontainer_profiles() -> Result<ProfileSweepReport, SandboxError> {
    let mut report = ProfileSweepReport::default();
    for (name, _sid) in ancestors::owned_profiles(PROFILE_PREFIX)? {
        // Anything that stops the proof keeps the profile: a failure here never deletes.
        let _guard = match ancestors::ProfileGuard::acquire(&name, 0) {
            Ok(Some(guard)) => guard,
            Ok(None) => {
                report.kept.push((name, ProfileKept::Busy));
                continue;
            }
            Err(error) => {
                let why = ProfileKept::CouldNotProveUnused(format!("lock: {error}"));
                report.kept.push((name, why));
                continue;
            }
        };
        match ancestors::profile_holder_exists(&name) {
            Ok(false) => {}
            Ok(true) => {
                report.kept.push((name, ProfileKept::InUse));
                continue;
            }
            Err(error) => {
                let why = ProfileKept::CouldNotProveUnused(format!("in-use mark: {error}"));
                report.kept.push((name, why));
                continue;
            }
        }
        let wide_name = wide(&name);
        // SAFETY: a NUL-terminated name that outlives the call.
        let result = unsafe { DeleteAppContainerProfile(wide_name.as_ptr()) };
        if result >= 0 {
            report.removed.push(name);
        } else {
            report
                .kept
                .push((name, ProfileKept::DeleteFailed(format!("{result:#010x}"))));
        }
    }
    Ok(report)
}

/// The profile name for a sorted grant key: stable for one set of grants, so the profile is reused.
fn profile_name(sorted_key: &[String]) -> String {
    format!("{PROFILE_PREFIX}{:016x}", fnv1a(&sorted_key.join("|")))
}

/// Test support: the name of the AppContainer profile a spawn with `policy` registers for the
/// current user. The granted paths must exist, as they must for the spawn itself.
#[doc(hidden)]
pub fn appcontainer_profile_name_for_test(policy: &SandboxPolicy) -> Result<String, SandboxError> {
    let mut key: Vec<String> = canonical_grants(policy)?
        .iter()
        .map(|(p, w)| format!("{}{}", if *w { "w:" } else { "r:" }, lexical_path_key(p)))
        .collect();
    key.sort();
    Ok(profile_name(&key))
}

/// Test support: delete the named AppContainer profile of the current user, so a test run leaves
/// none behind. Only a name this crate could have made is accepted. Returns whether Windows
/// reported the profile deleted; a profile that does not exist is not an error.
///
/// The crate itself never deletes a profile: a spawn creates or adopts one per distinct set of
/// grants and leaves it registered.
#[doc(hidden)]
pub fn delete_appcontainer_profile_for_test(name: &str) -> io::Result<bool> {
    let hash = name.strip_prefix(PROFILE_PREFIX).unwrap_or("");
    if hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{name}` is not a {PROFILE_PREFIX}<16 hex digits> profile name"),
        ));
    }
    let wide_name = wide(name);
    // SAFETY: a NUL-terminated name that outlives the call.
    let result = unsafe { DeleteAppContainerProfile(wide_name.as_ptr()) };
    Ok(result >= 0)
}

/// Spawn `command` confined to `policy`: suspended, assigned to a kill-on-close job, resumed.
pub(crate) fn spawn(
    policy: &SandboxPolicy,
    command: &SandboxCommand,
) -> Result<ConfinedSpawn, SandboxError> {
    let launched = spawn_inner(policy, command, None, None)?;
    let stdout = launched.stdout.ok_or_else(|| {
        SandboxError::InvalidPolicy("ordinary confined spawn did not create stdout capture".into())
    })?;
    let stderr = launched.stderr.ok_or_else(|| {
        SandboxError::InvalidPolicy("ordinary confined spawn did not create stderr capture".into())
    })?;
    Ok(ConfinedSpawn {
        child: launched.child,
        stdout,
        stderr,
    })
}

/// Spawn `command` with the two auxiliary client endpoints requested by
/// `handle_flag`; parent server endpoints remain outside the child handle list.
pub(crate) fn spawn_with_pipe_pair(
    policy: &SandboxPolicy,
    command: &SandboxCommand,
    handle_flag: &str,
    stdio_file: Option<&Path>,
) -> Result<ConfinedPipeSpawn, SandboxError> {
    if policy.network != NetworkPolicy::Deny {
        return Err(SandboxError::InvalidPolicy(
            "pipe-pair sandbox requires NetworkPolicy::Deny".into(),
        ));
    }
    let launched = spawn_inner(policy, command, Some(handle_flag), stdio_file)?;
    let (to_child, from_child) = launched.auxiliary.ok_or_else(|| {
        SandboxError::InvalidPolicy("pipe-pair launch did not create auxiliary handles".into())
    })?;
    Ok(ConfinedPipeSpawn {
        child: launched.child,
        stdout: launched.stdout,
        stderr: launched.stderr,
        to_child,
        from_child,
    })
}

fn spawn_inner(
    policy: &SandboxPolicy,
    command: &SandboxCommand,
    handle_flag: Option<&str>,
    stdio_file: Option<&Path>,
) -> Result<ConfinedLaunch, SandboxError> {
    // Launch and lease the real image: version managers (nvm-windows, volta, scoop) expose
    // tools through links whose link objects the container cannot use and the user cannot
    // re-ACL.
    let program = resolve_program(&command.program)?;
    let program = std::fs::canonicalize(&program)
        .map(|p| plain_path(&p))
        .unwrap_or(program);
    let grants = canonical_grants(policy)?;
    let mut key: Vec<String> = grants
        .iter()
        .map(|(p, w)| format!("{}{}", if *w { "w:" } else { "r:" }, lexical_path_key(p)))
        .collect();
    key.sort();
    let name = profile_name(&key);
    let profile = {
        // Held while the profile is marked in use and created or adopted, so
        // `remove_unused_appcontainer_profiles` in another process cannot delete it in between.
        let _guard =
            ancestors::ProfileGuard::acquire(&name, PROFILE_GUARD_WAIT_MS)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("AppContainer profile {name} stayed busy in another process"),
                )
            })?;
        let holder = ancestors::ProfileHolder::create(&name)?;
        let profile = AppContainerProfile::ensure(&name)?;
        *profile
            .leases
            .holder
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(holder);
        profile
    };
    for (path, _) in &grants {
        let lease = AncestorLease::acquire(path, &profile.sid_text)?;
        profile
            .leases
            .ancestors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(lease);
    }
    for (path, writable) in &grants {
        profile
            .lease(path, *writable)
            .map_err(|e| step_error(format!("lease {}", path.display()), e))?;
    }
    profile.lease_image_dir(&program)?;
    let folder = profile.folder_path()?;
    let cwd = match &command.cwd {
        Some(requested) => confined_cwd(requested, &grants)?,
        None => folder.clone(),
    };
    let environment = confined_environment(&cwd, &folder, &policy.env, &command.env);

    let security = PipeSecurity::new(&profile.sid_text)?;
    let stdin = std::fs::OpenOptions::new()
        .read(true)
        .open("NUL")?
        .into_raw_handle();
    let (stdout, stdout_child, stderr, stderr_child) = if handle_flag.is_some() {
        // Pipe-pair callers drain Chrome DevTools I/O themselves. Keep stdio on NUL (or on an
        // append-mode diagnostic file the caller named) so this path has no reader threads
        // whose ownership could outlive cancellation. The parent opens the file, so the
        // child needs no ACL on it; the handle rides the explicit inherit list.
        let open_sink = || -> io::Result<RawHandle> {
            let file = match stdio_file {
                Some(path) => std::fs::OpenOptions::new().append(true).open(path)?,
                None => std::fs::OpenOptions::new().write(true).open("NUL")?,
            };
            Ok(file.into_raw_handle())
        };
        let stdout_child = match open_sink() {
            Ok(handle) => handle,
            Err(error) => {
                close_raw(stdin);
                return Err(error.into());
            }
        };
        let stderr_child = match open_sink() {
            Ok(handle) => handle,
            Err(error) => {
                close_raw(stdin);
                close_raw(stdout_child);
                return Err(error.into());
            }
        };
        (None, stdout_child, None, stderr_child)
    } else {
        let (stdout, stdout_child) = match output_pipe(&security) {
            Ok(pair) => pair,
            Err(error) => {
                close_raw(stdin);
                return Err(error.into());
            }
        };
        let (stderr, stderr_child) = match output_pipe(&security) {
            Ok(pair) => pair,
            Err(error) => {
                close_raw(stdin);
                close_raw(stdout_child);
                return Err(error.into());
            }
        };
        (Some(stdout), stdout_child, Some(stderr), stderr_child)
    };
    let child_handles = [stdin, stdout_child, stderr_child];
    let auxiliary = if handle_flag.is_some() {
        let (to_child, child_read) = match auxiliary_pipe(&security, true) {
            Ok(pair) => pair,
            Err(error) => {
                for handle in child_handles {
                    close_raw(handle);
                }
                return Err(error.into());
            }
        };
        let (from_child, child_write) = match auxiliary_pipe(&security, false) {
            Ok(pair) => pair,
            Err(error) => {
                for handle in child_handles {
                    close_raw(handle);
                }
                return Err(error.into());
            }
        };
        Some((to_child, child_read, from_child, child_write))
    } else {
        None
    };
    let mut command_line = format!("\"{}\"", program.display());
    for arg in &command.args {
        command_line.push(' ');
        append_quoted_arg(&mut command_line, &arg.to_string_lossy());
    }
    if let (Some(handle_flag), Some((_, child_read, _, child_write))) =
        (handle_flag, auxiliary.as_ref())
    {
        let read = match u32::try_from(child_read.as_raw_handle() as usize) {
            Ok(handle) => handle,
            Err(_) => {
                for handle in child_handles {
                    close_raw(handle);
                }
                return Err(SandboxError::InvalidPolicy(
                    "child read pipe handle does not fit in u32".into(),
                ));
            }
        };
        let write = match u32::try_from(child_write.as_raw_handle() as usize) {
            Ok(handle) => handle,
            Err(_) => {
                for handle in child_handles {
                    close_raw(handle);
                }
                return Err(SandboxError::InvalidPolicy(
                    "child write pipe handle does not fit in u32".into(),
                ));
            }
        };
        command_line.push(' ');
        append_quoted_arg(&mut command_line, &format!("{handle_flag}={read},{write}"));
    }
    let auxiliary_child_handles: Vec<RawHandle> = auxiliary
        .as_ref()
        .map(|(_, read, _, write)| vec![read.as_raw_handle(), write.as_raw_handle()])
        .unwrap_or_default();
    let launched = (|| {
        set_inheritable(stdin)?;
        set_inheritable(stdout_child)?;
        set_inheritable(stderr_child)?;
        create_suspended(
            &profile,
            SuspendedLaunchParams {
                program: &program,
                command_line: &command_line,
                cwd: &cwd,
                environment: &environment,
                network: policy.network == NetworkPolicy::Allow,
                named_capabilities: &policy.named_capabilities,
                stdio: [stdin, stdout_child, stderr_child],
                auxiliary: &auxiliary_child_handles,
            },
        )
    })();
    for handle in child_handles {
        close_raw(handle);
    }
    let (process, thread, pid) = launched?;
    let mut child = ConfinedChild {
        job: None,
        process,
        thread,
        pid,
        _leases: profile.leases.clone(),
    };
    // Assigned while suspended, so no fast child can spawn descendants outside the job. Any
    // failure before resume kills the process (ConfinedChild's drop) and fails closed.
    let job = WindowsJob::new(Some(DEFAULT_ACTIVE_PROCESS_LIMIT))
        .and_then(|job| job.assign_process_id(pid).map(|()| job))
        .map_err(|e| step_error("assign confined process to its Job Object", e))?;
    child.job = Some(job);
    // SAFETY: primary-thread handle from CreateProcessW.
    if unsafe { ResumeThread(HANDLE(thread)) } == u32::MAX {
        return Err(last_error("resume confined process").into());
    }
    let auxiliary = auxiliary.map(|(to_child, _, from_child, _)| (to_child, from_child));
    Ok(ConfinedLaunch {
        child,
        stdout,
        stderr,
        auxiliary,
    })
}

/// `CreateProcessW` with the AppContainer's `SECURITY_CAPABILITIES` (plus `internetClient`
/// when network is allowed), an explicit inherited-handle list, no window, and suspended.
fn create_suspended(
    profile: &AppContainerProfile,
    params: SuspendedLaunchParams<'_>,
) -> io::Result<(*mut c_void, *mut c_void, u32)> {
    let SuspendedLaunchParams {
        program,
        command_line,
        cwd,
        environment,
        network,
        named_capabilities,
        stdio,
        auxiliary,
    } = params;
    let wprogram = wide(program);
    let mut wcommand = wide(command_line);
    let wcwd = wide(cwd);
    let stdio_handles: [HANDLE; 3] = stdio.map(HANDLE);
    let mut handles = Vec::with_capacity(stdio_handles.len() + auxiliary.len());
    handles.extend(stdio_handles);
    handles.extend(auxiliary.iter().copied().map(HANDLE));

    let mut size = 0usize;
    // SAFETY: the sizing call fails by design and reports the size.
    let _ = unsafe { InitializeProcThreadAttributeList(None, 2, None, &mut size) };
    let mut buffer = vec![0u8; size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
    // SAFETY: `buffer` has the reported size and outlives CreateProcessW.
    unsafe { InitializeProcThreadAttributeList(Some(list), 2, None, &mut size) }
        .map_err(|e| io::Error::other(format!("init attribute list: {e}")))?;
    let result = (|| {
        // `internet` and `capabilities` must outlive CreateProcessW.
        let internet = if network {
            Some(LocalSid::parse(INTERNET_CLIENT_CAPABILITY_SID)?)
        } else {
            None
        };
        crate::policy::validate_named_capabilities(named_capabilities)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let derived = named_capabilities
            .iter()
            .map(|name| DerivedCapabilitySids::derive(name))
            .collect::<io::Result<Vec<_>>>()?;
        let mut capabilities: Vec<SidAndAttributes> = internet
            .iter()
            .map(|sid| sid.0)
            .chain(derived.iter().flat_map(DerivedCapabilitySids::sids))
            .map(|sid| SidAndAttributes {
                sid,
                attributes: SE_GROUP_ENABLED,
            })
            .collect();
        let security = SecurityCapabilities {
            app_container_sid: profile.sid,
            capabilities: if capabilities.is_empty() {
                std::ptr::null_mut()
            } else {
                capabilities.as_mut_ptr()
            },
            capability_count: capabilities.len() as u32,
            reserved: 0,
        };
        // SAFETY: `security` and the SIDs it references outlive CreateProcessW.
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                Some(&security as *const _ as *const c_void),
                std::mem::size_of::<SecurityCapabilities>(),
                None,
                None,
            )
        }
        .map_err(|e| io::Error::other(format!("set AppContainer attribute: {e}")))?;
        // SAFETY: the handle array outlives CreateProcessW; only stdio and explicitly supplied
        // auxiliary client endpoints are inherited.
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                std::mem::size_of_val(handles.as_slice()),
                None,
                None,
            )
        }
        .map_err(|e| io::Error::other(format!("set inherited handle list: {e}")))?;

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = list;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = stdio_handles[0];
        startup.StartupInfo.hStdOutput = stdio_handles[1];
        startup.StartupInfo.hStdError = stdio_handles[2];
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: NUL-terminated, mutable command line; valid in/out structures.
        unsafe {
            CreateProcessW(
                PCWSTR(wprogram.as_ptr()),
                Some(PWSTR(wcommand.as_mut_ptr())),
                None,
                None,
                true,
                // Never a window: without a parent console a console child would open one.
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_UNICODE_ENVIRONMENT
                    | CREATE_NO_WINDOW
                    | CREATE_SUSPENDED,
                Some(environment.as_ptr().cast()),
                PCWSTR(wcwd.as_ptr()),
                &startup.StartupInfo,
                &mut info,
            )
        }
        .map_err(|e| {
            io::Error::other(format!(
                "CreateProcessW in AppContainer {}: {e}",
                profile.name
            ))
        })?;
        Ok((info.hProcess.0, info.hThread.0, info.dwProcessId))
    })();
    // SAFETY: initialized above; no longer needed.
    unsafe { DeleteProcThreadAttributeList(list) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests register profiles for the real user: CI or the repository's opt-in only.
    fn real_user_stores_allowed(test: &str) -> bool {
        let is = |name: &str, value: &str| std::env::var(name).is_ok_and(|v| v == value);
        if is("GITHUB_ACTIONS", "true") || is("RIGHTKIT_TEST_REAL_USER_STORES", "1") {
            return true;
        }
        println!(
            "skipped {test}: it registers AppContainer profiles for the real user; it runs only \
             on CI (GITHUB_ACTIONS=true) or with RIGHTKIT_TEST_REAL_USER_STORES=1"
        );
        false
    }

    /// A profile name in this crate's format that no grant set of another test produces.
    fn scratch_profile(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        profile_name(&[format!("test:{tag}:{}:{nanos}", std::process::id())])
    }

    /// Deletes the named profiles however the test ends, whatever their names.
    struct Profiles(Vec<String>);
    impl Drop for Profiles {
        fn drop(&mut self) {
            for name in &self.0 {
                let wide_name = wide(name);
                // SAFETY: a NUL-terminated name that outlives the call.
                unsafe { DeleteAppContainerProfile(wide_name.as_ptr()) };
            }
        }
    }

    /// A test here registers a profile and only then takes its holder, or takes none at all. A
    /// sweep run by another test of this binary in that gap removes the profile, as it should:
    /// nothing holds it. So the tests that register scratch profiles or sweep run one at a time.
    /// (A real spawn takes the mark inside the profile's lock before it creates the profile.)
    static SWEEP_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn one_sweep_test_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        SWEEP_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn registered(name: &str) -> bool {
        ancestors::registered_profiles()
            .unwrap()
            .iter()
            .any(|(moniker, _)| moniker.eq_ignore_ascii_case(name))
    }

    #[test]
    fn an_unused_profile_is_removed_and_one_with_a_live_holder_is_kept() {
        if !real_user_stores_allowed(
            "an_unused_profile_is_removed_and_one_with_a_live_holder_is_kept",
        ) {
            return;
        }
        let _alone = one_sweep_test_at_a_time();
        let (used, unused) = (scratch_profile("used"), scratch_profile("unused"));
        let _cleanup = Profiles(vec![used.clone(), unused.clone()]);
        let _used = AppContainerProfile::ensure(&used).unwrap();
        let _unused = AppContainerProfile::ensure(&unused).unwrap();
        assert!(registered(&used) && registered(&unused));
        let holder = ancestors::ProfileHolder::create(&used).unwrap();

        // The registry is what is asserted; the report is checked for what only this call can say.
        let report = remove_unused_appcontainer_profiles().unwrap();
        assert!(!report.removed.contains(&used), "{report:?}");
        assert!(
            report.kept.contains(&(used.clone(), ProfileKept::InUse)),
            "{report:?}"
        );
        assert!(
            registered(&used),
            "a profile with a live holder was removed"
        );
        assert!(!registered(&unused), "an unused profile was kept");

        drop(holder);
        remove_unused_appcontainer_profiles().unwrap();
        assert!(
            !registered(&used),
            "the profile stayed after its holder went"
        );
    }

    #[test]
    fn a_profile_this_crate_did_not_name_is_never_touched() {
        if !real_user_stores_allowed("a_profile_this_crate_did_not_name_is_never_touched") {
            return;
        }
        let _alone = one_sweep_test_at_a_time();
        let foreign = format!(
            "rightkit-test.foreign.{:016x}",
            fnv1a(&scratch_profile("x"))
        );
        let _cleanup = Profiles(vec![foreign.clone()]);
        let _profile = AppContainerProfile::ensure(&foreign).unwrap();
        assert!(registered(&foreign));
        let report = remove_unused_appcontainer_profiles().unwrap();
        assert!(!report.removed.contains(&foreign), "{report:?}");
        assert!(!report.kept.iter().any(|(name, _)| name == &foreign));
        assert!(registered(&foreign));
        // The test-support delete refuses it too.
        assert!(delete_appcontainer_profile_for_test(&foreign).is_err());
    }

    #[test]
    fn the_in_use_mark_is_machine_wide_and_names_the_user() {
        // No object is created: only the name is built.
        let name =
            ancestors::profile_object_text("holder", "RightKit.Sandbox.0123456789ABCDEF").unwrap();
        let rest = name
            .strip_prefix(r"Global\rightkit-profile-holder-S-1-")
            .unwrap_or_else(|| panic!("{name}"));
        assert!(
            rest.ends_with("-rightkit.sandbox.0123456789abcdef"),
            "{name}"
        );
        assert_ne!(
            name,
            ancestors::profile_object_text("guard", "rightkit.sandbox.0123456789abcdef").unwrap()
        );
    }

    /// The documented order is the ancestor sweep first, then profile removal. The other order
    /// is harmless: the sweep after a removal fails nothing and removes nothing it does not own.
    /// What it cannot do is clean up after the deleted profile, which the docs say.
    #[test]
    fn sweeping_ancestor_entries_after_removing_profiles_is_harmless() {
        if !real_user_stores_allowed(
            "sweeping_ancestor_entries_after_removing_profiles_is_harmless",
        ) {
            return;
        }
        let _alone = one_sweep_test_at_a_time();
        let name = scratch_profile("order");
        let _cleanup = Profiles(vec![name.clone()]);
        let _ = AppContainerProfile::ensure(&name).unwrap();
        let dir = std::env::temp_dir().join(format!("rk-profile-order-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        remove_unused_appcontainer_profiles().unwrap();
        assert!(!registered(&name));
        let swept = sweep_stale_appcontainer_ancestor_aces(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(swept.unwrap(), 0);
    }

    const SWEEP_CHILD: &str = "RIGHTKIT_SANDBOX_PROFILE_SWEEP_CHILD";

    /// Child half of the two-process test: inert unless driven.
    #[test]
    fn child_removes_unused_profiles() {
        if std::env::var_os(SWEEP_CHILD).is_none() {
            return;
        }
        remove_unused_appcontainer_profiles().expect("a concurrent sweep must not fail");
    }

    #[test]
    fn two_processes_removing_at_once_do_not_fail() {
        if !real_user_stores_allowed("two_processes_removing_at_once_do_not_fail") {
            return;
        }
        let _alone = one_sweep_test_at_a_time();
        let names: Vec<String> = (0..6)
            .map(|i| scratch_profile(&format!("both{i}")))
            .collect();
        let _cleanup = Profiles(names.clone());
        for name in &names {
            let _ = AppContainerProfile::ensure(name).unwrap();
        }
        let children: Vec<_> = (0..2)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "windows_appcontainer::tests::child_removes_unused_profiles",
                        "--nocapture",
                    ])
                    .env(SWEEP_CHILD, "1")
                    .spawn()
                    .unwrap()
            })
            .collect();
        for mut child in children {
            assert!(child.wait().unwrap().success(), "a sweeping process failed");
        }
        for name in &names {
            assert!(!registered(name), "{name} is still registered");
        }
    }

    #[test]
    fn arguments_quote_only_when_needed() {
        let mut out = String::new();
        for arg in ["/c", "", "a b", r#"say "hi""#, r"C:\dir\"] {
            out.push(' ');
            append_quoted_arg(&mut out, arg);
        }
        assert_eq!(out, r#" /c "" "a b" "say \"hi\"" C:\dir\"#);
        let mut trailing = String::new();
        append_quoted_arg(&mut trailing, r"C:\a dir\");
        assert_eq!(trailing, r#""C:\a dir\\""#);
    }

    #[test]
    fn ace_coverage_requires_bits_scope_and_propagation() {
        let read = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
        let tree = SUB_CONTAINERS_AND_OBJECTS_INHERIT;
        let scope = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE;
        assert!(ace_covers_grant(scope, read, read, tree));
        assert!(!ace_covers_grant(scope, FILE_GENERIC_READ, read, tree));
        assert!(!ace_covers_grant(0, read, read, tree));
        assert!(!ace_covers_grant(
            scope | INHERIT_ONLY_ACE,
            read,
            read,
            tree
        ));
        assert!(!ace_covers_grant(
            scope | NO_PROPAGATE_INHERIT_ACE,
            read,
            read,
            tree
        ));
        assert!(ace_covers_grant(0, read, read, 0));
    }

    #[test]
    fn environment_block_is_sorted_scrubbed_and_container_rooted() {
        let mut policy = EnvPolicy::default();
        policy.set.insert("RK_SET".into(), "1".into());
        let folder = Path::new(r"C:\Users\x\AppData\Local\Packages\p\AC");
        let block = confined_environment(Path::new(r"D:\work"), folder, &policy, &[]);
        let text = String::from_utf16_lossy(&block);
        let entries: Vec<&str> = text.split('\0').filter(|e| !e.is_empty()).collect();
        let keys: Vec<String> = entries
            .iter()
            .map(|e| e[..e[1..].find('=').unwrap() + 1].to_ascii_uppercase())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "Windows needs a sorted block");
        assert!(entries.contains(&r"=D:=D:\work"));
        assert!(entries.contains(&r"TEMP=C:\Users\x\AppData\Local\Packages\p\AC\Temp"));
        assert!(entries.contains(&"RK_SET=1"));
        assert!(entries
            .iter()
            .any(|e| e.starts_with("NODE_OPTIONS=--preserve-symlinks")));
    }

    #[test]
    fn package_readable_dirs_are_detected_from_the_live_dacl() {
        let system32 = PathBuf::from(system_root()).join("System32");
        assert!(packages_can_read(&system32));
        let private = std::env::temp_dir().join(format!("rk-private-{}", std::process::id()));
        std::fs::create_dir_all(&private).unwrap();
        assert!(!packages_can_read(&private));
        std::fs::remove_dir_all(&private).unwrap();
    }

    #[test]
    fn huge_image_dirs_lease_only_the_image() {
        let base = std::env::temp_dir().join(format!("rk-image-lease-{}", std::process::id()));
        let (small, large) = (base.join("small"), base.join("large"));
        std::fs::create_dir_all(&small).unwrap();
        std::fs::create_dir_all(&large).unwrap();
        for i in 0..=IMAGE_DIR_LEASE_MAX_ENTRIES {
            std::fs::File::create(large.join(format!("f{i}"))).unwrap();
        }
        let (small_exe, large_exe) = (small.join("tool.exe"), large.join("f0"));
        std::fs::File::create(&small_exe).unwrap();
        assert_eq!(image_lease_target(&small_exe, &small), small);
        assert_eq!(image_lease_target(&large_exe, &large), large_exe);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn junction_aliases_its_target() {
        let base = std::env::temp_dir().join(format!("rk-junction-{}", std::process::id()));
        let target = base.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("f.txt"), "through").unwrap();
        let link = base.join("link");
        create_junction(&link, &plain_path(&std::fs::canonicalize(&target).unwrap())).unwrap();
        assert_eq!(
            std::fs::read_to_string(link.join("f.txt")).unwrap(),
            "through"
        );
        std::fs::remove_dir(&link).unwrap();
        std::fs::remove_dir_all(&base).unwrap();
    }
}
