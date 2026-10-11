//! Owner-only files and directories.
//!
//! Donor: CodeRight `private_fs` (perm/atomic/sweep). Unix uses mode bits applied at
//! creation time (never chmod-after-create for new files); Windows applies one protected
//! owner-only DACL through native security APIs. There is no weaker fallback: a failed native
//! step is an error, never a partial hardening reported as success.

use crate::atomic::{atomic_write_with, AtomicOptions};
use crate::error::{FsError, Result};
use std::path::{Path, PathBuf};

pub const FILE_MODE_OWNER_ONLY: u32 = 0o600;
pub const DIR_MODE_OWNER_ONLY: u32 = 0o700;

/// Narrow an existing file or directory to owner-only. A missing path is a no-op.
/// Symlinks are never followed on unix: a symlink is refused rather than chmod-ing its target.
/// On macOS an entry that carries an extended ACL is **refused** ([`FsError::UnsafeAcl`]) after its
/// mode has been narrowed: the ACL is left as it is, because removing it cannot revoke access
/// through a descriptor somebody already holds open, so a repair would report an owner-only
/// entry that was not.
pub fn restrict_owner_only(path: &Path) -> Result<()> {
    restrict_owner_only_impl(path)
}

#[cfg(unix)]
fn restrict_owner_only_impl(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(FsError::io(path, e)),
    };
    if meta.file_type().is_symlink() {
        return Err(FsError::Symlink(path.to_path_buf()));
    }
    let mode = if meta.is_dir() {
        DIR_MODE_OWNER_ONLY
    } else {
        FILE_MODE_OWNER_ONLY
    };
    if meta.permissions().mode() & 0o7777 != mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| FsError::io(path, e))?;
    }
    // The mode bits are only half of the guarantee: on macOS an ACL `allow` entry is evaluated
    // before them, so a `0600` file can still be readable by others. An ACL is not removed (that
    // cannot revoke an open descriptor); the entry is refused, so the caller knows it is not
    // owner-only.
    if cfg!(target_os = "macos") && (meta.is_file() || meta.is_dir()) {
        let file = open_no_follow(path).map_err(|e| FsError::io(path, e))?;
        crate::acl::refuse_acl(&file, path)?;
    }
    Ok(())
}

/// Opens `path` read-only without following a link at its name.
#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn restrict_owner_only_impl(_path: &Path) -> Result<()> {
    Ok(())
}

/// True when no group/other access exists on unix: no group or other mode bits and, on macOS, no
/// extended ACL (an `allow` entry is evaluated before the mode bits). On macOS an entry that
/// cannot be opened to read its ACL is reported as not owner-only, because that cannot be
/// proven. On Windows true when the DACL is not inherited-open (approximated by a successful
/// hardening having been applied: always true here, callers needing proof use
/// `restrict_owner_only` and trust its error).
pub fn is_owner_only(path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::symlink_metadata(path).map_err(|e| FsError::io(path, e))?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Ok(false);
        }
        if cfg!(target_os = "macos") && (meta.is_file() || meta.is_dir()) {
            return match open_no_follow(path) {
                Ok(file) => Ok(!crate::acl::has_allow_acl(&file, path)?),
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Ok(false),
                Err(e) => Err(FsError::io(path, e)),
            };
        }
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(true)
    }
}

/// Create `dir` and any missing ancestors. Each *newly created* directory is owner-only
/// before a child is created inside it; pre-existing ancestors are left alone, and the leaf
/// is always hardened. A symlinked leaf is refused. This does not sync directories: it is not a
/// durable operation (the durable form is [`crate::atomic::create_dir_all_durable`]). On macOS a
/// directory is not created under a parent whose ACL would give it an inheritable `allow` entry, and
/// one that got an `allow` entry anyway is removed again ([`FsError::UnsafeAcl`]).
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    if dir.as_os_str().is_empty() {
        return Err(FsError::io(
            dir,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty path"),
        ));
    }
    let mut missing: Vec<PathBuf> = Vec::new();
    for ancestor in dir.ancestors() {
        if ancestor.as_os_str().is_empty() || ancestor.symlink_metadata().is_ok() {
            break;
        }
        missing.push(ancestor.to_path_buf());
    }
    for path in missing.iter().rev() {
        create_private_dir_one(path)?;
    }
    let meta = std::fs::symlink_metadata(dir).map_err(|e| FsError::io(dir, e))?;
    if meta.file_type().is_symlink() {
        return Err(FsError::Symlink(dir.to_path_buf()));
    }
    if !meta.is_dir() {
        return Err(FsError::NotADirectory(dir.to_path_buf()));
    }
    restrict_owner_only(dir)
}

