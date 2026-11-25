//! What a host can say about a peer it rejected, without keeping anything the peer sent (Pulse
//! ES-9, T6).
//!
//! The daemon gives a host only the reason text of a rejected peer. A different-user peer
//! (`PeerError::Mismatch`) carries its pid in that text ("(pid N)"); a peer the OS could not open
//! or query ("Access is denied" for an elevated Windows client) is built from an `io::Error` that
//! drops the pid, so for it the pid stays unknown ([`rejected_peer`] gives `None`). Only the image
//! file name is kept, never the folder path.

#[cfg(any(windows, target_os = "macos"))]
use std::path::Path;

/// The pid in a rejection reason's "(pid N)" and the image file name of that process.
pub fn rejected_peer(reason: &str) -> (Option<u32>, Option<String>) {
    let pid = reason
        .split("(pid ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|n| n.trim().parse::<u32>().ok());
    (pid, pid.and_then(image_name))
}

/// The executable file name of `pid`: Windows `QueryFullProcessImageNameW` through a
/// `PROCESS_QUERY_LIMITED_INFORMATION` handle (works for most elevated processes too), macOS
/// `proc_pidpath`. `None` when the process is gone or cannot be queried, and on other hosts.
#[cfg(windows)]
pub fn image_name(pid: u32) -> Option<String> {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn QueryFullProcessImageNameW(
            process: *mut c_void,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    // SAFETY: the handle is checked for null, used only for the query below and closed; the
    // buffer is 1024 writable u16 and its length is passed in and read back.
    let path = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        String::from_utf16_lossy(&buffer[..(len as usize).min(buffer.len())])
    };
    Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
pub fn image_name(pid: u32) -> Option<String> {
    extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut std::ffi::c_void, buffersize: u32) -> i32;
    }
    let pid = i32::try_from(pid).ok()?;
    let mut buffer = [0u8; 4096];
    // SAFETY: the buffer is 4096 writable bytes and its size is passed.
    let n = unsafe { proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if n <= 0 {
        return None;
    }
    let path = String::from_utf8_lossy(&buffer[..n as usize]).into_owned();
    Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn image_name(_pid: u32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_is_read_from_the_reason_text() {
        assert_eq!(rejected_peer("different user (pid 4242)").0, Some(4242));
        assert_eq!(rejected_peer("Access is denied"), (None, None));
    }
}
