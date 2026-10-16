//! Process-free platform identity used by diagnostics & telemetry.

#[derive(Clone, Debug)]
pub struct PlatformIdentity {
    pub hw_model: String,
    pub chip: String,
    pub os_version: String,
    pub os_build: String,
    pub memory_total_mb: u64,
    pub webview_version: String,
}

pub fn current() -> PlatformIdentity {
    platform::current()
}

#[cfg(target_os = "windows")]
pub fn graphics_driver_version(adapter_name: &str) -> Option<String> {
    platform::graphics_driver_version(adapter_name)
}

#[cfg(target_os = "windows")]
mod platform {
    use super::PlatformIdentity;
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::null_mut;
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    const DISPLAY_CLASS: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";

    pub(super) fn current() -> PlatformIdentity {
        let product = registry_string(HKEY_LOCAL_MACHINE, CURRENT_VERSION, "ProductName")
            .unwrap_or_else(|| "Windows".into());
        let display = registry_string(HKEY_LOCAL_MACHINE, CURRENT_VERSION, "DisplayVersion")
            .or_else(|| registry_string(HKEY_LOCAL_MACHINE, CURRENT_VERSION, "ReleaseId"));
        let build = registry_string(HKEY_LOCAL_MACHINE, CURRENT_VERSION, "CurrentBuildNumber")
            .unwrap_or_else(|| "unknown".into());
        PlatformIdentity {
            hw_model: registry_string(
                HKEY_LOCAL_MACHINE,
                r"HARDWARE\DESCRIPTION\System\BIOS",
                "SystemProductName",
            )
            .unwrap_or_else(|| "unknown".into()),
            chip: registry_string(
                HKEY_LOCAL_MACHINE,
                r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
                "ProcessorNameString",
            )
            .unwrap_or_else(|| "unknown".into()),
            os_version: display
                .map(|display| format!("{product} {display}"))
                .unwrap_or(product),
            os_build: build,
            memory_total_mb: total_memory_bytes() / 1_048_576,
            webview_version: webview_version().unwrap_or_else(|| "unavailable".into()),
        }
    }

    pub(super) fn graphics_driver_version(adapter_name: &str) -> Option<String> {
        let expected = adapter_name.trim().to_ascii_lowercase();
        if expected.is_empty() {
            return None;
        }
        for index in 0..32 {
            let key = format!(r"{DISPLAY_CLASS}\{index:04}");
            let Some(description) = registry_string(HKEY_LOCAL_MACHINE, &key, "DriverDesc") else {
                continue;
            };
            let description = description.trim().to_ascii_lowercase();
            if expected == description
                || expected.contains(&description)
                || description.contains(&expected)
            {
                return registry_string(HKEY_LOCAL_MACHINE, &key, "DriverVersion");
            }
        }
        None
    }

    fn webview_version() -> Option<String> {
        const CLIENT: &str =
            r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
        registry_string(HKEY_CURRENT_USER, CLIENT, "pv")
            .or_else(|| registry_string(HKEY_LOCAL_MACHINE, CLIENT, "pv"))
            .or_else(|| {
                registry_string(
                    HKEY_LOCAL_MACHINE,
                    r"Software\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
                    "pv",
                )
            })
    }

