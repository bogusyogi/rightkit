//! OS-level sandbox authentication for the checks that mark
//! `requires_network_sandbox`.
//!
//! Native Audit previously refused every check in that set unconditionally:
//! there was no code path that could produce a `SandboxReceipt`, so the
//! executor's `requires_network_sandbox && !sandbox.network` gate in
//! `executor.rs` always tripped. This module gives one platform (macOS) a
//! real authenticator: it writes a `sandbox-exec` profile, wraps the
//! requested command under it, and only then returns a receipt. The receipt
//! is authenticated because the OS sandbox is the thing that actually ran
//! the process, not an assertion made after the fact.
//!
//! Two profiles are supported:
//! - `DenyNetwork`: for checks that execute target-project code (build,
//!   lint, types, ...). Network access is denied; project files stay
//!   read/write so the tool can operate normally.
//! - `DenyNetworkDenyProjectWrite`: for project test execution. Network access
//!   and writes under the audited project root are denied.
//! - `AllowNetworkDenyProjectWrite`: for advisory checks that must reach a
//!   registry or vulnerability database (cargo_audit, cargo_deny, deps_cve,
//!   py_deps_cve, outdated, cargo_outdated). Network is allowed; writes
//!   under the project root are denied so the check cannot execute or
//!   mutate project code.
//!
//! No other platform has an authenticator yet. `authenticate` on
//! non-macOS returns `SandboxGap::UnsupportedPlatform` so callers keep
//! refusing rather than fabricate a receipt — a typed degradation, not a
//! silent pass.

