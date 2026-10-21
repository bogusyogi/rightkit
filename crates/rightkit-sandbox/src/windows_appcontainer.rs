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

use crate::deny_read::{expand_glob, plan_deny_read_paths, GLOB_EXPANSION_LIMIT};
use crate::windows_job::{WindowsJob, DEFAULT_ACTIVE_PROCESS_LIMIT};
use crate::{
    ConfinementMode, DenyReadEntry, EnvPolicy, LoopbackPolicy, NetworkPolicy, SandboxCommand,
    SandboxError, SandboxPolicy,
};
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
const DENY_ACCESS: i32 = 3;
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
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn GetProcessHeap() -> *mut c_void;
    fn HeapFree(heap: *mut c_void, flags: u32, memory: *mut c_void) -> i32;
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

/// Whether a lease allows access or carves it out of a wider allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseKind {
    Allow,
    Deny,
}

/// Everything a deny entry takes away: read, write, execute and delete.
const DENY_PERMISSIONS: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;

/// One per-command grant for a container SID, an allow or a deny carve-out; dropping the last
/// lease on a key revokes the ACE, so grants never outlive the commands that needed them.
struct GrantLease {
    key: String,
    kind: LeaseKind,
    writable: bool,
    sid_text: String,
    path: PathBuf,
}

/// Registry key of one lease. Allow keys keep the `<sid>|<path>` form the ancestor sweep reads
/// to tell a live grant from a stale one; deny keys carry a prefix so they never collide with it.
fn lease_key(kind: LeaseKind, sid_text: &str, path: &Path) -> String {
    match kind {
        LeaseKind::Allow => format!("{sid_text}|{}", lexical_path_key(path)),
        LeaseKind::Deny => format!("deny|{sid_text}|{}", lexical_path_key(path)),
    }
}

impl GrantLease {
    fn acquire(sid: *mut c_void, sid_text: &str, path: &Path, writable: bool) -> io::Result<Self> {
        Self::acquire_kind(LeaseKind::Allow, sid, sid_text, path, writable)
    }

    /// An explicit DENY ACE for the container on `path` (and below it, for a directory).
    fn deny(sid: *mut c_void, sid_text: &str, path: &Path) -> io::Result<Self> {
        Self::acquire_kind(LeaseKind::Deny, sid, sid_text, path, false)
    }

    fn acquire_kind(
        kind: LeaseKind,
        sid: *mut c_void,
        sid_text: &str,
        path: &Path,
        writable: bool,
    ) -> io::Result<Self> {
        let key = lease_key(kind, sid_text, path);
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
                if kind == LeaseKind::Allow && writable && !entry.writable {
                    grant(true)?;
                    entry.writable = true;
                }
                entry.count += 1;
            }
            None => {
                match kind {
                    LeaseKind::Allow => {
                        // No live lease owns the key, so a write-capable ACE here is stale:
                        // remove it before granting narrower read/execute access.
                        if !writable && path_has_write_grant(path, sid) {
                            apply_sid_path_entry(
                                path,
                                sid,
                                REVOKE_ACCESS,
                                0,
                                inheritance_for(path),
                            )
                            .map_err(|e| {
                                step_error(
                                    format!("revoke stale write ACE on {}", path.display()),
                                    e,
                                )
                            })?;
                        }
                        grant(writable)?;
                    }
                    LeaseKind::Deny => {
                        apply_sid_path_entry(
                            path,
                            sid,
                            DENY_ACCESS,
                            DENY_PERMISSIONS,
                            inheritance_for(path),
                        )
                        .map_err(|e| step_error(format!("deny ACE on {}", path.display()), e))?;
                    }
                }
                registry.insert(key.clone(), LeaseEntry { count: 1, writable });
            }
        }
        Ok(Self {
            key,
            kind,
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
        // same key re-checks the live DACL (and strips a stale write grant). A stale deny only
        // ever narrows this container's access.
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
    /// The deepest ancestor of each leased path the container could not be given traverse
    /// access to (best-effort leases only). Exported to children as `GIT_CEILING_DIRECTORIES` so
    /// upward repository discovery stops before it.
    git_ceilings: Mutex<Vec<PathBuf>>,
    /// The loopback exemption of a network-enabled container; removed with the last holder.
    loopback: Mutex<Option<LoopbackLease>>,
}

// ---------------------------------------------------------------------------------------
// Loopback exemption
// ---------------------------------------------------------------------------------------

/// `NetworkIsolation*AppContainerConfig` entry points, resolved at run time so the crate has no
/// link-time dependency on `FirewallAPI.dll`.
/// `LOAD_LIBRARY_SEARCH_SYSTEM32` (documented value 0x00000800): resolve only from System32, never
/// the application directory or the current directory, so a planted DLL cannot be loaded.
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

struct LoopbackApi {
    get: unsafe extern "system" fn(*mut u32, *mut *mut SidAndAttributes) -> u32,
    set: unsafe extern "system" fn(u32, *mut SidAndAttributes) -> u32,
}

fn loopback_api() -> io::Result<&'static LoopbackApi> {
    static API: OnceLock<Result<LoopbackApi, String>> = OnceLock::new();
    API.get_or_init(|| {
        let name = wide("FirewallAPI.dll");
        // SAFETY: `name` is NUL-terminated; the module stays loaded for the life of the process.
        let module = unsafe {
            LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(format!(
                "load FirewallAPI.dll: {}",
                io::Error::last_os_error()
            ));
        }
        // SAFETY: `module` is a live module handle and each name is a NUL-terminated export.
        let (get, set) = unsafe {
            (
                GetProcAddress(
                    module,
                    c"NetworkIsolationGetAppContainerConfig".as_ptr().cast(),
                ),
                GetProcAddress(
                    module,
                    c"NetworkIsolationSetAppContainerConfig".as_ptr().cast(),
                ),
            )
        };
        if get.is_null() || set.is_null() {
            return Err("FirewallAPI.dll lacks the NetworkIsolation AppContainer exports".into());
        }
        // SAFETY: the exports have exactly these signatures (netfw.h).
        Ok(unsafe {
            LoopbackApi {
                get: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "system" fn(*mut u32, *mut *mut SidAndAttributes) -> u32,
                >(get),
                set: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "system" fn(u32, *mut SidAndAttributes) -> u32,
                >(set),
            }
        })
    })
    .as_ref()
    .map_err(|message| io::Error::other(message.clone()))
}

/// Add (`add`) or remove the container from the machine's loopback-exempt list. Windows blocks
/// loopback for every AppContainer, and `internetClient` only opens the external network, so a
/// network-enabled child that must reach a local service needs this exemption. The list is
/// replaced wholesale by the API, so the current one is read, merged and written back. Writing it
/// needs elevation: an unelevated caller gets `ERROR_ACCESS_DENIED` and the container simply has
/// no loopback (never more access than before).
fn update_loopback_exemption(sid: *mut c_void, add: bool) -> io::Result<()> {
    let api = loopback_api()?;
    let mut count = 0u32;
    let mut list: *mut SidAndAttributes = std::ptr::null_mut();
    // SAFETY: valid out-pointers; success yields a process-heap array released below.
    let status = unsafe { (api.get)(&mut count, &mut list) };
    if status != 0 {
        return Err(step_error(
            "read the loopback exemption list",
            io::Error::from_raw_os_error(status as i32),
        ));
    }
    let existing: &[SidAndAttributes] = if list.is_null() || count == 0 {
        &[]
    } else {
        // SAFETY: the API returned `count` contiguous entries at `list`.
        unsafe { std::slice::from_raw_parts(list, count as usize) }
    };
    let mut merged: Vec<SidAndAttributes> = Vec::with_capacity(existing.len() + 1);
    let mut present = false;
    for entry in existing {
        // SAFETY: both SIDs are valid for the duration of the call.
        if unsafe { EqualSid(entry.sid, sid) } != 0 {
            present = true;
            if !add {
                continue;
            }
        }
        merged.push(SidAndAttributes {
            sid: entry.sid,
            attributes: entry.attributes,
        });
    }
    if add && !present {
        merged.push(SidAndAttributes { sid, attributes: 0 });
    }
    let result = if add == present {
        // Already in the requested state.
        Ok(())
    } else {
        // SAFETY: `merged` holds `len` valid entries whose SIDs outlive the call (API-owned
        // until freed below, or `sid`).
        let status = unsafe { (api.set)(merged.len() as u32, merged.as_mut_ptr()) };
        if status == 0 {
            Ok(())
        } else {
            Err(step_error(
                if add {
                    "add the container to the loopback exemption list"
                } else {
                    "remove the container from the loopback exemption list"
                },
                io::Error::from_raw_os_error(status as i32),
            ))
        }
    };
    if !list.is_null() {
        // The Get call allocates the array and every SID on the process heap and the caller
        // frees each with HeapFree (NetworkIsolationFreeAppContainers belongs to the differently
        // shaped Enum result and corrupts the heap here).
        // SAFETY: `list` holds `count` entries from that call; each SID and the array are freed
        // exactly once, after their last use.
        unsafe {
            let heap = GetProcessHeap();
            for entry in std::slice::from_raw_parts(list, count as usize) {
                if !entry.sid.is_null() {
                    HeapFree(heap, 0, entry.sid);
                }
            }
            HeapFree(heap, 0, list.cast());
        }
    }
    result
}

