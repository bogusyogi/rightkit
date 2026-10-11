//! Extended ACLs under the owner-only guarantee (Unix).
//!
//! `0600` and `0700` constrain the BSD mode bits only. macOS evaluates a file's ACL before those
//! bits, and a new file or directory inherits the *inheritable* entries of its parent's ACL when
//! it is created. A parent that carries `everyone allow read,file_inherit` therefore makes every
//! `0600` file created below it readable by everyone, whatever the mode says.
//!
//! The guarantee this module gives, on macOS (it is a no-op elsewhere, see below):
//!
//! 1. **Before creating** a private file or directory, the parent's ACL is read. If it holds any
//!    inheritable `allow` entry the creation is refused with [`FsError::UnsafeAcl`] and nothing
//!    is created, so the new inode never exists with an ACL that admits another principal. (An
//!    inheritable `deny` entry cannot widen access and is accepted.)
//! 2. **After creating**, the new inode's own ACL is read through its descriptor and, if it has
//!    any entry (the parent's ACL changed between step 1 and the creation), removed and read back.
//!    If it cannot be removed the creation is refused with [`FsError::UnsafeAcl`] and the caller
//!    removes the inode. This closes only the race; step 1 is what keeps the file clean.
//! 3. **Repair and verification** of an existing inode ([`crate::restrict_owner_only`],
//!    [`crate::private::is_owner_only`]) look at the ACL as well as the mode bits.
//!
//! Linux and the other Unixes: this module does nothing. POSIX default ACLs on Linux are applied
//! to a new file masked by the creation mode, and a `0600` creation sets the mask to `---`, so
//! named-user and named-group entries grant nothing. That is the documented kernel behaviour and
//! is not checked here. Other ACL models (FreeBSD NFSv4 ACLs) are not handled.
//!
//! Every ACL call below is declared by hand against `<sys/acl.h>` on macOS and none of it has been
//! compiled or run in this tree's verification.

