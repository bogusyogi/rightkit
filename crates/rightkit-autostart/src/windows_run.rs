//! Launch at login through one value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`,
//! written with direct registry calls (no `reg.exe` process, no extra crate features), and the
//! reads that go with it: whether Task Manager's Startup tab disabled the value, and whether
//! Windows toasts are off for the account or for the app.
//!
//! Per-user, no elevation. The value is only rewritten when it differs. `apply_run_value` makes
//! the entry match a switch for the installed copy of an app: only the executable that sits in
//! `%LOCALAPPDATA%\Programs\<install folder>` registers itself, so a build run from another
//! folder never repoints the entry. The registry symbols are declared here against
//! `advapi32.dll`.
//!
//! This is the logic Pulse's Windows notch had (`windows/src/autostart.rs`); the host keeps its
//! own value name, install folder name, logging and the words for each outcome.

use std::path::Path;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
/// `HKEY_CURRENT_USER` (0x80000001 sign-extended to pointer width).
const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
const KEY_QUERY_VALUE: u32 = 0x0001;
const KEY_SET_VALUE: u32 = 0x0002;
const KEY_READ: u32 = 0x0002_0019;
const REG_SZ: u32 = 1;
const REG_DWORD: u32 = 4;
const REG_BINARY: u32 = 3;
const PUSH_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\PushNotifications";
const APP_NOTIFICATIONS: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Notifications\\Settings";
const ERROR_SUCCESS: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;
/// Where Task Manager's Startup tab keeps its on/off choice for each Run value.
const APPROVED_KEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";

#[allow(non_snake_case, clippy::too_many_arguments)]
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: isize,
        sub_key: *const u16,
        options: u32,
        desired: u32,
        result: *mut isize,
    ) -> i32;
    fn RegSetValueExW(
        key: isize,
        name: *const u16,
        reserved: u32,
        kind: u32,
        data: *const u8,
        len: u32,
    ) -> i32;
    fn RegQueryValueExW(
        key: isize,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        len: *mut u32,
    ) -> i32;
    fn RegDeleteValueW(key: isize, name: *const u16) -> i32;
    fn RegEnumKeyExW(
        key: isize,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *mut u32,
        class: *mut u16,
        class_len: *mut u32,
        last_write: *mut u64,
    ) -> i32;
    fn RegCloseKey(key: isize) -> i32;
}

struct Key(isize);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful RegOpenKeyExW and is closed once.
        unsafe { RegCloseKey(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_run_key() -> Result<Key, i32> {
    let sub_key = wide(RUN_KEY);
    let mut handle = 0isize;
    // SAFETY: NUL-terminated key path and a valid out pointer, both alive for the call.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            sub_key.as_ptr(),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut handle,
        )
    };
    if status == ERROR_SUCCESS {
        Ok(Key(handle))
    } else {
        Err(status)
    }
}

fn current_value(key: &Key, value_name: &str) -> Option<String> {
    let name = wide(value_name);
    let mut kind = 0u32;
    let mut len = 0u32;
    // SAFETY: size probe (null data) then a read into a buffer of the reported size.
    unsafe {
        if RegQueryValueExW(
            key.0,
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            std::ptr::null_mut(),
            &mut len,
        ) != ERROR_SUCCESS
            || kind != REG_SZ
            || len == 0
            || len > 32 * 1024
        {
            return None;
        }
        let mut buffer = vec![0u16; (len as usize).div_ceil(2)];
        let mut capacity = (buffer.len() * 2) as u32;
        if RegQueryValueExW(
            key.0,
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            buffer.as_mut_ptr().cast(),
            &mut capacity,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let units = (capacity as usize / 2).min(buffer.len());
        let text = &buffer[..units];
        let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
        Some(String::from_utf16_lossy(&text[..end]))
    }
}

/// Whether `exe` sits directly in `%LOCALAPPDATA%\Programs\<install_dir_name>` (the installer's
/// folder), compared without regard to case.
fn in_install_folder(exe: &Path, install_dir_name: &str) -> bool {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return false;
    };
    let install = std::path::PathBuf::from(local)
        .join("Programs")
        .join(install_dir_name);
    let folder = |path: &Path| path.to_string_lossy().to_ascii_lowercase();
    exe.parent()
        .is_some_and(|parent| folder(parent) == folder(install.as_path()))
}

/// What `apply_run_value` did. Status codes are Win32 registry error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The Run key could not be opened.
    Unavailable(i32),
    /// The value is not there now (it was removed, or was never there).
    Removed,
    /// Removing the value failed.
    RemoveFailed(i32),
    /// This program's own path could not be read.
    NoExecutable,
    /// This program is not the installed copy, so nothing was written.
    NotInstalled,
    /// The value already named this program; nothing was written.
    Unchanged,
    /// The value was written.
    Enabled,
    /// Writing the value failed.
    SetFailed(i32),
}

