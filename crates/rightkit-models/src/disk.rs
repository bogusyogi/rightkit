//! Free-space query with no extra crates: `df -k` on unix, kernel32 FFI on Windows.

use std::path::Path;

/// Free bytes at the deepest existing ancestor of `path`.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            return None;
        }
    }
    free_bytes_at(&p)
}

#[cfg(unix)]
fn free_bytes_at(path: &Path) -> Option<u64> {
    let out = std::process::Command::new("df")
        .args(["-k"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // `df -k` wraps long device names onto a second line; the numbers are the
    // last line's fields: ... Used Available Capacity ...
    let line = text.lines().skip(1).collect::<Vec<_>>().join(" ");
    let fields: Vec<&str> = line.split_whitespace().collect();
    // Available is the first numeric field after Used: find "<n> <n> <n> <pct>%".
    let pct = fields.iter().position(|f| f.ends_with('%'))?;
    let avail_kb: u64 = fields.get(pct.checked_sub(1)?)?.parse().ok()?;
    Some(avail_kb * 1024)
}

#[cfg(windows)]
fn free_bytes_at(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            free_avail: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: valid NUL-terminated wide string and three valid out pointers.
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) } == 0 {
        return None;
    }
    Some(avail)
}

#[cfg(not(any(unix, windows)))]
fn free_bytes_at(_path: &Path) -> Option<u64> {
    None
}