fn create_private_dir_one(path: &Path) -> Result<()> {
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::DirBuilderExt;
        // macOS: refuse a parent whose ACL would give the new directory an inheritable `allow` entry.
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            crate::acl::guard_parent_path(parent, path)?;
        }
        let made = std::fs::DirBuilder::new()
            .mode(DIR_MODE_OWNER_ONLY)
            .create(path);
        if made.is_ok() {
            // A directory that carries an `allow` entry anyway is empty: remove it and refuse.
            if let Err(error) = crate::acl::refuse_acl_path(path) {
                let _ = std::fs::remove_dir(path);
                return Err(error);
            }
        }
        made
    };
    #[cfg(not(unix))]
    let result = std::fs::create_dir(path);
    match result {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(FsError::io(path, e)),
    }
    #[cfg(not(unix))]
    restrict_owner_only(path)?;
    Ok(())
}

/// Atomically write an owner-only file (the temp inode is owner-only from creation, so the
/// secret is never visible under the destination name with a permissive mode).
pub fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    atomic_write_with(path, AtomicOptions::private(), |f| f.write_all(bytes))
}

pub fn write_private_text(path: &Path, text: &str) -> Result<()> {
    write_private_file(path, text.as_bytes())
}

/// Exclusively create an owner-only file, never following a symlink at `path`.
/// For files that are appended to or locked rather than replaced (logs, lock files).
///
/// The file is owner-only from the instant it exists: Unix creates it `0600`, Windows creates it
/// with an owner-only security descriptor passed to `CreateFileW`, so no principal the parent's
/// inherited ACL admits can open it before it is restricted and keep a readable handle. A name
/// shaped like a temp name this crate generates is refused ([`FsError::BadName`]); a sweep would
/// delete such a file.
pub fn create_private_new(path: &Path) -> Result<std::fs::File> {
    if let Some(name) = path.file_name() {
        crate::atomic::reject_reserved_name(&name.to_string_lossy())?;
    }
    create_private_new_impl(path)
}

#[cfg(windows)]
fn create_private_new_impl(path: &Path) -> Result<std::fs::File> {
    create_new_owner_only(path).map_err(|e| FsError::io(path, e))
}

#[cfg(not(windows))]
fn create_private_new_impl(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(FILE_MODE_OWNER_ONLY)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    // macOS: refuse a parent whose ACL would give the new file an inheritable `allow` entry, before
    // creating.
    #[cfg(unix)]
    crate::acl::guard_parent_path(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
        path,
    )?;
    let file = options.open(path).map_err(|e| FsError::io(path, e))?;
    #[cfg(unix)]
    // A file that carries an `allow` entry anyway is still empty: remove it and refuse.
    if let Err(error) = crate::acl::refuse_acl(&file, path) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    #[cfg(not(unix))]
    restrict_owner_only(path)?;
    Ok(file)
}

/// Owner-only repair for the named entries directly inside `dir`, plus `dir` itself.
/// One level deep and by exact name only: foreign entries are untouched and no symlink is
/// followed. Returns how many entries were narrowed.
pub fn sweep_owner_only(dir: &Path, owned_names: &[&str]) -> Result<usize> {
    restrict_owner_only(dir)?;
    let mut narrowed = 0;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(FsError::io(dir, e)),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if owned_names.iter().any(|owned| name == *owned)
            && (cfg!(windows) || !is_owner_only(&entry.path())?)
        {
            restrict_owner_only(&entry.path())?;
            narrowed += 1;
        }
    }
    Ok(narrowed)
}

#[cfg(windows)]
fn restrict_owner_only_impl(path: &Path) -> crate::error::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    // Fail closed. The native call replaces the whole DACL (one protected ACE for the current
    // user, nothing inherited). An `icacls /inheritance:r /grant:r` fallback is not equivalent: it
    // only replaces the grants for the one SID, so an explicit ACE for another principal (an
    // Everyone read ACE on the destination, say) would survive and the result would be reported
    // as owner-only.
    native_restrict_owner_only(path)
}

#[cfg(windows)]
fn current_process_token_user(path: &Path) -> crate::error::Result<Vec<usize>> {
    use std::ffi::c_void;

    type Handle = *mut c_void;
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER: u32 = 1;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
        fn GetTokenInformation(
            token: Handle,
            class: u32,
            buffer: *mut c_void,
            len: u32,
            needed: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
    }

    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(crate::error::FsError::io(
                path,
                std::io::Error::last_os_error(),
            ));
        }
        let mut needed = 0;
        let _ = GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            let error = std::io::Error::last_os_error();
            let _ = CloseHandle(token);
            return Err(crate::error::FsError::io(path, error));
        }
        let words = (needed as usize).div_ceil(std::mem::size_of::<usize>());
        let mut token_info = vec![0usize; words];
        if GetTokenInformation(
            token,
            TOKEN_USER,
            token_info.as_mut_ptr().cast(),
            needed,
            &mut needed,
        ) == 0
        {
            let error = std::io::Error::last_os_error();
            let _ = CloseHandle(token);
            return Err(crate::error::FsError::io(path, error));
        }
        let _ = CloseHandle(token);
        Ok(token_info)
    }
}