use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxMode {
    DenyNetwork,
    DenyNetworkDenyProjectWrite,
    AllowNetworkDenyProjectWrite,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxAuthentication {
    pub id: String,
    pub network: bool,
    pub filesystem_scope: Option<String>,
    pub wrapped_executable: String,
    pub wrapped_args: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SandboxGap {
    /// No authenticator is implemented for this platform yet.
    UnsupportedPlatform,
    /// The sandbox tool itself is not present on this host.
    ToolMissing,
    /// The profile could not be written to disk.
    ProfileWriteFailed(String),
    /// A project-write-denied profile could not resolve a usable project root.
    ProjectRootUnavailable(String),
    /// A scoped IPC prerequisite could not be established or revalidated.
    IpcNamespaceInvalid(String),
}

impl SandboxGap {
    pub fn as_str(&self) -> String {
        match self {
            Self::UnsupportedPlatform => {
                "sandbox_unsupported_platform: no OS sandbox authenticator for this platform".into()
            }
            Self::ToolMissing => "sandbox_tool_missing: /usr/bin/sandbox-exec not found".into(),
            Self::ProfileWriteFailed(detail) => format!("sandbox_profile_write_failed: {detail}"),
            Self::ProjectRootUnavailable(detail) => {
                format!("sandbox_project_root_unavailable: {detail}")
            }
            Self::IpcNamespaceInvalid(detail) => format!("sandbox_ipc_namespace_invalid: {detail}"),
        }
    }
}

#[cfg(target_os = "macos")]
fn ipc_error(detail: impl std::fmt::Display) -> SandboxGap {
    SandboxGap::IpcNamespaceInvalid(detail.to_string())
}

#[cfg(target_os = "macos")]
fn require_empty_acl(path: &Path) -> Result<(), SandboxGap> {
    use std::{
        ffi::{c_char, c_int, c_void, CString},
        os::unix::ffi::OsStrExt,
    };
    // Apple Libc gen/filesec.c, sys/{fcntl,stat,acl}.h define this ABI.
    // Read only. Reject every nonempty ACL rather than interpret principals,
    // inherited entries or permissions. Unsupported/unreadable fails closed.
    unsafe extern "C" {
        fn filesec_init() -> *mut c_void;
        fn filesec_free(filesec: *mut c_void);
        fn filesec_query_property(
            filesec: *mut c_void,
            property: c_int,
            present: *mut c_int,
        ) -> c_int;
        fn filesec_get_property(filesec: *mut c_void, property: c_int, value: *mut c_void)
            -> c_int;
        #[cfg_attr(target_arch = "x86_64", link_name = "lstatx_np$INODE64")]
        fn lstatx_np(path: *const c_char, stat: *mut libc::stat, filesec: *mut c_void) -> c_int;
        fn acl_get_entry(acl: *mut c_void, which: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_free(object: *mut c_void) -> c_int;
    }
    let path = CString::new(path.as_os_str().as_bytes()).map_err(ipc_error)?;
    // acl_get_link_np alone conflates absent ACL with failed lookup. A fresh
    // filesec populated by successful lstatx_np proves which case occurred.
    let filesec = unsafe { filesec_init() };
    if filesec.is_null() {
        return Err(ipc_error("cannot allocate IPC ancestor filesec"));
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let status = unsafe { lstatx_np(path.as_ptr(), stat.as_mut_ptr(), filesec) };
    let error = std::io::Error::last_os_error();
    if status != 0 {
        unsafe { filesec_free(filesec) };
        return Err(ipc_error(format!(
            "cannot inspect IPC ancestor filesec: {error}"
        )));
    }
    let mut present = -1;
    let queried = unsafe { filesec_query_property(filesec, 5, &mut present) };
    if queried != 0 || present < 0 {
        unsafe { filesec_free(filesec) };
        return Err(ipc_error("cannot query IPC ancestor ACL presence"));
    }
    if present == 0 {
        unsafe { filesec_free(filesec) };
        return Ok(());
    }
    let mut acl: *mut c_void = std::ptr::null_mut();
    let copied = unsafe { filesec_get_property(filesec, 5, (&mut acl as *mut *mut c_void).cast()) };
    unsafe { filesec_free(filesec) };
    if copied != 0 || acl.is_null() || acl as usize <= 16 {
        return Err(ipc_error("cannot copy IPC ancestor ACL"));
    }
    let mut entry = std::ptr::null_mut();
    // Darwin acl_get_entry returns 0 on an entry, -1/EINVAL when FIRST_ENTRY
    // is beyond an empty valid ACL; it does not use POSIX's 1/0 convention.
    let status = unsafe { acl_get_entry(acl, 0, &mut entry) };
    let error = std::io::Error::last_os_error();
    let freed = unsafe { acl_free(acl) };
    if freed != 0 {
        return Err(ipc_error("cannot free IPC ancestor ACL"));
    }
    match (status, error.raw_os_error()) {
        (-1, Some(libc::EINVAL)) => Ok(()),
        (0, _) => Err(ipc_error("nonempty IPC ancestor ACL")),
        _ => Err(ipc_error("cannot enumerate IPC ancestor ACL")),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn canonical_owned_path(path: &Path) -> Result<PathBuf, SandboxGap> {
    use std::os::unix::fs::MetadataExt;
    if !path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(ipc_error("noncanonical IPC path"));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        let invalid = |detail: String| ipc_error(format!("{detail}: {}", current.display()));
        let meta =
            std::fs::symlink_metadata(&current).map_err(|error| invalid(error.to_string()))?;
        // Root/system ancestors may be root-owned; no other user may own an
        // ancestor that can replace the namespace. Reject symlinks throughout.
        if meta.file_type().is_symlink()
            || !meta.is_dir()
            || (meta.uid() != 0 && meta.uid() != unsafe { libc::geteuid() })
        {
            return Err(invalid(
                "symlink, non-directory or foreign-owned IPC ancestor".into(),
            ));
        }
        if meta.mode() & 0o022 != 0 && !(meta.uid() == 0 && meta.mode() & 0o1000 != 0) {
            return Err(invalid("IPC ancestor writable by another principal".into()));
        }
        require_empty_acl(&current).map_err(|error| match error {
            SandboxGap::IpcNamespaceInvalid(detail) => invalid(detail),
            other => other,
        })?;
    }
    let canonical = path.canonicalize().map_err(ipc_error)?;
    if canonical != path {
        return Err(ipc_error("IPC path is not canonical"));
    }
    Ok(canonical)
}

#[cfg(target_os = "macos")]
pub fn authenticate(
    executable: &str,
    args: &[String],
    cwd: &str,
    mode: SandboxMode,
    profile_dir: &Path,
) -> Result<SandboxAuthentication, SandboxGap> {
    use sha2::{Digest, Sha256};
    use std::fs;

    let sandbox_exec = PathBuf::from("/usr/bin/sandbox-exec");
    if !sandbox_exec.is_file() {
        return Err(SandboxGap::ToolMissing);
    }

    let filesystem_scope = match mode {
        SandboxMode::DenyNetworkDenyProjectWrite | SandboxMode::AllowNetworkDenyProjectWrite => {
            let root = Path::new(cwd).canonicalize().map_err(|error| {
                SandboxGap::ProjectRootUnavailable(format!(
                    "could not resolve project root {cwd:?}: {error}"
                ))
            })?;
            if !root.is_dir() {
                return Err(SandboxGap::ProjectRootUnavailable(format!(
                    "project root is not a directory: {}",
                    root.display()
                )));
            }
            root.to_str().map(str::to_owned).ok_or_else(|| {
                SandboxGap::ProjectRootUnavailable(format!(
                    "resolved project root is not UTF-8: {}",
                    root.display()
                ))
            })?
        }
        SandboxMode::DenyNetwork => cwd.to_owned(),
    };
    let profile = render_profile(mode, &filesystem_scope);
    let digest = {
        let mut hasher = Sha256::new();
        hasher.update(profile.as_bytes());
        hex::encode(hasher.finalize())
    };
    let profile_path = profile_dir.join(format!("audit-{digest}.sb"));
    fs::create_dir_all(profile_dir).map_err(|error| {
        SandboxGap::ProfileWriteFailed(format!("create_dir_all failed: {error}"))
    })?;
    fs::write(&profile_path, &profile)
        .map_err(|error| SandboxGap::ProfileWriteFailed(format!("write failed: {error}")))?;

    let mut wrapped_args: Vec<String> = vec![
        "-f".into(),
        profile_path.to_string_lossy().into_owned(),
        "--".into(),
        executable.to_string(),
    ];
    wrapped_args.extend(args.iter().cloned());

    Ok(SandboxAuthentication {
        id: format!("sandbox:macos:{digest}"),
        network: true,
        filesystem_scope: Some(filesystem_scope),
        wrapped_executable: sandbox_exec.to_string_lossy().into_owned(),
        wrapped_args,
    })
}

#[cfg(target_os = "macos")]
fn render_profile(mode: SandboxMode, cwd: &str) -> String {
    let escaped = cwd.replace('\\', "\\\\").replace('"', "\\\"");
    match mode {
        SandboxMode::DenyNetwork => format!(
            "(version 1)\n(allow default)\n(deny network*)\n(allow file-read* file-write* (subpath \"{escaped}\"))\n"
        ),
        SandboxMode::DenyNetworkDenyProjectWrite => format!(
            "(version 1)\n(allow default)\n(deny network*)\n(allow file-read* (subpath \"{escaped}\"))\n(deny file-write* (subpath \"{escaped}\"))\n"
        ),
        SandboxMode::AllowNetworkDenyProjectWrite => format!(
            "(version 1)\n(allow default)\n(allow network*)\n(allow file-read* (subpath \"{escaped}\"))\n(deny file-write* (subpath \"{escaped}\"))\n"
        ),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn authenticate(
    _executable: &str,
    _args: &[String],
    _cwd: &str,
    _mode: SandboxMode,
    _profile_dir: &Path,
) -> Result<SandboxAuthentication, SandboxGap> {
    Err(SandboxGap::UnsupportedPlatform)
}