/// Process-wide reference counts of live loopback exemptions, and the containers whose exemption
/// could not be established (not retried: the failure is environmental, usually no elevation).
#[derive(Default)]
struct LoopbackRegistry {
    live: HashMap<String, usize>,
    failed: std::collections::HashSet<String>,
}

fn loopback_registry() -> &'static Mutex<LoopbackRegistry> {
    static REGISTRY: OnceLock<Mutex<LoopbackRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// A loopback exemption held for the lifetime of a profile's commands and removed when the last
/// holder in the process ends, like every other grant.
struct LoopbackLease {
    sid_text: String,
}

impl LoopbackLease {
    /// `Ok(None)`: an earlier attempt for this container failed, so none is made again.
    fn acquire(sid: *mut c_void, sid_text: &str) -> io::Result<Option<Self>> {
        let mut registry = loopback_registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if registry.failed.contains(sid_text) {
            return Ok(None);
        }
        let lease = || Self {
            sid_text: sid_text.to_string(),
        };
        if let Some(count) = registry.live.get_mut(sid_text) {
            *count += 1;
            return Ok(Some(lease()));
        }
        if let Err(error) = update_loopback_exemption(sid, true) {
            registry.failed.insert(sid_text.to_string());
            return Err(error);
        }
        registry.live.insert(sid_text.to_string(), 1);
        Ok(Some(lease()))
    }
}

impl Drop for LoopbackLease {
    fn drop(&mut self) {
        let mut registry = loopback_registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(count) = registry.live.get_mut(&self.sid_text) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count > 0 {
            return;
        }
        registry.live.remove(&self.sid_text);
        let outcome =
            LocalSid::parse(&self.sid_text).and_then(|sid| update_loopback_exemption(sid.0, false));
        if let Err(error) = outcome {
            // A crashed holder cannot clean up either: the exemption then stays until the next
            // holder for the same container ends. It never widens file access.
            eprintln!(
                "rightkit-sandbox: could not remove the loopback exemption for {}: {error}",
                self.sid_text
            );
        }
    }
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
struct ContainerProfile {
    name: String,
    sid: *mut c_void,
    sid_text: String,
    leases: Arc<LeaseSet>,
}

impl ContainerProfile {
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
        let key = lease_key(LeaseKind::Allow, &self.sid_text, path);
        let mut items = self.leases.items.lock().unwrap_or_else(|e| e.into_inner());
        if items
            .iter()
            .any(|l| l.kind == LeaseKind::Allow && l.key == key && (l.writable || !writable))
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

    /// Deny the container `path` (and everything below it) for the lifetime of the launch's
    /// child, with an explicit DENY ACE that wins over any allow inherited from a granted root.
    fn lease_deny(&self, path: &Path) -> io::Result<()> {
        let key = lease_key(LeaseKind::Deny, &self.sid_text, path);
        let mut items = self.leases.items.lock().unwrap_or_else(|e| e.into_inner());
        if items.iter().any(|l| l.key == key) {
            return Ok(());
        }
        items.push(GrantLease::deny(self.sid, &self.sid_text, path)?);
        Ok(())
    }

    /// Lease traverse access on every physical ancestor of `path` for this container. Under
    /// [`AncestorLeasePolicy::BestEffort`] an ancestor whose DACL cannot be written (no
    /// WRITE_DAC, for example under `C:\Users`) is skipped instead of failing the launch, and the
    /// deepest skipped one is remembered as a Git ceiling for the child's environment.
    fn lease_ancestors(
        &self,
        path: &Path,
        policy: AncestorLeasePolicy,
    ) -> Result<(), SandboxError> {
        let lease = AncestorLease::acquire_with(path, &self.sid_text, policy)?;
        // Ancestors nearest the leased path are the deepest; Git must not reach any of them, so
        // the deepest unleasable one bounds discovery.
        if let Some(deepest) = lease
            .unleasable()
            .iter()
            .max_by_key(|ancestor| ancestor.path.components().count())
        {
            let ceiling = plain_path(&deepest.path);
            let mut ceilings = self
                .leases
                .git_ceilings
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !ceilings.contains(&ceiling) {
                ceilings.push(ceiling);
            }
        }
        self.leases
            .ancestors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(lease);
        Ok(())
    }

    /// Take (once per container) the loopback exemption a network-enabled container needs to
    /// reach local services. `false` when it could not be established, typically because the
    /// process is not elevated: the container then stays loopback-blocked, which is narrower
    /// than asked, never wider.
    fn lease_loopback(&self) -> bool {
        let mut held = self
            .leases
            .loopback
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if held.is_some() {
            return true;
        }
        match LoopbackLease::acquire(self.sid, &self.sid_text) {
            Ok(Some(lease)) => {
                *held = Some(lease);
                true
            }
            Ok(None) => false,
            Err(error) => {
                eprintln!(
                    "rightkit-sandbox: loopback exemption unavailable for {} ({error}); \
                     adding it needs an elevated process, the container stays loopback-blocked",
                    self.sid_text
                );
                false
            }
        }
    }

