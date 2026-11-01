//! Per-user and machine folders asked of the operating system's account database or known-folder
//! API, never of environment variables (`HOME`, `USERPROFILE`, `ProgramFiles`, ...). A redirected
//! environment must not choose where the app looks for an executable or an app folder.

#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::path::PathBuf;

/// The current account's home folder from the account database (`getpwuid_r`), not `$HOME`.
#[cfg(target_os = "macos")]
pub(crate) fn account_home_dir() -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let mut buf = vec![0u8; 4096];
    loop {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `pwd`, `buf` and `result` are valid for the call; the strings in `pwd` point
        // into `buf`, which outlives their use below.
        let rc = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
        let path = PathBuf::from(OsStr::from_bytes(dir.to_bytes()));
        return (path.is_absolute()).then_some(path);
    }
}

/// A Windows known folder for the current user, or `None` when the shell cannot answer.
#[cfg(target_os = "windows")]
pub(crate) fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT},
    };

    // SAFETY: standard SHGetKnownFolderPath contract; the returned buffer is NUL-terminated and
    // freed with CoTaskMemFree after it is copied.
    unsafe {
        let mut ptr: *mut u16 = std::ptr::null_mut();
        if SHGetKnownFolderPath(id, KF_FLAG_DEFAULT as u32, std::ptr::null_mut(), &mut ptr) != 0
            || ptr.is_null()
        {
            return None;
        }
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let path = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
        CoTaskMemFree(ptr as _);
        (!path.trim().is_empty()).then(|| PathBuf::from(path))
    }
}
