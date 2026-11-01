//! Encryption at rest for the clipboard history (docs/clipboard-spec.md section 9).
//!
//! Every file the store writes (`index.json`, `boards.json`, `appcolours.json`, `apppaths.json`,
//! `items/*`, `thumbs/*`) is protected with DPAPI (`CryptProtectData`, user scope). Windows keeps
//! the per-user key, so the application holds no key of its own and no separate key store is needed.
//! Sealed layout: `PULSECL2` (8 bytes), then the DPAPI blob. A file without the header is a plain
//! file from before encryption: it is read as it is, and [`prepare`] seals it in place (the atomic
//! replace removes the plaintext). The switch `clipboardEncryption` only decides how new writes
//! are stored; reads take both forms.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use windows::Win32::Foundation::{HLOCAL, LocalFree};
#[cfg(windows)]
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
};
#[cfg(windows)]
use windows::core::PCWSTR;

const MAGIC: &[u8; 8] = b"PULSECL2";
#[cfg(windows)]
/// `CRYPTPROTECT_UI_FORBIDDEN`: DPAPI never shows a prompt.
const UI_FORBIDDEN: u32 = 0x1;

/// What [`prepare`] found. `Unavailable` is logged and never crashes the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prepared {
    /// The history is readable and new writes follow the switch.
    Ready,
    /// DPAPI refused to protect data: nothing is written this session.
    Unavailable,
}

/// The switch `clipboardEncryption` (default on). Off where there is no DPAPI.
pub fn enabled() -> bool {
    cfg!(windows) && crate::host::switch_on("clipboardEncryption")
}

#[cfg(windows)]
/// Copies the DPAPI output into memory and frees the OS buffer.
fn copy_and_free(blob: &CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if blob.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: DPAPI returned `cbData` bytes at `pbData`, allocated with LocalAlloc; the bytes are
    // copied before the buffer is freed, once.
    let bytes = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
    // SAFETY: the buffer came from CryptProtectData/CryptUnprotectData and is freed exactly once.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(blob.pbData as *mut c_void)));
    }
    bytes
}

#[cfg(windows)]
fn protect(plain: &[u8]) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(plain.len()).ok()?,
        pbData: plain.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` points at `plain`, which lives through the call; `output` is written by
    // the call and released by `copy_and_free`. No description, entropy or prompt is passed.
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            UI_FORBIDDEN,
            &mut output,
        )
    }
    .ok()?;
    Some(copy_and_free(&output))
}

#[cfg(windows)]
fn unprotect(sealed: &[u8]) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(sealed.len()).ok()?,
        pbData: sealed.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`; DPAPI only reads `input` for the call.
    unsafe { CryptUnprotectData(&input, None, None, None, None, UI_FORBIDDEN, &mut output) }
        .ok()?;
    Some(copy_and_free(&output))
}

#[cfg(not(windows))]
fn protect(_plain: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(not(windows))]
fn unprotect(_sealed: &[u8]) -> Option<Vec<u8>> {
    None
}

fn has_magic(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 8];
    file.read_exact(&mut head).is_ok() && &head == MAGIC
}

/// Every file that holds history or its caches, plain or sealed.
fn history_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        dir.join("index.json"),
        dir.join("boards.json"),
        dir.join("appcolours.json"),
        dir.join("apppaths.json"),
    ];
    for folder in ["items", "thumbs"] {
        if let Ok(entries) = fs::read_dir(dir.join(folder)) {
            paths.extend(entries.flatten().map(|entry| entry.path()));
        }
    }
    paths
}

/// True when every sealed history file (index, boards, payloads) opens. An absent or plain set
/// opens trivially.
fn history_opens(dir: &Path) -> bool {
    let mut paths = vec![dir.join("index.json"), dir.join("boards.json")];
    if let Ok(entries) = fs::read_dir(dir.join("items")) {
        paths.extend(entries.flatten().map(|entry| entry.path()));
    }
    paths
        .iter()
        .filter(|path| has_magic(path))
        .all(|path| read(path).is_some())
}

/// Seals every plain file in the history folder, in place.
fn migrate_plain(dir: &Path) {
    for path in history_files(dir) {
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        if bytes.starts_with(MAGIC) {
            continue;
        }
        if let Some(sealed) = seal(&bytes) {
            let _ = rightkit_fs::atomic_write(&path, &sealed);
        }
    }
}

/// Runs once when the store opens, before anything is read. `owner` is false when another process
/// holds the folder: then nothing is migrated, cleared or written.
///
/// - Sealed files that open: ready; plain leftovers are sealed (when the switch is on).
/// - Sealed files that do not open: `Unavailable`; the files are kept and not used this session.
/// - DPAPI refusing to protect: `Unavailable`, and the plain files stay as they are.
pub fn prepare(dir: &Path, owner: bool) -> Prepared {
    if !owner {
        return Prepared::Ready;
    }
    let sealed = history_files(dir).iter().any(|path| has_magic(path));
    if sealed && !history_opens(dir) {
        // Never deleted here: one damaged file or a DPAPI that is not ready yet (a profile still
        // loading) must not cost the whole history. It is left on disk and not used this session.
        crate::host::log_info("clipboard", &[("result", "history_unopened_kept")]);
        return Prepared::Unavailable;
    }
    if enabled() {
        if protect(&[0u8]).is_none() {
            crate::host::log_info("clipboard", &[("result", "history_protect_unavailable")]);
            return Prepared::Unavailable;
        }
        migrate_plain(dir);
    }
    Prepared::Ready
}

/// The sealed form of `plain`, or `None` when DPAPI refuses.
pub fn seal(plain: &[u8]) -> Option<Vec<u8>> {
    let blob = protect(plain)?;
    let mut out = Vec::with_capacity(MAGIC.len() + blob.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&blob);
    Some(out)
}

/// The plain form of a sealed file. `None` when it is not sealed or DPAPI cannot open it.
pub fn open(sealed: &[u8]) -> Option<Vec<u8>> {
    let blob = sealed.strip_prefix(&MAGIC[..])?;
    unprotect(blob)
}

/// The bytes of a store file: opened when sealed, as they are when plain. `None` when the file is
/// missing, sealed and not openable.
pub fn read(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    if bytes.starts_with(MAGIC) {
        open(&bytes)
    } else {
        Some(bytes)
    }
}

/// Writes a store file atomically, sealed when the switch is on. `false` when it could not be
/// written (including when the switch is on and DPAPI refuses).
pub fn write(path: &Path, bytes: &[u8]) -> bool {
    let out = if enabled() {
        match seal(bytes) {
            Some(sealed) => sealed,
            None => return false,
        }
    } else {
        bytes.to_vec()
    };
    rightkit_fs::atomic_write(path, &out).is_ok()
}

/// Runs `f` on a plain copy of `bytes` in the store's `.tmp` folder, for the decoders that take a
/// path. The copy is removed afterwards.
pub fn with_plain_file<R>(bytes: &[u8], extension: &str, f: impl FnOnce(&Path) -> R) -> Option<R> {
    let path = crate::model::tmp_file(extension)?;
    if fs::write(&path, bytes).is_err() {
        let _ = fs::remove_file(&path);
        return None;
    }
    let out = f(&path);
    let _ = fs::remove_file(&path);
    Some(out)
}