/// Makes the Run value `value_name` match `enabled`: the quoted path of this executable, written
/// only when this executable is the installed copy (it sits in
/// `%LOCALAPPDATA%\Programs\<install_dir_name>`) and only when the value differs. With `enabled`
/// false the value is deleted (an absent value counts as removed). Never fatal: the caller logs the
/// outcome.
pub fn apply_run_value(value_name: &str, install_dir_name: &str, enabled: bool) -> RunOutcome {
    let key = match open_run_key() {
        Ok(key) => key,
        Err(code) => return RunOutcome::Unavailable(code),
    };
    let name = wide(value_name);
    if !enabled {
        // SAFETY: valid key handle and NUL-terminated value name.
        let status = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
            return RunOutcome::RemoveFailed(status);
        }
        return RunOutcome::Removed;
    }
    let Ok(exe) = std::env::current_exe() else {
        return RunOutcome::NoExecutable;
    };
    // Only the installed copy registers itself (the Mac's `/Applications` rule): a build run
    // from another folder never repoints the entry, and the entry is written once.
    if !in_install_folder(&exe, install_dir_name) {
        return RunOutcome::NotInstalled;
    }
    let desired = format!("\"{}\"", exe.display());
    if current_value(&key, value_name).as_deref() == Some(desired.as_str()) {
        return RunOutcome::Unchanged;
    }
    let data = wide(&desired);
    // SAFETY: `data` is a NUL-terminated UTF-16 buffer; the length passed is its byte size.
    let status = unsafe {
        RegSetValueExW(
            key.0,
            name.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    if status == ERROR_SUCCESS {
        RunOutcome::Enabled
    } else {
        RunOutcome::SetFailed(status)
    }
}

// ------------------------------------------------------------------ system permission rows

fn open_read(path: &str) -> Option<Key> {
    let sub_key = wide(path);
    let mut handle = 0isize;
    // SAFETY: NUL-terminated key path and a valid out pointer, both alive for the call.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            sub_key.as_ptr(),
            0,
            KEY_READ,
            &mut handle,
        )
    };
    (status == ERROR_SUCCESS).then_some(Key(handle))
}

fn dword(key: &Key, name: &str) -> Option<u32> {
    let name = wide(name);
    let mut kind = 0u32;
    let mut value = 0u32;
    let mut len = size_of::<u32>() as u32;
    // SAFETY: the out buffer is a live u32 of the declared size.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            (&mut value as *mut u32).cast(),
            &mut len,
        )
    };
    (status == ERROR_SUCCESS && kind == REG_DWORD).then_some(value)
}

fn subkey_names(key: &Key) -> Vec<String> {
    let mut names = Vec::new();
    for index in 0..512u32 {
        let mut buffer = [0u16; 256];
        let mut len = buffer.len() as u32;
        // SAFETY: `buffer` is valid for `len` UTF-16 units; the other out pointers are null.
        let status = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                buffer.as_mut_ptr(),
                &mut len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        // ERROR_NO_MORE_ITEMS ends the list; any other error ends the read the same way.
        if status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(
            &buffer[..(len as usize).min(buffer.len())],
        ));
    }
    names
}

/// Whether toasts are off for the whole account (`PushNotifications\ToastEnabled` is 0) or for an
/// app entry whose key name contains `app_name_fragment` (lower case; `Settings\<name>\Enabled`
/// is 0). Absent keys and values count as on.
pub fn notifications_blocked(app_name_fragment: &str) -> bool {
    let account_off = open_read(PUSH_KEY).and_then(|key| dword(&key, "ToastEnabled")) == Some(0);
    let app_off = open_read(APP_NOTIFICATIONS).is_some_and(|settings| {
        subkey_names(&settings).iter().any(|name| {
            name.to_lowercase().contains(app_name_fragment)
                && open_read(&format!("{APP_NOTIFICATIONS}\\{name}"))
                    .and_then(|key| dword(&key, "Enabled"))
                    == Some(0)
        })
    });
    account_off || app_off
}

/// Task Manager's Startup tab disabled the Run value `value_name`: the first byte of its approval
/// binary is odd (0x03 or 0x07) when disabled, even (0x02 or 0x06) when enabled. (This is the
/// Startup tab's own encoding; the crate's `autostart::startup_approved_is_disabled` follows
/// `auto-launch`'s last-eight-bytes rule for the values `tauri-plugin-autostart` wrote.)
pub fn disabled_in_task_manager(value_name: &str) -> bool {
    let Some(approved) = open_read(APPROVED_KEY) else {
        return false;
    };
    let name = wide(value_name);
    let mut data = [0u8; 12];
    let mut kind = 0u32;
    let mut len = data.len() as u32;
    // SAFETY: `data` is a live buffer of the declared length; the out pointers are valid.
    let status = unsafe {
        RegQueryValueExW(
            approved.0,
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            data.as_mut_ptr(),
            &mut len,
        )
    };
    status == ERROR_SUCCESS && kind == REG_BINARY && len >= 1 && data[0] & 1 == 1
}

/// Whether the Run value `value_name` is in effect now: present and not disabled in Task
/// Manager's Startup tab. `None` when the Run key cannot be read.
pub fn run_value_in_effect(value_name: &str) -> Option<bool> {
    let key = open_run_key().ok()?;
    Some(current_value(&key, value_name).is_some() && !disabled_in_task_manager(value_name))
}

/// How the Run value stands, as a settings row reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    /// There, and not disabled in Task Manager's Startup tab.
    InEffect,
    /// Not there, or there but disabled in Task Manager.
    Off,
    /// The Run key cannot be read.
    Unknown,
}

/// The status of the Run value `value_name`.
pub fn run_value_status(value_name: &str) -> RunStatus {
    match open_run_key() {
        Ok(key)
            if current_value(&key, value_name).is_some()
                && !disabled_in_task_manager(value_name) =>
        {
            RunStatus::InEffect
        }
        Ok(_) => RunStatus::Off,
        Err(_) => RunStatus::Unknown,
    }
}
