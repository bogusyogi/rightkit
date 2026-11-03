//! Windows: can Chrome's sandbox read the Chrome it is launched from?
//!
//! Chrome starts its network service and renderers inside an AppContainer. A process there
//! reads and executes only what the file's access list grants to `ALL APPLICATION PACKAGES`
//! (`S-1-15-2-1`) or, for the less privileged containers, `ALL RESTRICTED APPLICATION
//! PACKAGES` (`S-1-15-2-2`). `Program Files` carries both grants and a Chrome installed
//! there inherits them. A Chrome unpacked from an archive into a user or tool directory has
//! neither: Chrome logs "Sandbox cannot access executable ... Access is denied", its network
//! service crashes at startup, and a navigation issued meanwhile hangs or aborts.
//!
//! The launcher checks before it starts such a browser and returns
//! [`BrowserError::ChromeSandboxAccess`](crate::BrowserError::ChromeSandboxAccess) instead.
//! Reading the access list is decided on its text form (SDDL), so the decision itself is
//! tested on every host.

// The decision is compiled and tested everywhere; only Windows calls it.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

/// `ALL APPLICATION PACKAGES`.
pub(crate) const ALL_APPLICATION_PACKAGES: &str = "S-1-15-2-1";
/// `ALL RESTRICTED APPLICATION PACKAGES`.
pub(crate) const ALL_RESTRICTED_APPLICATION_PACKAGES: &str = "S-1-15-2-2";

/// `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`: what `(RX)` grants.
const READ_EXECUTE: u32 = 0x0012_00A9;
const GENERIC_ALL: u32 = 0x1000_0000;
const GENERIC_EXECUTE: u32 = 0x2000_0000;
const GENERIC_READ: u32 = 0x8000_0000;

/// The access mask an SDDL rights field stands for, as far as read and execute go.
fn rights_mask(rights: &str) -> u32 {
    let rights = rights.trim();
    if let Some(hex) = rights
        .strip_prefix("0x")
        .or_else(|| rights.strip_prefix("0X"))
    {
        return u32::from_str_radix(hex, 16).unwrap_or(0);
    }
    let mut mask = 0;
    let bytes = rights.as_bytes();
    for pair in bytes.chunks(2) {
        mask |= match pair {
            b"FA" | b"GA" => GENERIC_ALL,
            b"FR" | b"GR" => GENERIC_READ,
            b"FX" | b"GX" => GENERIC_EXECUTE,
            _ => 0,
        };
    }
    mask
}

fn allows_read_execute(mask: u32) -> bool {
    mask & GENERIC_ALL != 0
        || mask & READ_EXECUTE == READ_EXECUTE
        || (mask & GENERIC_READ != 0 && mask & GENERIC_EXECUTE != 0)
        || (mask & GENERIC_READ != 0 && mask & 0x0012_00A0 == 0x0012_00A0)
        || (mask & GENERIC_EXECUTE != 0 && mask & 0x0012_0089 == 0x0012_0089)
}

/// The AppContainer principals that the access list `sddl` does not grant read and execute.
///
/// Only allow entries that apply to the object itself count: an inherit-only entry (`IO`)
/// describes what children get. Deny entries are not weighed; the launcher only needs to
/// know that the grant is absent.
pub(crate) fn missing_grants(sddl: &str) -> Vec<&'static str> {
    let mut all = 0u32;
    let mut restricted = 0u32;
    for ace in sddl.split('(').skip(1) {
        let ace = ace.split(')').next().unwrap_or_default();
        let fields: Vec<&str> = ace.split(';').collect();
        if fields.len() < 6 || fields[0].trim() != "A" {
            continue;
        }
        let inherit_only = fields[1].as_bytes().chunks(2).any(|flag| flag == b"IO");
        if inherit_only {
            continue;
        }
        let mask = rights_mask(fields[2]);
        match fields[5].trim() {
            "AC" | ALL_APPLICATION_PACKAGES => all |= mask,
            ALL_RESTRICTED_APPLICATION_PACKAGES => restricted |= mask,
            _ => {}
        }
    }
    let mut missing = Vec::new();
    if !allows_read_execute(all) {
        missing.push(ALL_APPLICATION_PACKAGES);
    }
    if !allows_read_execute(restricted) {
        missing.push(ALL_RESTRICTED_APPLICATION_PACKAGES);
    }
    missing
}

/// The command that grants both principals read and execute on `directory` and all in it.
pub(crate) fn grant_command(directory: &Path) -> String {
    format!(
        "icacls \"{}\" /grant \"*{ALL_APPLICATION_PACKAGES}:(OI)(CI)(RX)\" \"*{ALL_RESTRICTED_APPLICATION_PACKAGES}:(OI)(CI)(RX)\" /T",
        directory.display()
    )
}

/// What the launcher reports when the sandbox cannot read the browser: the directory that
/// needs the grant (the executable's own) and the principals that lack it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MissingAccess {
    pub(crate) directory: PathBuf,
    pub(crate) principals: Vec<&'static str>,
}

/// `None` when Chrome's sandbox can read `executable`, or when that cannot be determined
/// (the access list is unreadable): only a grant known to be absent stops a launch.
#[cfg(windows)]
pub(crate) fn missing_access(executable: &Path) -> Option<MissingAccess> {
    let principals = missing_grants(&dacl_sddl(executable)?);
    if principals.is_empty() {
        return None;
    }
    Some(MissingAccess {
        directory: executable.parent().unwrap_or(executable).to_path_buf(),
        principals,
    })
}

/// Other systems have no AppContainer.
#[cfg(not(windows))]
pub(crate) fn missing_access(_executable: &Path) -> Option<MissingAccess> {
    None
}

/// The discretionary access list of `path` in SDDL text, inherited entries included.
#[cfg(windows)]
fn dacl_sddl(path: &Path) -> Option<String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const SE_FILE_OBJECT: i32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 4;
    const SDDL_REVISION_1: u32 = 1;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetNamedSecurityInfoW(
            name: *const u16,
            object_type: i32,
            information: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor: *const c_void,
            revision: u32,
            information: u32,
            text: *mut *mut u16,
            length: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    if name[..name.len() - 1].contains(&0) {
        return None;
    }
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated and outlives the call; the unused outputs are null,
    // which the function allows, and `descriptor` is a valid out-parameter.
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 || descriptor.is_null() {
        return None;
    }
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `descriptor` is the security descriptor the call above returned; `text` is a
    // valid out-parameter and the length output may be null.
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            std::ptr::null_mut(),
        )
    };
    let sddl = if converted != 0 && !text.is_null() {
        let mut len = 0;
        // SAFETY: the function returned a NUL-terminated string, so every offset up to the
        // NUL is readable.
        while unsafe { *text.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` code units were just read from this allocation.
        let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        // SAFETY: the caller frees the string with LocalFree, once.
        unsafe { LocalFree(text.cast()) };
        Some(value)
    } else {
        None
    };
    // SAFETY: the caller frees the descriptor with LocalFree, once.
    unsafe { LocalFree(descriptor) };
    sddl
}
