//! The shell's icon for a file (an installer, the app itself) as a premultiplied image the
//! card renderer can draw. Live cards only: the view renders never ask for one. Icons are
//! cached by path so a card that redraws does not ask the shell again.
//!
//! The icon is taken from the shell's image list by its index, which leaves out the arrow
//! the shell lays over a shortcut's icon: the launcher lists Start menu shortcuts as apps.

use crate::surface::Surface;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::{Arc, Mutex, PoisonError};
use windows::Win32::Graphics::Gdi::GdiFlush;
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_SYSICONINDEX, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{DI_NORMAL, DestroyIcon, DrawIconEx, HICON};
use windows::core::PCWSTR;

#[link(name = "comctl32")]
unsafe extern "system" {
    /// A copy of one image of an image list as an icon the caller destroys; null on failure.
    fn ImageList_GetIcon(list: *mut c_void, index: i32, flags: u32) -> *mut c_void;
}

/// Side of the large shell icon, in pixels.
const SIDE: usize = 32;
/// Icons kept; the cache is dropped when it grows past this.
const KEPT: usize = 24;

pub struct Icon {
    pub width: usize,
    pub height: usize,
    /// Premultiplied `0xAARRGGBB`, row-major.
    pub pixels: Vec<u32>,
}

type Cache = HashMap<String, Option<Arc<Icon>>>;

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// The icon of the file at `path`, or `None` when the shell has none.
pub fn of(path: &str) -> Option<Arc<Icon>> {
    if path.is_empty() {
        return None;
    }
    {
        let guard = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = guard.as_ref().and_then(|cache| cache.get(path)) {
            return found.clone();
        }
    }
    // The shell is asked with the lock released: a slow first call must not hold up another
    // thread that wants a cached icon (the launcher's panel thread).
    let icon = load(path).map(Arc::new);
    let mut guard = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let cache = guard.get_or_insert_with(HashMap::new);
    if cache.len() >= KEPT {
        cache.clear();
    }
    cache.insert(path.to_string(), icon.clone());
    icon
}

/// The file's icon as the shell's image list holds it, with nothing laid over it. The
/// caller destroys it.
fn plain_icon(wide: &[u16]) -> Option<HICON> {
    let mut info = SHFILEINFOW::default();
    // SAFETY: `wide` is NUL-terminated and `info` is a zeroed SHFILEINFOW; both outlive the
    // call. With this flag the result is the shell's own image list, which is not freed.
    let list = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        )
    };
    if list == 0 {
        return None;
    }
    // SAFETY: `list` is the image list the shell just returned and the index is its own.
    let icon = unsafe { ImageList_GetIcon(list as *mut c_void, info.iIcon, 0) };
    (!icon.is_null()).then_some(HICON(icon))
}

/// The file's icon as the shell hands it out whole, a shortcut's arrow included. The caller
/// destroys it.
fn shell_icon(wide: &[u16]) -> Option<HICON> {
    let mut info = SHFILEINFOW::default();
    // SAFETY: `wide` is NUL-terminated and `info` is a zeroed SHFILEINFOW; both outlive the
    // call.
    let found = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON,
        )
    };
    (found != 0 && !info.hIcon.is_invalid()).then_some(info.hIcon)
}

fn load(path: &str) -> Option<Icon> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut surface = Surface::new(SIDE, SIDE)?;
    let icon = plain_icon(&wide).or_else(|| shell_icon(&wide))?;
    // SAFETY: the surface's DC is live for the call; the icon handle is valid until the
    // DestroyIcon that follows.
    let drawn = unsafe {
        let drawn = DrawIconEx(
            surface.dc(),
            0,
            0,
            icon,
            SIDE as i32,
            SIDE as i32,
            0,
            None,
            DI_NORMAL,
        );
        let _ = GdiFlush();
        let _ = DestroyIcon(icon);
        drawn
    };
    drawn.ok()?;
    let mut pixels = surface.pixels_mut().to_vec();
    // An icon without an alpha channel leaves every alpha at zero: what it drew is opaque.
    if pixels.iter().all(|p| p >> 24 == 0) {
        for pixel in &mut pixels {
            if *pixel & 0x00FF_FFFF != 0 {
                *pixel |= 0xFF00_0000;
            }
        }
    }
    Some(Icon {
        width: SIDE,
        height: SIDE,
        pixels,
    })
}
