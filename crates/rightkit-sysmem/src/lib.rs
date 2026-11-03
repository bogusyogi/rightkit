//! Allocator pressure relief, physical-memory probe and monitor colour profile, behind a safe API.
//!
//! This is the one place the RightKit suite keeps its platform FFI for these three answers. Ember,
//! Pulse and `rightkit-fsindex` each carried their own copy; they now call this crate. Every
//! function is safe to call from any thread, returns nothing on failure or on platforms without the
//! call, and never panics.
//!
//! Background, kept from the donor (Ember `crates/sysmem`): macOS's allocator keeps freed large
//! blocks mapped as "reusable" (not in the footprint, still in resident size) until the system runs
//! short; [`release_free_memory`] returns those pages now (`malloc_zone_pressure_relief`). Elsewhere
//! blocks this large are unmapped when freed, so it does nothing. [`total_physical_memory`] reads the
//! machine's RAM on Windows, which has no other native-free source. [`window_monitor_profile`] finds
//! the ICC profile Windows assigns to the monitor showing the caller's main window.
//!
//! `unsafe` lives only in the FFI modules below (`macos`, `windows`, `windows_display`); each block
//! carries a `// SAFETY:` comment.
#![deny(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

/// Return free allocator pages to the system. Returns the number of bytes released (always 0 on
/// platforms where freed large blocks are already unmapped). Safe to call at any time, from any
/// thread.
pub fn release_free_memory() -> usize {
    #[cfg(target_os = "macos")]
    {
        macos::release()
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// The machine's physical memory in bytes, where only a native call can tell (Windows); `None`
/// elsewhere (other platforms read it without native calls) or when the call fails.
pub fn total_physical_memory() -> Option<u64> {
    #[cfg(windows)]
    {
        windows::total_physical()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The path of the ICC profile Windows has assigned to the monitor the calling thread's main window
/// is on (the primary monitor when it has none), through the same colour-management settings as
/// Settings ▸ Display ▸ Color management. Call it on the thread that owns the window. `None` off
/// Windows, without a monitor, or when the lookup fails; the caller then shows sRGB. The file may
/// be Windows' stock sRGB profile when none was assigned.
pub fn window_monitor_profile() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        windows_display::profile_path()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_display {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;

    type Hwnd = *mut c_void;
    type Hmonitor = *mut c_void;
    type Hdc = *mut c_void;

    /// `RECT` (windef.h).
    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    /// `MONITORINFOEXW` (winuser.h): `MONITORINFO` plus the device name.
    #[repr(C)]
    struct MonitorInfoExW {
        size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
        device: [u16; 32],
    }

    const MONITOR_DEFAULTTOPRIMARY: u32 = 1;
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    const GW_OWNER: u32 = 4;
    /// Capacity, in UTF-16 units, of the buffer the profile path is read into.
    const PATH_UNITS: usize = 1024;

    #[link(name = "user32")]
    unsafe extern "system" {
        /// user32: calls `func(window, lparam)` for each top-level window of thread `thread_id` until it returns 0.
        fn EnumThreadWindows(
            thread_id: u32,
            func: unsafe extern "system" fn(Hwnd, isize) -> i32,
            lparam: isize,
        ) -> i32;
        /// user32: nonzero when the window is visible.
        fn IsWindowVisible(window: Hwnd) -> i32;
        /// user32: the window related to `window` by `cmd` (null when there is none).
        fn GetWindow(window: Hwnd, cmd: u32) -> Hwnd;
        /// user32: the monitor `window` is mostly on (`flags` picks what a null window gets).
        fn MonitorFromWindow(window: Hwnd, flags: u32) -> Hmonitor;
        /// user32: fills `info` (whose `size` must be set); nonzero on success.
        fn GetMonitorInfoW(monitor: Hmonitor, info: *mut MonitorInfoExW) -> i32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        /// gdi32: a device context for a device, or null on failure.
        fn CreateDCW(
            driver: *const u16,
            device: *const u16,
            port: *const u16,
            mode: *const c_void,
        ) -> Hdc;
        /// gdi32: frees a context made by `CreateDCW`.
        fn DeleteDC(dc: Hdc) -> i32;
        /// gdi32: writes the colour profile path of the device behind `dc` into `name` (`*size` holds its
        /// capacity in UTF-16 units and receives the length); nonzero on success.
        fn GetICMProfileW(dc: Hdc, size: *mut u32, name: *mut u16) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// kernel32: the calling thread's id.
        fn GetCurrentThreadId() -> u32;
    }

    /// `EnumThreadWindows` callback: stores the first visible window that has no owner (a main
    /// window, not a dialog or tool window) in the `Hwnd` slot `lparam` points to, and stops.
    unsafe extern "system" fn pick_main_window(window: Hwnd, lparam: isize) -> i32 {
        // SAFETY: `lparam` is the address of the `Hwnd` slot `main_window` keeps alive and exclusively
        // owns for the whole synchronous enumeration, and no other code touches it meanwhile.
        // `window` is a handle `EnumThreadWindows` has just handed us; `IsWindowVisible` and
        // `GetWindow` only read window state and are safe to call with any handle value.
        unsafe {
            if IsWindowVisible(window) != 0 && GetWindow(window, GW_OWNER).is_null() {
                *(lparam as *mut Hwnd) = window;
                return 0;
            }
        }
        1
    }

    /// The calling thread's first visible top-level window, or null.
    fn main_window() -> Hwnd {
        let mut found: Hwnd = std::ptr::null_mut();
        // SAFETY: `EnumThreadWindows` is a documented user32 function that calls our callback
        // synchronously, on this thread, and keeps no pointer after returning. `found` outlives the
        // call and only the callback writes to it, through the address passed as `lparam`.
        unsafe {
            EnumThreadWindows(
                GetCurrentThreadId(),
                pick_main_window,
                std::ptr::addr_of_mut!(found) as isize,
            );
        }
        found
    }

    /// The text of a NUL-terminated UTF-16 buffer (all of it without a terminator).
    pub(super) fn wide_to_path(units: &[u16]) -> Option<std::path::PathBuf> {
        let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
        (end > 0).then(|| std::path::PathBuf::from(std::ffi::OsString::from_wide(&units[..end])))
    }

    pub fn profile_path() -> Option<std::path::PathBuf> {
        let window = main_window();
        let flags = if window.is_null() {
            MONITOR_DEFAULTTOPRIMARY
        } else {
            MONITOR_DEFAULTTONEAREST
        };
        // SAFETY: `MonitorFromWindow` accepts a null or a valid window handle and returns a monitor
        // handle (or null, which `GetMonitorInfoW` then rejects); it keeps nothing.
        let monitor = unsafe { MonitorFromWindow(window, flags) };
        if monitor.is_null() {
            return None;
        }
        let mut info = MonitorInfoExW {
            size: u32::try_from(std::mem::size_of::<MonitorInfoExW>()).ok()?,
            monitor: Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            work: Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            flags: 0,
            device: [0; 32],
        };
        // SAFETY: `GetMonitorInfoW` writes only into the `MONITORINFOEXW` we pass, a live,
        // exclusively borrowed, correctly laid out (`repr(C)`) struct whose `size` holds its size as
        // the API requires; it keeps no pointer. A zero return means failure.
        let ok = unsafe { GetMonitorInfoW(monitor, &mut info) };
        if ok == 0 || info.device[0] == 0 {
            return None;
        }
        let driver: Vec<u16> = "DISPLAY\0".encode_utf16().collect();
        info.device[31] = 0;
        // SAFETY: `driver` and `info.device` are NUL-terminated UTF-16 strings that outlive the
        // call (`device` is a fixed buffer, not empty, with its last unit forced to 0 above). The
        // port and mode arguments are optional (null).
        let dc = unsafe {
            CreateDCW(
                driver.as_ptr(),
                info.device.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if dc.is_null() {
            return None;
        }
        let mut buffer = [0u16; PATH_UNITS];
        let mut size = PATH_UNITS as u32;
        // SAFETY: `dc` is the live context we just created and delete exactly once right after.
        // `GetICMProfileW` writes at most `size` (= `buffer.len()`) UTF-16 units into `buffer`, a
        // live, exclusively borrowed array, and updates `size`; it keeps no pointer afterwards.
        let ok = unsafe {
            let ok = GetICMProfileW(dc, &mut size, buffer.as_mut_ptr());
            DeleteDC(dc);
            ok
        };
        if ok == 0 { None } else { wide_to_path(&buffer) }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    /// `MEMORYSTATUSEX` (sysinfoapi.h).
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// kernel32: fills `buffer` (whose `length` must be set to its size); nonzero on success.
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }

    pub fn total_physical() -> Option<u64> {
        let mut status = MemoryStatusEx {
            length: u32::try_from(std::mem::size_of::<MemoryStatusEx>()).ok()?,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        // SAFETY: `GlobalMemoryStatusEx` is a documented, thread-safe kernel32 function. It writes
        // only into the `MEMORYSTATUSEX` we pass, a live, exclusively borrowed, correctly laid out
        // (`repr(C)`) struct whose `length` holds its size as the API requires, and keeps no
        // pointer after returning. A zero return means failure; then we report nothing.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        (ok != 0 && status.total_phys > 0).then_some(status.total_phys)
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    unsafe extern "C" {
        /// libSystem malloc: release free memory of `zone` (all zones when null) back to the
        /// system, up to `goal` bytes (0 = as much as possible). Returns the bytes released.
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }

    pub fn release() -> usize {
        // SAFETY: `malloc_zone_pressure_relief` is a documented, thread-safe libSystem function
        // (malloc/malloc.h). A null zone means "every zone" and a goal of 0 means "as much as
        // possible". It only returns already-free pages to the system: it reads and writes no
        // memory we own, takes no pointers we must keep alive, and has no failure mode beyond
        // releasing 0 bytes.
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) }
    }
}
