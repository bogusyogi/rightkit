//! Facts about a running program: the path of a process image and the name the program gives
//! itself in its version resource.

use std::ffi::c_void;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

#[allow(non_snake_case)]
mod ffi {
    use std::ffi::c_void;

    #[link(name = "version")]
    unsafe extern "system" {
        pub fn GetFileVersionInfoSizeW(file: *const u16, handle: *mut u32) -> u32;
        pub fn GetFileVersionInfoW(
            file: *const u16,
            handle: u32,
            length: u32,
            data: *mut c_void,
        ) -> i32;
        pub fn VerQueryValueW(
            block: *const c_void,
            sub_block: *const u16,
            buffer: *mut *mut c_void,
            length: *mut u32,
        ) -> i32;
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The full path of the program a process runs, or `None` when it cannot be opened (an elevated
/// process, from a caller that is not).
pub fn image_path(pid: u32) -> Option<String> {
    // SAFETY: a plain OpenProcess for limited query rights; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY: `buffer` holds `size` UTF-16 units for the call.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    // SAFETY: the handle came from OpenProcess above and is not used again.
    let _ = unsafe { CloseHandle(process) };
    queried.ok()?;
    Some(String::from_utf16_lossy(&buffer[..size as usize]))
}

const VERSION_MAX: u32 = 256 * 1024;

/// What the program at `path` calls itself (its version resource's file description, such as
/// "Google Chrome"); `None` when it has none.
pub fn product_name(path: &str) -> Option<String> {
    let file = wide(path);
    let mut ignored = 0u32;
    // SAFETY: `file` is NUL-terminated; `ignored` is a valid out pointer.
    let size = unsafe { ffi::GetFileVersionInfoSizeW(file.as_ptr(), &mut ignored) };
    if size == 0 || size > VERSION_MAX {
        return None;
    }
    let mut block = vec![0u8; size as usize];
    // SAFETY: `block` holds `size` bytes for the call.
    if unsafe { ffi::GetFileVersionInfoW(file.as_ptr(), 0, size, block.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let query = |sub_block: &str| -> Option<(*const u8, usize)> {
        let name = wide(sub_block);
        let mut found: *mut c_void = std::ptr::null_mut();
        let mut length = 0u32;
        // SAFETY: `block` is a version resource filled above; the out pointers are valid, and
        // what comes back points into `block`, which outlives every use below.
        let ok = unsafe {
            ffi::VerQueryValueW(
                block.as_ptr().cast(),
                name.as_ptr(),
                &mut found,
                &mut length,
            )
        };
        let found = found as *const u8;
        (ok != 0 && !found.is_null() && length > 0).then_some((found, length as usize))
    };
    // The first language the resource lists, then US English in Unicode as most programs have.
    let (pair, pair_bytes) = query("\\VarFileInfo\\Translation")?;
    let mut tables = vec!["040904b0".to_string()];
    if pair_bytes >= 4 {
        // SAFETY: the value is at least four bytes inside `block`: a language id and a code
        // page, two bytes each.
        let bytes = unsafe { std::slice::from_raw_parts(pair, 4) };
        let language = u16::from_le_bytes([bytes[0], bytes[1]]);
        let code_page = u16::from_le_bytes([bytes[2], bytes[3]]);
        tables.insert(0, format!("{language:04x}{code_page:04x}"));
    }
    for table in tables {
        let key = format!("\\StringFileInfo\\{table}\\FileDescription");
        let Some((text, units)) = query(&key) else {
            continue;
        };
        // SAFETY: `text` points at `units` UTF-16 units (two bytes each) inside `block`.
        let bytes = unsafe { std::slice::from_raw_parts(text, units * 2) };
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|unit| *unit != 0)
            .collect();
        let name = String::from_utf16_lossy(&units).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}