    fn registry_string(root: HKEY, key: &str, value: &str) -> Option<String> {
        let key = wide(key);
        let value = wide(value);
        let mut bytes = 0_u32;
        // SAFETY: first call asks Windows for required byte count only.
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                null_mut(),
                &mut bytes,
            )
        };
        if status != 0 || bytes < 2 {
            return None;
        }
        let mut buffer = vec![0_u16; bytes as usize / 2];
        // SAFETY: buffer size is exactly the byte count returned above.
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast::<c_void>(),
                &mut bytes,
            )
        };
        if status != 0 {
            return None;
        }
        let length = buffer
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(buffer.len());
        let value = String::from_utf16_lossy(&buffer[..length])
            .trim()
            .to_string();
        (!value.is_empty()).then_some(value)
    }

    fn total_memory_bytes() -> u64 {
        // SAFETY: Windows fills an initialized structure whose length is supplied.
        unsafe {
            let mut memory: MEMORYSTATUSEX = zeroed();
            memory.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut memory) != 0 {
                memory.ullTotalPhys
            } else {
                0
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::PlatformIdentity;
    use std::ffi::CString;
    use std::ptr::null_mut;

    pub(super) fn current() -> PlatformIdentity {
        PlatformIdentity {
            hw_model: sysctl_string("hw.model").unwrap_or_else(|| "unknown".into()),
            chip: sysctl_string("machdep.cpu.brand_string")
                .or_else(|| sysctl_string("hw.model"))
                .unwrap_or_else(|| "unknown".into()),
            os_version: sysctl_string("kern.osproductversion").unwrap_or_else(|| "unknown".into()),
            os_build: sysctl_string("kern.osversion").unwrap_or_else(|| "unknown".into()),
            memory_total_mb: sysctl_u64("hw.memsize").unwrap_or(0) / 1_048_576,
            webview_version: "not_applicable".into(),
        }
    }

    fn sysctl_string(name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        let mut size = 0_usize;
        // SAFETY: first call queries required byte count only.
        if unsafe { libc::sysctlbyname(name.as_ptr(), null_mut(), &mut size, null_mut(), 0) } != 0
            || size == 0
        {
            return None;
        }
        let mut bytes = vec![0_u8; size];
        // SAFETY: buffer matches size returned by the first call.
        if unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut size,
                null_mut(),
                0,
            )
        } != 0
        {
            return None;
        }
        bytes.truncate(size);
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        let value = String::from_utf8(bytes).ok()?.trim().to_string();
        (!value.is_empty()).then_some(value)
    }

    fn sysctl_u64(name: &str) -> Option<u64> {
        let name = CString::new(name).ok()?;
        let mut value = 0_u64;
        let mut size = std::mem::size_of::<u64>();
        // SAFETY: output buffer is a correctly sized u64.
        let status = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut u64).cast(),
                &mut size,
                null_mut(),
                0,
            )
        };
        (status == 0 && size == std::mem::size_of::<u64>()).then_some(value)
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use super::PlatformIdentity;

    /// Every field is "unknown" (or 0) except where a portable source exists. On Android the
    /// device model, OS version and build live in system properties (bionic's
    /// `__system_property_get`) or in Kotlin's `Build` fields; neither is wired here, so they
    /// stay "unknown" and an app that wants them adds them to its own report.
    pub(super) fn current() -> PlatformIdentity {
        PlatformIdentity {
            hw_model: "unknown".into(),
            chip: "unknown".into(),
            os_version: "unknown".into(),
            os_build: "unknown".into(),
            memory_total_mb: memory_total_mb(),
            webview_version: "not_applicable".into(),
        }
    }

    /// Android: `MemTotal` of `/proc/meminfo`, which an app may read. Elsewhere on this
    /// fallback path (Linux and other unix targets) the value stays 0, as before.
    #[cfg(target_os = "android")]
    fn memory_total_mb() -> u64 {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| super::parse_meminfo_total_mb(&text))
            .unwrap_or(0)
    }

    #[cfg(not(target_os = "android"))]
    fn memory_total_mb() -> u64 {
        0
    }
}

/// `MemTotal` of a `/proc/meminfo` text, in whole MiB. `None` when the line is missing, has no
/// number, or does not say `kB`.
#[cfg_attr(not(any(test, target_os = "android")), allow(dead_code))]
fn parse_meminfo_total_mb(text: &str) -> Option<u64> {
    let rest = text
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?;
    let mut parts = rest.split_whitespace();
    let kib: u64 = parts.next()?.parse().ok()?;
    if !parts.next()?.eq_ignore_ascii_case("kb") {
        return None;
    }
    Some(kib / 1024)
}

#[cfg(test)]
mod tests {
    use super::parse_meminfo_total_mb;

    #[test]
    fn meminfo_total_is_read_in_mib() {
        let text = "MemTotal:        7889408 kB\nMemFree:          123456 kB\n";
        assert_eq!(parse_meminfo_total_mb(text), Some(7704));
    }

    #[test]
    fn meminfo_without_a_usable_total_is_none() {
        assert_eq!(parse_meminfo_total_mb(""), None);
        assert_eq!(parse_meminfo_total_mb("MemFree: 12 kB\n"), None);
        assert_eq!(parse_meminfo_total_mb("MemTotal: lots kB\n"), None);
        assert_eq!(parse_meminfo_total_mb("MemTotal: 2048\n"), None);
        assert_eq!(parse_meminfo_total_mb("MemTotal: 2048 MB\n"), None);
    }
}