#[cfg(windows)]
pub(crate) fn current_process_sid_string(path: &Path) -> crate::error::Result<String> {
    use std::ffi::c_void;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertSidToStringSidW(sid: *mut c_void, string_sid: *mut *mut u16) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    let token_info = current_process_token_user(path)?;
    unsafe {
        let sid = *(token_info.as_ptr().cast::<*mut c_void>());
        let mut sid_text = std::ptr::null_mut();
        if ConvertSidToStringSidW(sid, &mut sid_text) == 0 {
            return Err(crate::error::FsError::io(
                path,
                std::io::Error::last_os_error(),
            ));
        }
        let sid_len = (0..).take_while(|&i| *sid_text.add(i) != 0).count();
        let sid_string = String::from_utf16_lossy(std::slice::from_raw_parts(sid_text, sid_len));
        let _ = LocalFree(sid_text.cast());
        Ok(sid_string)
    }
}

#[cfg(windows)]
fn native_restrict_owner_only(path: &Path) -> crate::error::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

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

    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    const SE_FILE_OBJECT: u32 = 1;
    const GENERIC_ALL: u32 = 0x1000_0000;
    const SET_ACCESS: i32 = 2;
    const NO_MULTIPLE_TRUSTEE: i32 = 0;
    const TRUSTEE_IS_SID: i32 = 0;
    const TRUSTEE_IS_USER: i32 = 1;
    const SUB_CONTAINERS_AND_OBJECTS_INHERIT: u32 = 0x3;

    #[link(name = "advapi32")]
    unsafe extern "system" {
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
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    let token_info = current_process_token_user(path)?;
    unsafe {
        let sid = *(token_info.as_ptr().cast::<*mut c_void>());
        let mut entry = ExplicitAccessW {
            access_permissions: GENERIC_ALL,
            access_mode: SET_ACCESS,
            inheritance: if path.is_dir() {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                0
            },
            trustee: TrusteeW {
                multiple_trustee: std::ptr::null_mut(),
                multiple_trustee_operation: NO_MULTIPLE_TRUSTEE,
                trustee_form: TRUSTEE_IS_SID,
                trustee_type: TRUSTEE_IS_USER,
                name: sid.cast(),
            },
        };
        let mut dacl = std::ptr::null_mut();
        let acl_result = SetEntriesInAclW(1, &mut entry, std::ptr::null_mut(), &mut dacl);
        if acl_result != 0 {
            return Err(crate::error::FsError::io(
                path,
                std::io::Error::from_raw_os_error(acl_result as i32),
            ));
        }
        let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let result = SetNamedSecurityInfoW(
            wide_path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null_mut(),
        );
        let _ = LocalFree(dacl);
        if result == 0 {
            Ok(())
        } else {
            Err(crate::error::FsError::io(
                path,
                std::io::Error::from_raw_os_error(result as i32),
            ))
        }
    }
}

/// A security descriptor built from SDDL: one protected ACE giving the current user full control,
/// freed on drop. A directory's ACE also inherits to what is created inside it. Used to create
/// files and directories owner-only from the start, with no window in which a wider inherited ACL
/// applies.
#[cfg(windows)]
pub(crate) struct SecurityDescriptor(pub(crate) *mut std::ffi::c_void);

#[cfg(windows)]
impl SecurityDescriptor {
    pub(crate) fn owner_only(shown: &Path, directory: bool) -> crate::error::Result<Self> {
        use std::ffi::c_void;
        #[link(name = "advapi32")]
        unsafe extern "system" {
            fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
                string: *const u16,
                revision: u32,
                descriptor: *mut *mut c_void,
                size: *mut u32,
            ) -> i32;
        }
        let sid = current_process_sid_string(shown)?;
        let inherit = if directory { "OICI" } else { "" };
        let sddl: Vec<u16> = format!("D:P(A;{inherit};FA;;;{sid})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut raw = std::ptr::null_mut();
        // SAFETY: NUL-terminated SDDL; the API allocates the returned descriptor.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut raw,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(FsError::io(shown, std::io::Error::last_os_error()));
        }
        Ok(Self(raw))
    }
}

#[cfg(windows)]
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }
        // SAFETY: allocation returned by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// Exclusively creates `path` for writing with the owner-only security descriptor passed to
/// `CreateFileW`, so the file never exists with the parent's inherited ACL (no principal that ACL
/// admits can open it before it is restricted). Fails if the descriptor cannot be built, or if
/// `path` exists (a link at the name is never followed: `CREATE_NEW` refuses an existing name).
#[cfg(windows)]
pub(crate) fn create_new_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit_handle: i32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *const c_void,
            disposition: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
    }
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x7;
    const CREATE_NEW: u32 = 1;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

    let descriptor = SecurityDescriptor::owner_only(path, false).map_err(std::io::Error::other)?;
    let attributes = SecurityAttributes {
        length: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor: descriptor.0,
        inherit_handle: 0,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if wide[..wide.len() - 1].contains(&0) {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    // SAFETY: `wide` is NUL-terminated and `attributes` (with the descriptor it points to) outlives
    // the call; the handle is owned by the returned `File` on success.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ_WRITE_DELETE,
            (&attributes as *const SecurityAttributes).cast(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle as isize == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a fresh handle that nothing else owns.
    Ok(unsafe { std::fs::File::from_raw_handle(handle) })
}
