//! Windows alert sounds: the stock `.wav` files in `%SystemRoot%\Media`, played with `PlaySoundW`
//! (asynchronous, so nothing waits), falling back to the system "asterisk" beep when no file can
//! be played. The application owns which tone uses which file and what is logged; this module
//! lists the files, checks a name and plays one.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const SND_ASYNC: u32 = 0x0001;
const SND_NODEFAULT: u32 = 0x0002;
const SND_FILENAME: u32 = 0x0002_0000;
const MB_ICONASTERISK: u32 = 0x40;
/// The longest sound name accepted.
const MAX_NAME_LEN: usize = 64;

#[link(name = "winmm")]
unsafe extern "system" {
    fn PlaySoundW(name: *const u16, module: *mut c_void, flags: u32) -> i32;
}

// winuser's MessageBeep, declared here so the crate needs no extra `windows` feature.
#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBeep(utype: u32) -> i32;
}

/// A sound name that is safe to pass on to `PlaySoundW`: a stock file's name without ".wav", so
/// no path, no extension and no odd characters (ASCII letters and digits, space, `-`, `_`, `(`, `)`;
/// at most 64 bytes).
pub fn valid_sound_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '(' | ')'))
}

/// The folder of the stock sounds: `%SystemRoot%\Media` (`C:\Windows\Media` when the variable is
/// unset).
pub fn media_dir() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    Path::new(&root).join("Media")
}

/// The stock sounds on offer: the `.wav` files in [`media_dir`] whose names pass
/// [`valid_sound_name`], by name without the extension, in alphabetical order (case-insensitive).
pub fn media_sounds() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(media_dir())
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    let is_wav = path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"));
                    let stem = path.file_stem()?.to_str()?.to_string();
                    (is_wav && valid_sound_name(&stem)).then_some(stem)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|name| name.to_lowercase());
    names
}

/// Plays `<media_dir>\<file>` without waiting for it. False when the file is missing or Windows
/// did not start it. `file` is a file name with extension from the caller's own table.
pub fn play_media_file(file: &str) -> bool {
    let path = media_dir().join(file);
    if !path.is_file() {
        return false;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call; with SND_FILENAME the module
    // argument is unused.
    let started = unsafe {
        PlaySoundW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
        )
    };
    started != 0
}

/// The system "asterisk" beep. False when Windows could not play it.
pub fn beep() -> bool {
    // SAFETY: MessageBeep has no preconditions.
    unsafe { MessageBeep(MB_ICONASTERISK) != 0 }
}

/// Plays `chosen` from [`media_dir`]; when that file is missing, plays `fallback` instead; when
/// neither plays, beeps. Returns the file name that was tried last and whether anything sounded,
/// so the caller can log it.
pub fn play_with_fallback(chosen: &str, fallback: &str) -> (String, bool) {
    let file = if media_dir().join(chosen).is_file() {
        chosen
    } else {
        fallback
    };
    let started = play_media_file(file) || beep();
    (file.to_string(), started)
}

#[cfg(test)]
mod tests {
    use super::valid_sound_name;

    #[test]
    fn names_are_plain() {
        assert!(valid_sound_name("Windows Notify Calendar"));
        assert!(valid_sound_name("chimes"));
        assert!(!valid_sound_name(""));
        assert!(!valid_sound_name("..\\evil"));
        assert!(!valid_sound_name("a.wav"));
        assert!(!valid_sound_name(&"a".repeat(65)));
    }
}