use crate::error::{FsError, Result};
use std::fs::File;
use std::path::Path;

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::{c_int, c_uint, c_void};
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    type Acl = *mut c_void;
    type Entry = *mut c_void;
    type Flagset = *mut c_void;

    /// `ACL_TYPE_EXTENDED`: the only ACL type macOS has.
    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: c_int = 0;
    const ACL_NEXT_ENTRY: c_int = -1;
    /// `ACL_EXTENDED_ALLOW`.
    const ACL_EXTENDED_ALLOW: c_int = 1;
    /// `ACL_ENTRY_FILE_INHERIT` and `ACL_ENTRY_DIRECTORY_INHERIT`.
    const ACL_ENTRY_FILE_INHERIT: c_uint = 1 << 5;
    const ACL_ENTRY_DIRECTORY_INHERIT: c_uint = 1 << 6;

    extern "C" {
        fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> Acl;
        fn acl_set_fd_np(fd: c_int, acl: Acl, acl_type: c_int) -> c_int;
        fn acl_init(count: c_int) -> Acl;
        fn acl_free(object: *mut c_void) -> c_int;
        fn acl_get_entry(acl: Acl, entry_id: c_int, entry: *mut Entry) -> c_int;
        fn acl_get_tag_type(entry: Entry, tag: *mut c_int) -> c_int;
        fn acl_get_flagset_np(object: *mut c_void, flagset: *mut Flagset) -> c_int;
        fn acl_get_flag_np(flagset: Flagset, flag: c_uint) -> c_int;
    }

    /// The ACL of `file`, or `None` when it has none or its filesystem cannot hold one.
    fn read(file: &File) -> io::Result<Option<Acl>> {
        // SAFETY: the descriptor is open for the call; the result is owned by the caller.
        let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if !acl.is_null() {
            return Ok(Some(acl));
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENOENT) | Some(libc::ENOTSUP) | Some(libc::EOPNOTSUPP) => Ok(None),
            _ => Err(error),
        }
    }

    /// Calls `visit` with each entry of `acl` in turn; true as soon as one call returns true.
    fn any_entry(acl: Acl, mut visit: impl FnMut(Entry) -> bool) -> bool {
        let mut which = ACL_FIRST_ENTRY;
        loop {
            let mut entry: Entry = std::ptr::null_mut();
            // SAFETY: `acl` is a live ACL and `entry` a valid out-parameter. The call fails
            // (nonzero) when there is no further entry.
            if unsafe { acl_get_entry(acl, which, &mut entry) } != 0 {
                return false;
            }
            if visit(entry) {
                return true;
            }
            which = ACL_NEXT_ENTRY;
        }
    }

    /// True when `file` has an ACL with at least one entry.
    pub fn has_acl(file: &File) -> io::Result<bool> {
        let Some(acl) = read(file)? else {
            return Ok(false);
        };
        let any = any_entry(acl, |_| true);
        // SAFETY: `acl` came from `acl_get_fd_np` and is freed once.
        unsafe { acl_free(acl) };
        Ok(any)
    }

    /// True when the ACL of directory `dir` has an `allow` entry that a new file or directory
    /// inherits. An entry whose type or flags cannot be read counts as inheritable (fail closed).
    pub fn dir_inherits_allow(dir: &File) -> io::Result<bool> {
        let Some(acl) = read(dir)? else {
            return Ok(false);
        };
        let found = any_entry(acl, |entry| {
            let mut tag: c_int = 0;
            // SAFETY: `entry` belongs to the live `acl`; `tag` is a valid out-parameter.
            if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 {
                return true;
            }
            if tag != ACL_EXTENDED_ALLOW {
                return false;
            }
            let mut flags: Flagset = std::ptr::null_mut();
            // SAFETY: as above; `flags` is a valid out-parameter.
            if unsafe { acl_get_flagset_np(entry, &mut flags) } != 0 {
                return true;
            }
            // SAFETY: `flags` was just returned for a live entry.
            let (file, directory) = unsafe {
                (
                    acl_get_flag_np(flags, ACL_ENTRY_FILE_INHERIT),
                    acl_get_flag_np(flags, ACL_ENTRY_DIRECTORY_INHERIT),
                )
            };
            file != 0 || directory != 0
        });
        // SAFETY: `acl` came from `acl_get_fd_np` and is freed once.
        unsafe { acl_free(acl) };
        Ok(found)
    }

    /// Replaces the ACL of `file` with an empty one, then reads it back: `Ok` only if none is left.
    pub fn remove_acl(file: &File) -> io::Result<()> {
        // SAFETY: `acl_init(0)` allocates an empty ACL that is freed below; the descriptor is open.
        let empty = unsafe { acl_init(0) };
        if empty.is_null() {
            return Err(io::Error::last_os_error());
        }
        let rc = unsafe { acl_set_fd_np(file.as_raw_fd(), empty, ACL_TYPE_EXTENDED) };
        let set_error = (rc != 0).then(io::Error::last_os_error);
        unsafe { acl_free(empty) };
        if let Some(error) = set_error {
            return Err(error);
        }
        if has_acl(file)? {
            return Err(io::Error::other("the ACL is still present after removal"));
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod sys {
    use std::fs::File;
    use std::io;

    pub fn has_acl(_file: &File) -> io::Result<bool> {
        Ok(false)
    }
    pub fn dir_inherits_allow(_dir: &File) -> io::Result<bool> {
        Ok(false)
    }
    pub fn remove_acl(_file: &File) -> io::Result<()> {
        Ok(())
    }
}

/// Step 1: refuse to create a private entry in the directory open as `parent` when it would
/// inherit an `allow` ACL entry. `shown` names the entry being created.
pub(crate) fn guard_parent(parent: &File, shown: &Path) -> Result<()> {
    match sys::dir_inherits_allow(parent) {
        Ok(false) => Ok(()),
        Ok(true) => Err(FsError::UnsafeAcl(shown.to_path_buf())),
        Err(e) => Err(FsError::io(shown, e)),
    }
}

/// [`guard_parent`] for a parent named by path. A parent that does not exist passes: the
/// creation that follows reports that failure itself.
pub(crate) fn guard_parent_path(parent: &Path, shown: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) {
        return Ok(());
    }
    match File::open(parent) {
        Ok(dir) => guard_parent(&dir, shown),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(FsError::io(parent, e)),
    }
}

/// Step 2: the entry this call just created must carry no ACL of its own. Removes one that is
/// there; an ACL that cannot be removed is [`FsError::UnsafeAcl`] and the caller removes the entry.
pub(crate) fn scrub_created(file: &File, shown: &Path) -> Result<()> {
    match sys::has_acl(file) {
        Ok(false) => Ok(()),
        Ok(true) => sys::remove_acl(file).map_err(|_| FsError::UnsafeAcl(shown.to_path_buf())),
        Err(e) => Err(FsError::io(shown, e)),
    }
}

/// [`scrub_created`] for a directory just created at `path` (off macOS: nothing to do).
pub(crate) fn scrub_created_path(path: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) {
        return Ok(());
    }
    let dir = File::open(path).map_err(|e| FsError::io(path, e))?;
    scrub_created(&dir, path)
}

/// Step 3: whether `file` carries an ACL (always false off macOS).
pub(crate) fn has_acl(file: &File, shown: &Path) -> Result<bool> {
    sys::has_acl(file).map_err(|e| FsError::io(shown, e))
}

/// Step 3: remove any ACL from `file`; [`FsError::UnsafeAcl`] if one stays.
pub(crate) fn strip(file: &File, shown: &Path) -> Result<()> {
    scrub_created(file, shown)
}