    /// The Git ceilings recorded for this container, joined for `GIT_CEILING_DIRECTORIES`.
    fn git_ceiling_value(&self) -> Option<String> {
        let ceilings = self
            .leases
            .git_ceilings
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if ceilings.is_empty() {
            return None;
        }
        std::env::join_paths(ceilings.iter())
            .ok()
            .map(|joined| joined.to_string_lossy().into_owned())
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

// SAFETY: `sid` is allocated once, never written afterwards and freed only in `Drop`; every use
// is a read-only FFI call. The mutable state (`leases`) is behind mutexes.
unsafe impl Send for ContainerProfile {}
unsafe impl Sync for ContainerProfile {}

impl Drop for ContainerProfile {
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

/// An outbound pipe for the child's stdin: the parent writes `File`, the child reads the
/// inheritable raw handle. The DACL admits only the user and the container, and dropping the
/// `File` is the child's end-of-input.
fn input_pipe(security: &PipeSecurity) -> io::Result<(std::fs::File, RawHandle)> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let name = wide(format!(
        r"\\.\pipe\rightkit-sandbox-in-{}-{}",
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
            PIPE_ACCESS_OUTBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            &attributes,
        )
    };
    if server.is_null() || server == INVALID_HANDLE {
        return Err(last_error("CreateNamedPipeW stdin"));
    }
    // SAFETY: opens the unique client end of the server just created.
    let client = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if client.is_null() || client == INVALID_HANDLE {
        let e = last_error("open stdin pipe client");
        close_raw(server);
        return Err(e);
    }
    // SAFETY: synchronous connect of a server whose client is already open.
    if unsafe { ConnectNamedPipe(server, std::ptr::null_mut()) } == 0
        && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED
    {
        let e = last_error("ConnectNamedPipe stdin");
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

    /// Whether this child's container holds the loopback exemption.
    pub(crate) fn loopback_exempt(&self) -> bool {
        self._leases
            .loopback
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
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
    ) -> io::Result<rightkit_child_process::ReapOutcome> {
        let started = Instant::now();
        if let Some(job) = &self.job {
            job.terminate()?;
        } else if !self.has_exited()? {
            // SAFETY: live process handle owned by self.
            unsafe { TerminateProcess(HANDLE(self.process), 1) }.map_err(io::Error::other)?;
        }
        let Some(status) = self.wait_timeout(timeout.saturating_sub(started.elapsed()))? else {
            return Ok(rightkit_child_process::ReapOutcome::TimedOut);
        };
        let empty = match &self.job {
            Some(job) => job.wait_empty(timeout.saturating_sub(started.elapsed()))?,
            None => true,
        };
        if empty {
            Ok(rightkit_child_process::ReapOutcome::Exited(status))
        } else {
            Ok(rightkit_child_process::ReapOutcome::TimedOut)
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
    /// The parent's write end of the child's stdin, for a piped spawn; `None` means NUL.
    pub(crate) stdin: Option<std::fs::File>,
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
    stdin: Option<std::fs::File>,
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

/// The identity of a launch's container: its grants, deny entries and loopback choice, sorted.
/// With no deny entry and no loopback exemption it is the grant key every earlier version used,
/// so existing profiles keep their names.
fn profile_key(grants: &[(PathBuf, bool)], denies: &[PathBuf], loopback: bool) -> Vec<String> {
    let mut key: Vec<String> = grants
        .iter()
        .map(|(p, w)| format!("{}{}", if *w { "w:" } else { "r:" }, lexical_path_key(p)))
        .chain(denies.iter().map(|p| format!("d:{}", lexical_path_key(p))))
        .collect();
    if loopback {
        key.push("loopback".into());
    }
    key.sort();
    key
}

/// Whether a policy asks for the loopback exemption (it needs network to mean anything).
fn wants_loopback(policy: &SandboxPolicy) -> bool {
    policy.network == NetworkPolicy::Allow && policy.loopback == LoopbackPolicy::Exempt
}

/// The concrete, existing paths the policy's `deny_read` entries name: globs expanded (bounded),
/// every path planned lexically and canonically. An entry that cannot be planned (a relative
/// path, a filesystem root, a glob over a huge tree) fails the spawn instead of being dropped.
/// A path that does not exist has no object to attach a deny to and is skipped.
fn deny_targets(policy: &SandboxPolicy) -> Result<Vec<PathBuf>, SandboxError> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in &policy.deny_read {
        match entry {
            DenyReadEntry::Path(path) => paths.push(path.clone()),
            DenyReadEntry::Glob(pattern) => {
                paths.extend(expand_glob(pattern, GLOB_EXPANSION_LIMIT).map_err(SandboxError::Io)?)
            }
        }
    }
    let planned = plan_deny_read_paths(&paths).map_err(|e| match e.kind() {
        io::ErrorKind::InvalidInput => SandboxError::InvalidPolicy(e.to_string()),
        _ => SandboxError::Io(e),
    })?;
    let mut targets: Vec<PathBuf> = Vec::new();
    for path in planned {
        let path = plain_path(&path);
        let key = lexical_path_key(&path);
        if path.exists() && !targets.iter().any(|t| lexical_path_key(t) == key) {
            targets.push(path);
        }
    }
    Ok(targets)
}

/// Test support: the name of the AppContainer profile a spawn with `policy` registers for the
/// current user. The granted paths must exist, as they must for the spawn itself.
#[doc(hidden)]
pub fn appcontainer_profile_name_for_test(policy: &SandboxPolicy) -> Result<String, SandboxError> {
    let key = profile_key(
        &canonical_grants(policy)?,
        &deny_targets(policy)?,
        wants_loopback(policy),
    );
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

// ---------------------------------------------------------------------------------------
// Reusable profile
// ---------------------------------------------------------------------------------------

/// Validate a requested extra grant and return its canonical path.
fn validated_grant(path: &Path) -> Result<PathBuf, SandboxError> {
    if crate::path_is_protected_secret(path) {
        return Err(SandboxError::InvalidPolicy(format!(
            "{} is a protected location and cannot be granted",
            path.display()
        )));
    }
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
    // A canonical path can still land inside a secret (a junction into `.ssh`).
    if crate::path_is_protected_secret(&canonical) {
        return Err(SandboxError::InvalidPolicy(format!(
            "{} resolves to a protected location and cannot be granted",
            path.display()
        )));
    }
    Ok(canonical)
}

/// A named, reusable AppContainer with its leases held: the confinement boundary for a series of
/// commands, spawned with [`crate::Sandbox::spawn_in_profile`].
///
/// The container is `rightkit.sandbox.<hash of its grants>` (so [`remove_unused_appcontainer_profiles`]
/// and the ancestor sweeps know it), its SID derived from that name. Its allow grants, deny
/// carve-outs, ancestor traverse leases and loopback exemption are reference-counted leases shared
/// with every child spawned from it: they are revoked when the profile and the last such child are
/// gone, never earlier. Ancestors the user cannot re-ACL are skipped rather than failing the
/// profile; the deepest one becomes a `GIT_CEILING_DIRECTORIES` entry in each child's environment
/// (unless the caller sets that variable), so upward repository discovery stops before it.
pub struct AppContainerProfile {
    prepared: PreparedContainer,
}

impl AppContainerProfile {
    /// The container for one workspace: read/write on `workspace_root`, with an explicit deny on
    /// every protected secret folder ([`crate::secret_paths_within`]) that lies inside it, so a
    /// workspace that is the user profile never exposes `.ssh` and friends.
    pub fn for_workspace(workspace_root: &Path) -> Result<Self, SandboxError> {
        let canonical = std::fs::canonicalize(workspace_root)
            .map(|p| plain_path(&p))
            .map_err(|e| {
                SandboxError::InvalidPolicy(format!(
                    "workspace {} does not resolve: {e}",
                    workspace_root.display()
                ))
            })?;
        let mut policy = SandboxPolicy::new().allow_write(&canonical);
        for secret in crate::secret_paths_within(&canonical) {
            policy = policy.deny_read(secret);
        }
        Self::for_policy(&policy)
    }

    /// The container for `policy`'s grants, deny entries and network choice, created or adopted by
    /// name. `ConfinementMode::WriteOnly` is refused.
    pub fn for_policy(policy: &SandboxPolicy) -> Result<Self, SandboxError> {
        policy.validate()?;
        Ok(Self {
            prepared: prepare_container(policy, AncestorLeasePolicy::BestEffort)?,
        })
    }

    /// Allow or withhold outbound network (the `internetClient` capability) for later spawns.
    /// The container's SID does not change.
    pub fn with_network(mut self, allow: bool) -> Self {
        self.prepared.network = allow;
        self
    }

    /// Ask for the loopback exemption on later network-enabled spawns. It needs an elevated
    /// process; without one the container stays loopback-blocked.
    pub fn with_loopback(mut self, exempt: bool) -> Self {
        self.prepared.loopback = exempt;
        self
    }

    /// Add a grant by building a newly keyed profile. The container SID derives from the complete
    /// frozen grant and deny set, so the returned profile has its own identity (existing grants
    /// plus `path`, with a deny on every protected secret inside it) and the original profile's
    /// SID and ACLs are never mutated. Consuming `self` releases the old profile's leases.
    /// Refuses a protected location, a filesystem root and a path that does not resolve; on any
    /// failure every lease newly taken for the new profile is rolled back.
    pub fn with_grant(self, path: &Path, writable: bool) -> Result<Self, SandboxError> {
        let canonical = validated_grant(path)?;
        let mut grants = self
            .prepared
            .grants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let key = lexical_path_key(&canonical);
        match grants.iter_mut().find(|(p, _)| lexical_path_key(p) == key) {
            Some(existing) => existing.1 |= writable,
            None => grants.push((canonical.clone(), writable)),
        }
        let mut denies = self.prepared.denies.clone();
        for secret in crate::secret_paths_within(&canonical) {
            let secret = plain_path(&secret);
            let secret_key = lexical_path_key(&secret);
            if !denies.iter().any(|d| lexical_path_key(d) == secret_key) {
                denies.push(secret);
            }
        }
        let prepared = prepare_container_from(
            grants,
            denies,
            self.prepared.loopback,
            self.prepared.network,
            self.prepared.named_capabilities.clone(),
            AncestorLeasePolicy::BestEffort,
        )?;
        Ok(Self { prepared })
    }

    /// Always refused for a grant that would otherwise be valid: a profile's SID derives from its
    /// frozen grant policy, so mutating the ACLs of that shared SID would widen every other
    /// profile and child using it (and a persistent ACE could outlive a crash). Use
    /// [`AppContainerProfile::with_grant`], which produces a newly keyed profile. Protected
    /// locations, filesystem roots and paths that do not resolve are refused first, with the
    /// same errors `with_grant` gives.
    pub fn lease_grant(&self, path: &Path, _writable: bool) -> Result<(), SandboxError> {
        validated_grant(path)?;
        Err(SandboxError::InvalidPolicy(format!(
            "cannot add {} to profile {} in place: its identity is frozen; use with_grant to \
             build a newly keyed profile",
            path.display(),
            self.prepared.profile.name
        )))
    }

    /// The profile name, `rightkit.sandbox.<16 hex digits>`.
    pub fn name(&self) -> &str {
        &self.prepared.profile.name
    }

    /// The container SID in `S-1-15-2-...` form, as `icacls` prints it.
    pub fn sid_string(&self) -> &str {
        &self.prepared.profile.sid_text
    }

    /// The container's own folder (`...\Packages\<name>\AC`), which it can always reach.
    pub fn folder_path(&self) -> io::Result<PathBuf> {
        self.prepared.profile.folder_path()
    }

    /// Whether spawns from this profile get the `internetClient` capability.
    pub fn network_allowed(&self) -> bool {
        self.prepared.network
    }

    /// The Git ceilings recorded for ancestors that could not be leased.
    pub fn git_ceilings(&self) -> Vec<PathBuf> {
        self.prepared
            .profile
            .leases
            .git_ceilings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Spawn `command` in `profile`'s container with `env`.
pub(crate) fn spawn_in_profile(
    profile: &AppContainerProfile,
    command: &SandboxCommand,
    env: &EnvPolicy,
    piped_stdin: bool,
) -> Result<ConfinedSpawn, SandboxError> {
    let stdin_mode = if piped_stdin {
        StdinMode::Piped
    } else {
        StdinMode::Null
    };
    let program = resolve_image(&command.program)?;
    confined_spawn(launch_container(
        &profile.prepared,
        program,
        command,
        env,
        None,
        None,
        stdin_mode,
    )?)
}

/// Resolve `program` to the full image path an AppContainer launch uses: an absolute path is kept,
/// anything else is searched on `PATH`, then in System32 and Windows PowerShell (the launch
/// passes an explicit image, so no search happens at creation).
pub fn resolve_program_str(program: &str) -> io::Result<PathBuf> {
    resolve_program(OsStr::new(program))
}

// ---------------------------------------------------------------------------------------
// Pinned read roots
// ---------------------------------------------------------------------------------------

/// `ACE_FLAGS` bit marking an inherited ACE.
const INHERITED_ACE: u8 = 0x10;

/// One explicit allow ACE for a trustee: its mask and inheritance flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PriorAce {
    mask: u32,
    flags: u8,
}

/// The inheritance bits (`OBJECT_INHERIT`, `CONTAINER_INHERIT`, `NO_PROPAGATE`, `INHERIT_ONLY`)
/// to hand back to `SetEntriesInAclW` when restoring a recorded ACE.
fn prior_ace_inheritance(flags: u8) -> u32 {
    u32::from(
        flags
            & (OBJECT_INHERIT_ACE
                | CONTAINER_INHERIT_ACE
                | NO_PROPAGATE_INHERIT_ACE
                | INHERIT_ONLY_ACE),
    )
}

/// Whether recorded prior ACEs already hold everything a pin adds, so the pin has nothing to
/// undo and nothing of its own to remove.
fn prior_entries_cover(prior: &[PriorAce], permissions: u32) -> bool {
    prior
        .iter()
        .any(|ace| ace.flags & INHERIT_ONLY_ACE == 0 && ace.mask & permissions == permissions)
}

/// Explicit (non-inherited) allow ACEs `sid` holds in the live DACL of `path`.
fn explicit_allow_entries(path: &Path, sid: *mut c_void) -> io::Result<Vec<PriorAce>> {
    let mut wide_path = wide(path);
    let mut found = Vec::new();
    // SAFETY: as in apply_sid_path_entry; the descriptor is freed after the DACL is read and
    // every ACE is bounds-checked against the ACL size before its fields are trusted.
    unsafe {
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        let got = GetNamedSecurityInfoW(
            wide_path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        );
        if got != 0 {
            return Err(io::Error::from_raw_os_error(got as i32));
        }
        if !dacl.is_null() {
            let acl = dacl as *const Acl;
            let (revision, acl_size, ace_count) =
                ((*acl).acl_revision, (*acl).acl_size, (*acl).ace_count);
            if revision == ACL_REVISION {
                for index in 0..u32::from(ace_count) {
                    let mut ace: *mut c_void = std::ptr::null_mut();
                    if GetAce(dacl, index, &mut ace) == 0 || ace.is_null() {
                        continue;
                    }
                    let offset = (ace as usize).wrapping_sub(dacl as usize);
                    if offset >= usize::from(acl_size) {
                        continue;
                    }
                    let allowed = &*(ace as *const AccessAllowedAce);
                    if allowed.header.ace_type != ACCESS_ALLOWED_ACE_TYPE
                        || usize::from(allowed.header.ace_size)
                            < std::mem::size_of::<AccessAllowedAce>()
                        || offset + usize::from(allowed.header.ace_size) > usize::from(acl_size)
                        || allowed.header.ace_flags & INHERITED_ACE != 0
                    {
                        continue;
                    }
                    if EqualSid(&allowed.sid_start as *const u32 as *mut c_void, sid) != 0 {
                        found.push(PriorAce {
                            mask: allowed.mask,
                            flags: allowed.header.ace_flags,
                        });
                    }
                }
            }
        }
        LocalFree(descriptor);
    }
    Ok(found)
}

/// What each live pin found on its path before it added anything: the explicit ALL APPLICATION
/// PACKAGES allow ACEs to put back when the pin is removed. Keyed by lexical path.
fn pin_prior_registry() -> &'static Mutex<HashMap<String, Vec<PriorAce>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Vec<PriorAce>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// Grant ALL APPLICATION PACKAGES read and execute on `path` (and below, for a directory) so every
/// AppContainer child can read an interpreter install. `Ok(false)`: the live DACL already allows
/// that, nothing was added and nothing must be revoked later. Explicit ACEs the trustee already
/// held are recorded first so [`pin_revoke`] restores exactly them.
pub(crate) fn pin_grant(path: &Path) -> io::Result<bool> {
    if packages_can_read(path) {
        return Ok(false);
    }
    let packages = LocalSid::parse(ALL_APP_PACKAGES_SID)?;
    let prior = explicit_allow_entries(path, packages.0)?;
    let key = lexical_path_key(path);
    pin_prior_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.clone())
        .or_insert(prior);
    let granted = apply_sid_path_entry(
        path,
        packages.0,
        GRANT_ACCESS,
        FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        inheritance_for(path),
    )
    .map_err(|e| step_error(format!("pin read root {}", path.display()), e));
    if granted.is_err() {
        pin_prior_registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }
    granted?;
    Ok(true)
}

/// Remove only what [`pin_grant`] added: revoke the trustee's explicit entries, then restore the
/// explicit ACEs recorded before the pin so pre-existing rights survive.
pub(crate) fn pin_revoke(path: &Path) -> io::Result<()> {
    let packages = LocalSid::parse(ALL_APP_PACKAGES_SID)?;
    let key = lexical_path_key(path);
    let prior = pin_prior_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned()
        .unwrap_or_default();
    if prior_entries_cover(&prior, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE) {
        // The original already held everything the pin adds; there is nothing of ours to remove.
        pin_prior_registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
        return Ok(());
    }
    apply_sid_path_entry(path, packages.0, REVOKE_ACCESS, 0, inheritance_for(path))
        .map_err(|e| step_error(format!("unpin read root {}", path.display()), e))?;
    for ace in &prior {
        apply_sid_path_entry(
            path,
            packages.0,
            GRANT_ACCESS,
            ace.mask,
            prior_ace_inheritance(ace.flags),
        )
        .map_err(|e| step_error(format!("restore prior rights on {}", path.display()), e))?;
    }
    pin_prior_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    Ok(())
}

/// Ancestor traverse policy of the one-shot `spawn`: an ancestor that cannot be leased fails it.
const ONE_SHOT_ANCESTOR_POLICY: AncestorLeasePolicy = AncestorLeasePolicy::Strict;
/// Ancestor traverse policy of `spawn_with_pipe_pair`: a denied ancestor write is skipped only
/// under the conditions `AncestorLease::record_unleasable` checks.
const PIPE_PAIR_ANCESTOR_POLICY: AncestorLeasePolicy = AncestorLeasePolicy::BestEffort;

/// Spawn `command` confined to `policy`: suspended, assigned to a kill-on-close job, resumed.
/// With `piped_stdin` the child's stdin is a pipe whose write end comes back in the result.
pub(crate) fn spawn(
    policy: &SandboxPolicy,
    command: &SandboxCommand,
    piped_stdin: bool,
) -> Result<ConfinedSpawn, SandboxError> {
    let stdin_mode = if piped_stdin {
        StdinMode::Piped
    } else {
        StdinMode::Null
    };
    let launched = spawn_inner(
        policy,
        command,
        None,
        None,
        stdin_mode,
        ONE_SHOT_ANCESTOR_POLICY,
    )?;
    confined_spawn(launched)
}

fn confined_spawn(launched: ConfinedLaunch) -> Result<ConfinedSpawn, SandboxError> {
    let stdout = launched.stdout.ok_or_else(|| {
        SandboxError::InvalidPolicy("ordinary confined spawn did not create stdout capture".into())
    })?;
    let stderr = launched.stderr.ok_or_else(|| {
        SandboxError::InvalidPolicy("ordinary confined spawn did not create stderr capture".into())
    })?;
    Ok(ConfinedSpawn {
        child: launched.child,
        stdin: launched.stdin,
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
    // Best effort: a standard user cannot re-ACL admin-owned ancestors such as C:\Users, and the
    // token's bypass-traverse privilege reaches the grants below them without an ACE. An ancestor
    // is skipped only when its write is denied and its readable DACL still lacks the access (see
    // `AncestorLease::record_unleasable`); every grant, deny and image lease stays strict. This
    // entry point accepts any `SandboxCommand` (network Deny), not only the browser fixture, so
    // directory listing above a skipped ancestor can fail for it; as for `AppContainerProfile`,
    // `launch_container` exports the deepest skipped ancestor as `GIT_CEILING_DIRECTORIES` unless
    // the caller sets that variable, so upward Git discovery stops there.
    let launched = spawn_inner(
        policy,
        command,
        Some(handle_flag),
        stdio_file,
        StdinMode::Null,
        PIPE_PAIR_ANCESTOR_POLICY,
    )?;
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

const FILE_SHARE_READ: u32 = 0x1;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// True when `path` equals or lies under `root` (component-wise, case-insensitive).
fn path_within(path: &Path, root: &Path) -> bool {
    let key = lexical_path_key(path);
    let root = lexical_path_key(root);
    key == root || key.starts_with(&format!("{root}\\"))
}

fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Check that `path` names a plain file directly inside a writable grant and return the
/// parent-canonicalized target. The child inherits the opened handle, so an unchecked path
/// would let it append to any file the parent can write.
fn validate_stdio_path(path: &Path, grants: &[(PathBuf, bool)]) -> Result<PathBuf, SandboxError> {
    use std::path::Component;
    let bad = |why: &str| {
        SandboxError::InvalidPolicy(format!("stdio file {} refused: {why}", path.display()))
    };
    if !path.is_absolute() {
        return Err(bad("path must be absolute"));
    }
    for component in path.components() {
        match component {
            Component::ParentDir | Component::CurDir => {
                return Err(bad("`.` and `..` components are not allowed"))
            }
            Component::Prefix(prefix) => {
                use std::path::Prefix;
                if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(..)) {
                    return Err(bad("verbatim and device paths are not allowed"));
                }
            }
            _ => {}
        }
    }
    let name = path.file_name().ok_or_else(|| bad("no file name"))?;
    let text = name.to_string_lossy();
    if text.contains([':', '/', '\\', '*', '?', '"', '<', '>', '|'])
        || text.ends_with('.')
        || text.ends_with(' ')
    {
        return Err(bad("file name is not a plain name"));
    }
    let stem = text.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err(bad("reserved device name"));
    }
    let parent = path.parent().ok_or_else(|| bad("no parent directory"))?;
    let parent = std::fs::canonicalize(parent)
        .map(|p| plain_path(&p))
        .map_err(|e| bad(&format!("parent directory does not resolve: {e}")))?;
    if !grants
        .iter()
        .any(|(root, writable)| *writable && path_within(&parent, root))
    {
        return Err(bad("outside the policy's write_paths"));
    }
    let target = parent.join(name);
    match std::fs::symlink_metadata(&target) {
        Ok(metadata) if is_reparse(&metadata) => Err(bad("target is a symlink or reparse point")),
        Ok(metadata) if !metadata.is_file() => Err(bad("target is not a regular file")),
        Ok(_) => Ok(target),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(target),
        Err(e) => Err(e.into()),
    }
}

/// Validate and open the diagnostic stdio file: append, no other writer, never following a
/// reparse point.
fn open_stdio_file(path: &Path, grants: &[(PathBuf, bool)]) -> Result<std::fs::File, SandboxError> {
    use std::os::windows::fs::OpenOptionsExt;
    let target = validate_stdio_path(path, grants)?;
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&target)?;
    if is_reparse(&file.metadata()?) {
        return Err(SandboxError::InvalidPolicy(format!(
            "stdio file {} refused: target is a symlink or reparse point",
            path.display()
        )));
    }
    if file_link_count(&file)? > 1 {
        return Err(SandboxError::InvalidPolicy(format!(
            "stdio file {} refused: target has more than one hard link",
            path.display()
        )));
    }
    // Re-check the object actually opened: the parent could have been swapped (junction,
    // mount) between validation and open. VOLUME_NAME_DOS (0) names the final location.
    let final_path = final_path_of_handle(&file)?;
    let final_text = normalize_final_path(&final_path.to_string_lossy());
    let inside = grants.iter().any(|(root, writable)| {
        *writable
            && path_within(
                Path::new(&final_text),
                Path::new(&normalize_final_path(&root.to_string_lossy())),
            )
    });
    if !inside {
        drop(file);
        return Err(SandboxError::InvalidPolicy(format!(
            "stdio file {} refused: opened file resolves outside the policy's write_paths",
            path.display()
        )));
    }
    Ok(file)
}

/// Final DOS path of an open file (GetFinalPathNameByHandleW, VOLUME_NAME_DOS).
fn final_path_of_handle(file: &std::fs::File) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let mut buffer = vec![0u16; 512];
    loop {
        // SAFETY: the handle is valid for the borrow; the buffer length is passed exactly.
        let len = unsafe {
            ancestors::GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        if (len as usize) < buffer.len() {
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(
                &buffer[..len as usize],
            )));
        }
        buffer.resize(len as usize + 1, 0);
    }
}

/// Strip the `\\?\` / `\\?\UNC\` verbatim prefixes so a final path and a grant compare in
/// one spelling (`\\?\UNC\srv\share` becomes `\\srv\share`).
fn normalize_final_path(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = text.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        text.to_string()
    }
}

#[repr(C)]
struct ByHandleFileInformation {
    file_attributes: u32,
    creation_time: [u32; 2],
    last_access_time: [u32; 2],
    last_write_time: [u32; 2],
    volume_serial_number: u32,
    file_size_high: u32,
    file_size_low: u32,
    number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetFileInformationByHandle(
        handle: *mut std::ffi::c_void,
        info: *mut ByHandleFileInformation,
    ) -> i32;
}

/// Hard-link count of an open file (nNumberOfLinks from GetFileInformationByHandle).
fn file_link_count(file: &std::fs::File) -> io::Result<u32> {
    let mut info = std::mem::MaybeUninit::<ByHandleFileInformation>::zeroed();
    // SAFETY: the handle is valid for the borrow and `info` is a correctly sized out buffer.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded and initialised the structure.
    Ok(unsafe { info.assume_init() }.number_of_links)
}

/// How a launch wires the child's stdin.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StdinMode {
    /// The NUL device: the child reads end-of-file at once.
    Null,
    /// A pipe whose write end the parent keeps; dropping it ends the child's input.
    Piped,
}

/// A container with its leases taken: what one or more launches share.
struct PreparedContainer {
    profile: ContainerProfile,
    /// The granted roots, (canonical path, writable). Extra grants join the list.
    grants: Mutex<Vec<(PathBuf, bool)>>,
    /// The deny carve-outs frozen into the profile identity.
    denies: Vec<PathBuf>,
    network: bool,
    /// Take the loopback exemption for a network-enabled launch.
    loopback: bool,
    named_capabilities: Vec<String>,
}

/// Create or adopt the container for `policy` and take every lease it needs: traverse access on
/// the ancestors of each grant (`ancestor_policy` says whether one that cannot be written fails
/// the launch), the grants themselves, and the deny carve-outs. Nothing is granted that the policy
/// does not list, and a failure at any step drops the leases already taken.
fn prepare_container(
    policy: &SandboxPolicy,
    ancestor_policy: AncestorLeasePolicy,
) -> Result<PreparedContainer, SandboxError> {
    if policy.mode == ConfinementMode::WriteOnly {
        return Err(SandboxError::Unsupported(
            "ConfinementMode::WriteOnly is implemented for macOS only; an AppContainer is \
             deny-by-default and cannot leave reads open"
                .into(),
        ));
    }
    let grants = canonical_grants(policy)?;
    let denies = deny_targets(policy)?;
    let loopback = wants_loopback(policy);
    prepare_container_from(
        grants,
        denies,
        loopback,
        policy.network == NetworkPolicy::Allow,
        policy.named_capabilities.clone(),
        ancestor_policy,
    )
}

/// Create or adopt the container keyed by the complete frozen `grants`, `denies` and `loopback`
/// choice, then take its leases. Deny carve-outs are installed before any allow so a protected
/// child is never reachable through a broad allow, and a failure at any step drops every lease
/// already taken (the profile's lease set is released with it).
fn prepare_container_from(
    grants: Vec<(PathBuf, bool)>,
    denies: Vec<PathBuf>,
    loopback: bool,
    network: bool,
    named_capabilities: Vec<String>,
    ancestor_policy: AncestorLeasePolicy,
) -> Result<PreparedContainer, SandboxError> {
    let name = profile_name(&profile_key(&grants, &denies, loopback));
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
        let profile = ContainerProfile::ensure(&name)?;
        *profile
            .leases
            .holder
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(holder);
        profile
    };
    for (path, _) in &grants {
        profile.lease_ancestors(path, ancestor_policy)?;
    }
    // Before every allow: an explicit deny ACE on the protected object outranks an allow
    // inherited from a granted root, and it is already in place when the broad allow lands.
    for path in &denies {
        profile
            .lease_deny(path)
            .map_err(|e| step_error(format!("deny {}", path.display()), e))?;
    }
    for (path, writable) in &grants {
        profile
            .lease(path, *writable)
            .map_err(|e| step_error(format!("lease {}", path.display()), e))?;
    }
    Ok(PreparedContainer {
        profile,
        grants: Mutex::new(grants),
        denies,
        network,
        loopback,
        named_capabilities,
    })
}

/// `program` resolved to the real image path `CreateProcessW` is given. Launch and lease the real
/// image: version managers (nvm-windows, volta, scoop) expose tools through links whose link
/// objects the container cannot use and the user cannot re-ACL.
fn resolve_image(program: &OsStr) -> Result<PathBuf, SandboxError> {
    let program = resolve_program(program)?;
    Ok(std::fs::canonicalize(&program)
        .map(|p| plain_path(&p))
        .unwrap_or(program))
}

fn spawn_inner(
    policy: &SandboxPolicy,
    command: &SandboxCommand,
    handle_flag: Option<&str>,
    stdio_file: Option<&Path>,
    stdin_mode: StdinMode,
    ancestor_policy: AncestorLeasePolicy,
) -> Result<ConfinedLaunch, SandboxError> {
    let program = resolve_image(&command.program)?;
    let prepared = prepare_container(policy, ancestor_policy)?;
    launch_container(
        &prepared,
        program,
        command,
        &policy.env,
        handle_flag,
        stdio_file,
        stdin_mode,
    )
}

/// Start `command` in `prepared`'s container: lease its image, build its cwd and environment,
/// wire its stdio, create it suspended, put it in a kill-on-close job, then resume it.
fn launch_container(
    prepared: &PreparedContainer,
    program: PathBuf,
    command: &SandboxCommand,
    env: &EnvPolicy,
    handle_flag: Option<&str>,
    stdio_file: Option<&Path>,
    stdin_mode: StdinMode,
) -> Result<ConfinedLaunch, SandboxError> {
    let profile = &prepared.profile;
    let grants: Vec<(PathBuf, bool)> = prepared
        .grants
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    profile.lease_image_dir(&program)?;
    let folder = profile.folder_path()?;
    let cwd = match &command.cwd {
        Some(requested) => confined_cwd(requested, &grants)?,
        None => folder.clone(),
    };
    if prepared.network && prepared.loopback {
        profile.lease_loopback();
    }
    let mut extra: Vec<(String, String)> = command.env.clone();
    if let Some(ceilings) = profile.git_ceiling_value() {
        let named = |key: &str| key.eq_ignore_ascii_case("GIT_CEILING_DIRECTORIES");
        // The caller's own setting wins over the computed one.
        if !extra.iter().any(|(key, _)| named(key)) && !env.set.keys().any(|key| named(key)) {
            extra.push(("GIT_CEILING_DIRECTORIES".into(), ceilings));
        }
    }
    let environment = confined_environment(&cwd, &folder, env, &extra);

    let security = PipeSecurity::new(&profile.sid_text)?;
    // A piped stdin is an ACL-bound pipe whose write end the parent keeps; otherwise the child
    // reads the NUL device. Pipe-pair launches always use NUL.
    let (stdin_parent, stdin) = match stdin_mode {
        StdinMode::Piped if handle_flag.is_none() => {
            let (parent, child_end) = input_pipe(&security)?;
            (Some(parent), child_end)
        }
        _ => (
            None,
            std::fs::OpenOptions::new()
                .read(true)
                .open("NUL")?
                .into_raw_handle(),
        ),
    };
    let (stdout, stdout_child, stderr, stderr_child) = if handle_flag.is_some() {
        // Pipe-pair callers drain Chrome DevTools I/O themselves. Keep stdio on NUL (or on an
        // append-mode diagnostic file the caller named) so this path has no reader threads
        // whose ownership could outlive cancellation. The parent opens the file, so the
        // child needs no ACL on it; the handle rides the explicit inherit list.
        // The diagnostic file is validated against the policy's write grants and opened once
        // (no other writer may share it); stdout and stderr get duplicates of that handle.
        let sink_file = match stdio_file {
            Some(path) => match open_stdio_file(path, &grants) {
                Ok(file) => Some(file),
                Err(error) => {
                    close_raw(stdin);
                    return Err(error);
                }
            },
            None => None,
        };
        let open_sink = || -> io::Result<RawHandle> {
            let file = match &sink_file {
                Some(file) => file.try_clone()?,
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
            profile,
            SuspendedLaunchParams {
                program: &program,
                command_line: &command_line,
                cwd: &cwd,
                environment: &environment,
                network: prepared.network,
                named_capabilities: &prepared.named_capabilities,
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
        stdin: stdin_parent,
        stdout,
        stderr,
        auxiliary,
    })
}

/// `CreateProcessW` with the AppContainer's `SECURITY_CAPABILITIES` (plus `internetClient`
/// when network is allowed), an explicit inherited-handle list, no window, and suspended.
fn create_suspended(
    profile: &ContainerProfile,
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
    #[test]
    fn profile_identity_covers_every_grant_and_deny() {
        let base = vec![(PathBuf::from("C:\\w"), true)];
        let widened = vec![
            (PathBuf::from("C:\\w"), true),
            (PathBuf::from("C:\\q"), false),
        ];
        let denies = vec![PathBuf::from("C:\\w\\.ssh")];
        assert_ne!(
            profile_key(&base, &[], false),
            profile_key(&widened, &[], false)
        );
        assert_ne!(
            profile_key(&base, &[], false),
            profile_key(&base, &denies, false)
        );
        assert_ne!(
            profile_name(&profile_key(&base, &[], false)),
            profile_name(&profile_key(&widened, &[], false))
        );
    }

    #[test]
    fn prior_ace_helpers_preserve_inheritance_and_detect_coverage() {
        let flags = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE | INHERITED_ACE;
        assert_eq!(prior_ace_inheritance(flags), 0x3);
        let read_only = PriorAce {
            mask: FILE_GENERIC_READ,
            flags: 0,
        };
        let both = PriorAce {
            mask: FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            flags: 0,
        };
        let inherit_only = PriorAce {
            mask: both.mask,
            flags: INHERIT_ONLY_ACE,
        };
        let want = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
        assert!(!prior_entries_cover(&[], want));
        assert!(!prior_entries_cover(&[read_only], want));
        assert!(!prior_entries_cover(&[inherit_only], want));
        assert!(prior_entries_cover(&[read_only, both], want));
    }

    #[test]
    fn firewall_dll_loads_from_system32_only() {
        assert_eq!(LOAD_LIBRARY_SEARCH_SYSTEM32, 0x0000_0800);
    }

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

    fn scratch_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("rk-stdio-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).map(|p| plain_path(&p)).unwrap()
    }

    #[test]
    fn stdio_file_inside_write_grant_is_accepted() {
        let root = scratch_dir("ok");
        let grants = vec![(root.clone(), true)];
        let file = open_stdio_file(&root.join("chrome-stderr.txt"), &grants).unwrap();
        drop(file);
        assert!(root.join("chrome-stderr.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stdio_file_preexisting_empty_file_without_open_handle_is_accepted() {
        // Mirrors the browser fixture: the file exists (created and closed), nothing holds it.
        let root = scratch_dir("pre");
        let grants = vec![(root.clone(), true)];
        let path = root.join("chrome-stderr.txt");
        drop(std::fs::File::create(&path).unwrap());
        let file = open_stdio_file(&path, &grants).unwrap();
        drop(file);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn normalize_final_path_strips_verbatim_prefixes() {
        assert_eq!(normalize_final_path(r"\\?\C:\a\b.txt"), r"C:\a\b.txt");
        assert_eq!(
            normalize_final_path(r"\\?\UNC\srv\share\a.txt"),
            r"\\srv\share\a.txt"
        );
        assert_eq!(normalize_final_path(r"C:\plain"), r"C:\plain");
        assert_eq!(normalize_final_path(r"\\srv\share"), r"\\srv\share");
    }

    #[test]
    fn stdio_file_outside_write_grant_is_rejected() {
        let root = scratch_dir("in");
        let other = scratch_dir("out");
        let grants = vec![(root.clone(), true)];
        assert!(open_stdio_file(&other.join("x.txt"), &grants).is_err());
        // a read-only grant does not authorize it either
        let read_only = vec![(other.clone(), false)];
        assert!(open_stdio_file(&other.join("x.txt"), &read_only).is_err());
        assert!(!other.join("x.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn stdio_file_dotdot_escape_is_rejected() {
        let root = scratch_dir("dd");
        let other = scratch_dir("dd-out");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let grants = vec![(root.clone(), true)];
        let escape = root
            .join("sub")
            .join("..")
            .join("..")
            .join(other.file_name().unwrap())
            .join("x.txt");
        assert!(open_stdio_file(&escape, &grants).is_err());
        assert!(open_stdio_file(Path::new("relative.txt"), &grants).is_err());
        assert!(open_stdio_file(&root.join("a:stream"), &grants).is_err());
        assert!(!other.join("x.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn stdio_file_junction_or_symlink_out_of_grant_is_rejected() {
        let root = scratch_dir("jn");
        let other = scratch_dir("jn-out");
        let grants = vec![(root.clone(), true)];
        // directory junction inside the grant pointing outside
        let junction = root.join("link");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&other)
            .output()
            .unwrap();
        assert!(status.status.success(), "mklink /J failed");
        assert!(open_stdio_file(&junction.join("x.txt"), &grants).is_err());
        assert!(!other.join("x.txt").exists());
        // file symlink inside the grant pointing at an outside file (needs symlink privilege)
        let victim = other.join("victim.txt");
        std::fs::write(&victim, b"keep").unwrap();
        let link = root.join("file-link.txt");
        if std::os::windows::fs::symlink_file(&victim, &link).is_ok() {
            assert!(open_stdio_file(&link, &grants).is_err());
            assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        }
        let _ = std::fs::remove_dir(&junction);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn stdio_file_hard_link_to_outside_file_is_rejected() {
        let root = scratch_dir("hl");
        let other = scratch_dir("hl-out");
        let grants = vec![(root.clone(), true)];
        let victim = other.join("victim.txt");
        std::fs::write(&victim, b"keep").unwrap();
        let link = root.join("hard.txt");
        std::fs::hard_link(&victim, &link).unwrap();
        assert!(matches!(
            open_stdio_file(&link, &grants),
            Err(SandboxError::InvalidPolicy(_))
        ));
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        // a freshly created file has exactly one link
        let fresh = open_stdio_file(&root.join("fresh.txt"), &grants).unwrap();
        assert_eq!(file_link_count(&fresh).unwrap(), 1);
        drop(fresh);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other);
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
        let _used = ContainerProfile::ensure(&used).unwrap();
        let _unused = ContainerProfile::ensure(&unused).unwrap();
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
        let _profile = ContainerProfile::ensure(&foreign).unwrap();
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
        let _ = ContainerProfile::ensure(&name).unwrap();
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
            let _ = ContainerProfile::ensure(name).unwrap();
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

    #[test]
    fn the_profile_key_is_the_old_grant_key_until_a_deny_or_loopback_is_added() {
        let grants = vec![
            (PathBuf::from(r"D:\work"), true),
            (PathBuf::from(r"C:\tools"), false),
        ];
        let old = profile_key(&grants, &[], false);
        assert_eq!(
            old,
            vec![r"r:c:\tools".to_string(), r"w:d:\work".to_string()]
        );
        let denied = profile_key(&grants, &[PathBuf::from(r"D:\work\secrets")], false);
        let loopback = profile_key(&grants, &[], true);
        assert_ne!(denied, old, "a different deny set is a different container");
        assert_ne!(loopback, old, "a loopback-exempt container is its own SID");
        assert_ne!(profile_name(&denied), profile_name(&old));
        assert_ne!(profile_name(&loopback), profile_name(&old));
        // Order of the inputs does not matter.
        let mut reversed = grants.clone();
        reversed.reverse();
        assert_eq!(profile_key(&reversed, &[], false), old);
    }

    #[test]
    fn allow_and_deny_leases_never_share_a_registry_key() {
        let path = Path::new(r"D:\work\secrets");
        let allow = lease_key(LeaseKind::Allow, "S-1-15-2-1-2-3", path);
        let deny = lease_key(LeaseKind::Deny, "S-1-15-2-1-2-3", path);
        assert_ne!(allow, deny);
        // The ancestor sweep reads allow keys as `<sid>|<path>`.
        assert!(allow.starts_with("S-1-15-2-1-2-3|"), "{allow}");
        assert!(deny.starts_with("deny|"), "{deny}");
    }

    #[test]
    fn deny_targets_plans_existing_paths_skips_missing_ones_and_refuses_the_hostile() {
        let root = scratch_dir("deny-targets");
        let secrets = root.join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        std::fs::write(secrets.join("a.pem"), "x").unwrap();
        let policy = SandboxPolicy::new()
            .deny_read(&secrets)
            .deny_read(root.join("not-yet"))
            .deny_read_glob(format!("{}\\secrets\\*.pem", root.display()));
        let targets = deny_targets(&policy).unwrap();
        let keys: Vec<String> = targets.iter().map(|p| lexical_path_key(p)).collect();
        assert!(keys.contains(&lexical_path_key(&secrets)), "{keys:?}");
        assert!(
            keys.contains(&lexical_path_key(&secrets.join("a.pem"))),
            "{keys:?}"
        );
        assert!(
            !keys.iter().any(|k| k.ends_with("not-yet")),
            "a missing path has no object to deny: {keys:?}"
        );
        // A filesystem root and a relative path fail the spawn; they are never dropped.
        let root_policy = SandboxPolicy {
            deny_read: vec![DenyReadEntry::Path(PathBuf::from(r"C:\"))],
            ..SandboxPolicy::default()
        };
        assert!(matches!(
            deny_targets(&root_policy),
            Err(SandboxError::InvalidPolicy(_))
        ));
        let relative = SandboxPolicy {
            deny_read: vec![DenyReadEntry::Path(PathBuf::from("relative"))],
            ..SandboxPolicy::default()
        };
        assert!(deny_targets(&relative).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_only_mode_is_refused_by_the_appcontainer_backend() {
        let policy = SandboxPolicy::new().mode(ConfinementMode::WriteOnly);
        assert!(matches!(
            prepare_container(&policy, AncestorLeasePolicy::Strict),
            Err(SandboxError::Unsupported(_))
        ));
    }

    #[test]
    fn a_deny_ace_is_added_and_revoked_on_a_scratch_folder() {
        let root = scratch_dir("deny-ace");
        // A SID that belongs to no container: the ACE is written on a folder this test owns and
        // removed again, and nothing outside that folder is touched.
        let sid = LocalSid::parse("S-1-15-2-1-2-3-4-5-6-7").unwrap();
        let lease = GrantLease::deny(sid.0, "S-1-15-2-1-2-3-4-5-6-7", &root).unwrap();
        assert!(lease_registry().lock().unwrap().contains_key(&lease_key(
            LeaseKind::Deny,
            "S-1-15-2-1-2-3-4-5-6-7",
            &root
        )));
        drop(lease);
        assert!(!lease_registry().lock().unwrap().contains_key(&lease_key(
            LeaseKind::Deny,
            "S-1-15-2-1-2-3-4-5-6-7",
            &root
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- Ancestor policy of the pipe-pair launch: what BestEffort relaxes and what stays fatal ----

    #[test]
    fn only_the_pipe_pair_launch_takes_ancestors_best_effort() {
        assert_eq!(ONE_SHOT_ANCESTOR_POLICY, AncestorLeasePolicy::Strict);
        assert_eq!(PIPE_PAIR_ANCESTOR_POLICY, AncestorLeasePolicy::BestEffort);
    }

    #[test]
    fn pipe_pair_launch_still_refuses_network_before_any_lease() {
        // The relaxed ancestor policy does not touch the network capability: Allow is refused
        // before a profile is created. The child-side proof is the pipe-pair test in
        // tests/windows_outbound_denial.rs, which runs this same path.
        let policy = SandboxPolicy::new().network(NetworkPolicy::Allow);
        let command = SandboxCommand::new("rightkit-never-launched.exe");
        let refused = spawn_with_pipe_pair(&policy, &command, "--remote-debugging-io-pipes", None);
        assert!(matches!(
            refused.err(),
            Some(SandboxError::InvalidPolicy(_))
        ));
    }

    #[test]
    fn a_filesystem_root_grant_is_refused_under_best_effort_too() {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let root = PathBuf::from(format!("{drive}\\"));
        let policy = SandboxPolicy::new().allow_write(&root);
        for ancestors in [AncestorLeasePolicy::Strict, AncestorLeasePolicy::BestEffort] {
            let refused = prepare_container(&policy, ancestors);
            assert!(
                matches!(refused.err(), Some(SandboxError::InvalidPolicy(_))),
                "a root grant must fail before any profile or lease exists"
            );
        }
    }

    /// Gate for the tests that use the real user's AppContainer store. When the body is allowed
    /// to run it prints a marker, so a log proves the test did not early-return.
    fn real_store_body(test: &str) -> bool {
        if !real_user_stores_allowed(test) {
            return false;
        }
        println!("[sandbox] REAL-USER-STORE body ran: {test}");
        true
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        fn CreateRestrictedToken(
            existing: *mut c_void,
            flags: u32,
            disable_sid_count: u32,
            sids_to_disable: *const c_void,
            delete_privilege_count: u32,
            privileges_to_delete: *const c_void,
            restricted_sid_count: u32,
            sids_to_restrict: *const c_void,
            new_token: *mut *mut c_void,
        ) -> i32;
        fn ImpersonateLoggedOnUser(token: *mut c_void) -> i32;
        fn RevertToSelf() -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    /// Impersonates, on this thread only, the process token with every privilege removed
    /// and BUILTIN\Administrators (S-1-5-32-544) deny-only (`CreateRestrictedToken` with
    /// `DISABLE_MAX_PRIVILEGE` plus that SID in `SidsToDisable`), like a UAC-filtered token.
    /// Privileges alone do not make a standard user: an elevated token keeps Administrators
    /// enabled, and objects it creates are owned by Administrators (the token's default
    /// owner), so the token still matches the owner and gets implicit `WRITE_DAC`, which
    /// the `OWNER RIGHTS` ACE in `Unleasable` does not cap for that group-owner match.
    /// Reverted and closed on drop.
    struct StandardUserImpersonation(*mut c_void);

    impl StandardUserImpersonation {
        fn start() -> Self {
            const TOKEN_ALL_ACCESS: u32 = 0xF01FF;
            const DISABLE_MAX_PRIVILEGE: u32 = 0x1;
            let administrators = LocalSid::parse("S-1-5-32-544")
                .unwrap_or_else(|e| panic!("precondition: parse the Administrators SID: {e}"));
            let disable = [SidAndAttributes {
                sid: administrators.0,
                attributes: 0,
            }];
            let mut process_token = std::ptr::null_mut();
            // SAFETY: valid out-pointer; the current-process pseudo handle needs no closing.
            let opened = unsafe {
                OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut process_token)
            };
            assert!(
                opened != 0,
                "precondition: open the process token: {}",
                io::Error::last_os_error()
            );
            let mut restricted = std::ptr::null_mut();
            // SAFETY: `disable` holds one live SID owned by `administrators` for the call;
            // null privilege arrays with zero counts plus DISABLE_MAX_PRIVILEGE remove every
            // privilege and restrict no SID; valid out-pointer; process_token opened above.
            let created = unsafe {
                CreateRestrictedToken(
                    process_token,
                    DISABLE_MAX_PRIVILEGE,
                    1,
                    disable.as_ptr().cast(),
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    &mut restricted,
                )
            };
            let create_error = io::Error::last_os_error();
            // SAFETY: process_token was opened above and is closed exactly once here.
            unsafe { CloseHandle(process_token) };
            assert!(
                created != 0,
                "precondition: create a standard-user token without privileges: {create_error}"
            );
            let guard = Self(restricted);
            // SAFETY: `restricted` is a primary token owned by `guard`; reverted in Drop.
            let impersonated = unsafe { ImpersonateLoggedOnUser(restricted) };
            assert!(
                impersonated != 0,
                "precondition: impersonate the standard-user token: {}",
                io::Error::last_os_error()
            );
            guard
        }
    }

    impl Drop for StandardUserImpersonation {
        fn drop(&mut self) {
            // SAFETY: reverts this thread's impersonation begun in `start`, then closes the token
            // that `start` created, exactly once.
            unsafe {
                RevertToSelf();
                CloseHandle(self.0);
            }
        }
    }

    /// Runs `body` on this thread under a standard-user token. The DACL refusals the
    /// `best_effort_still_fails_*` tests need are only real for a token without privileges
    /// and without enabled Administrators: an elevated runner's token can rewrite any DACL
    /// (enabled Administrators matches the group owner of objects it creates and keeps
    /// implicit `WRITE_DAC`), so the same write succeeds there.
    fn as_standard_user<T>(body: impl FnOnce() -> T) -> T {
        let _standard = StandardUserImpersonation::start();
        body()
    }

    /// Read the DACL of `path` the way a lease does before it writes (GetNamedSecurityInfoW).
    fn dacl_is_readable(path: &Path) -> io::Result<()> {
        let mut wide_path = wide(path);
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: NUL-terminated path, valid out-pointers, descriptor freed once.
        let got = unsafe {
            GetNamedSecurityInfoW(
                wide_path.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if got != 0 {
            return Err(io::Error::from_raw_os_error(got as i32));
        }
        // SAFETY: allocated by the call above.
        unsafe { LocalFree(descriptor) };
        Ok(())
    }

    /// A scratch directory (under this test's own temp tree) that the calling token cannot
    /// re-ACL. Only a token without privileges and without enabled Administrators is refused:
    /// an elevated token can rewrite any DACL (privileges, plus enabled Administrators
    /// matching the group owner), which is why the callers run under
    /// `as_standard_user`. An explicit WRITE_DAC deny is not enough: the
    /// owner holds implicit READ_CONTROL | WRITE_DAC unless an OWNER RIGHTS (S-1-3-4) ACE is
    /// present. So the DACL is: the user may modify (no WRITE_DAC, no WRITE_OWNER) and
    /// OWNER RIGHTS may read, traverse and delete (enough for cleanup), nothing else.
    /// Inheritance is cut so no ALL APPLICATION PACKAGES grant leaks in and makes the directory
    /// look readable to a container. Only this scratch directory's ACL changes.
    struct Unleasable(PathBuf);
    impl Unleasable {
        fn new(tag: &str) -> Self {
            let dir = scratch_dir(tag);
            let user = format!("*{}", current_user_sid().unwrap());
            let icacls = format!("{}\\System32\\icacls.exe", system_root());
            let run = |args: &[&str]| {
                let status = std::process::Command::new(&icacls)
                    .arg(&dir)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(status.status.success(), "icacls {args:?} failed");
            };
            // Order matters: the OWNER RIGHTS ACE goes last, because once it is there the owner
            // can no longer change the DACL.
            run(&["/inheritance:r", "/grant", &format!("{user}:(OI)(CI)(M)")]);
            run(&["/grant", "*S-1-3-4:(OI)(CI)(RX,D,DC)"]);
            let locked = Self(dir);
            locked.assert_precondition();
            locked
        }

        /// The DACL reads, and a real DACL write with the calling token is refused with
        /// ERROR_ACCESS_DENIED: the tests exercise write refusal, not an unreadable DACL. Call it
        /// only inside `as_standard_user`; the message names the precondition it checks.
        fn assert_precondition(&self) {
            dacl_is_readable(&self.0)
                .unwrap_or_else(|e| panic!("DACL of {} must be readable: {e}", self.0.display()));
            let probe = LocalSid::parse("S-1-15-2-1-2-3-4-5-6-9").unwrap();
            let write = apply_sid_path_entry(
                &self.0,
                probe.0,
                GRANT_ACCESS,
                permissions_for(false),
                inheritance_for(&self.0),
            );
            match write {
                Err(e) if e.raw_os_error() == Some(5) => {}
                other => panic!(
                    "precondition not met: a DACL write on {} must fail with ERROR_ACCESS_DENIED \
                     (5) for the calling token, got {other:?}; this host cannot establish the \
                     standard-user refusal the test needs",
                    self.0.display()
                ),
            }
            dacl_is_readable(&self.0).expect("the DACL stays readable after the refused write");
        }
    }
    impl Drop for Unleasable {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn profile_of(grants: &[(PathBuf, bool)], denies: &[PathBuf]) -> Profiles {
        Profiles(vec![profile_name(&profile_key(grants, denies, false))])
    }

    fn prepare(
        grants: Vec<(PathBuf, bool)>,
        denies: Vec<PathBuf>,
        ancestors: AncestorLeasePolicy,
    ) -> Result<PreparedContainer, SandboxError> {
        prepare_container_from(grants, denies, false, false, Vec::new(), ancestors)
    }

    #[test]
    fn best_effort_prepares_a_user_owned_grant_and_leases_it_strictly() {
        if !real_store_body("best_effort_prepares_a_user_owned_grant_and_leases_it_strictly") {
            return;
        }
        // Standard user: the ancestors above the temp tree (C:\Users) refuse the ACE and are
        // skipped; administrator: they take it. Either way the grant itself is leased.
        let dir = scratch_dir("be-ok");
        let grants = vec![(dir.clone(), true)];
        let _cleanup = profile_of(&grants, &[]);
        let prepared = prepare(grants, Vec::new(), AncestorLeasePolicy::BestEffort)
            .unwrap_or_else(|e| panic!("best effort preparation failed: {e}"));
        let key = lease_key(LeaseKind::Allow, &prepared.profile.sid_text, &dir);
        assert!(lease_registry().lock().unwrap().contains_key(&key));
        drop(prepared);
        assert!(!lease_registry().lock().unwrap().contains_key(&key));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn best_effort_still_fails_when_the_grant_itself_cannot_be_leased() {
        if !real_store_body("best_effort_still_fails_when_the_grant_itself_cannot_be_leased") {
            return;
        }
        as_standard_user(|| {
            let locked = Unleasable::new("be-grant");
            let grants = vec![(locked.0.clone(), true)];
            let _cleanup = profile_of(&grants, &[]);
            let failed = prepare(grants, Vec::new(), AncestorLeasePolicy::BestEffort);
            match failed.err() {
                Some(SandboxError::Io(e)) => {
                    assert!(e.to_string().contains("lease "), "{e}");
                    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
                }
                other => panic!("a grant that cannot be leased must fail the launch: {other:?}"),
            }
        });
    }

    #[test]
    fn best_effort_still_fails_when_a_deny_carve_out_cannot_be_leased() {
        if !real_store_body("best_effort_still_fails_when_a_deny_carve_out_cannot_be_leased") {
            return;
        }
        as_standard_user(|| {
            let ok = scratch_dir("be-deny-ok");
            let locked = Unleasable::new("be-deny");
            let grants = vec![(ok.clone(), true)];
            let denies = vec![locked.0.clone()];
            let _cleanup = profile_of(&grants, &denies);
            let failed = prepare(grants, denies, AncestorLeasePolicy::BestEffort);
            match failed.err() {
                Some(SandboxError::Io(e)) => {
                    assert!(e.to_string().contains("deny "), "{e}");
                    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
                }
                other => panic!("a deny that cannot be leased must fail the launch: {other:?}"),
            }
            let _ = std::fs::remove_dir_all(&ok);
        });
    }

    #[test]
    fn best_effort_still_fails_when_the_image_directory_cannot_be_leased() {
        if !real_store_body("best_effort_still_fails_when_the_image_directory_cannot_be_leased") {
            return;
        }
        as_standard_user(|| {
            let ok = scratch_dir("be-image-ok");
            let locked = Unleasable::new("be-image");
            let grants = vec![(ok.clone(), true)];
            let _cleanup = profile_of(&grants, &[]);
            let prepared = prepare(grants, Vec::new(), AncestorLeasePolicy::BestEffort)
                .unwrap_or_else(|e| panic!("best effort preparation failed: {e}"));
            // The image lives in a directory no container can be granted; the launch must stop
            // before CreateProcess instead of starting a child that cannot execute its image.
            let command = SandboxCommand::new(locked.0.join("image.exe"));
            let failed = launch_container(
                &prepared,
                locked.0.join("image.exe"),
                &command,
                &EnvPolicy::default(),
                Some("--remote-debugging-io-pipes"),
                None,
                StdinMode::Null,
            );
            match failed.err() {
                Some(SandboxError::Io(e)) => {
                    assert!(e.to_string().contains("lease image"), "{e}");
                    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
                }
                other => panic!("an unleasable image directory must fail the launch: {other:?}"),
            }
            drop(prepared);
            let _ = std::fs::remove_dir_all(&ok);
        });
    }

    #[test]
    fn git_ceilings_are_joined_for_the_environment() {
        let leases = LeaseSet::default();
        leases
            .git_ceilings
            .lock()
            .unwrap()
            .extend([PathBuf::from(r"C:\Users"), PathBuf::from(r"C:\Users\x")]);
        let value = std::env::join_paths(leases.git_ceilings.lock().unwrap().iter())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(value, r"C:\Users;C:\Users\x");
    }
}
