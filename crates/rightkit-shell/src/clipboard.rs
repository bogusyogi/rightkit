//! Rich clipboard: text, HTML, PNG image, file lists and app-defined formats, in one transaction,
//! plus a typed "what did the person copy" contract for paste and open-from-clipboard.
//!
//! Donors: HeardRight `clipboard` + delivery (text/HTML/image, paste-sequence discipline),
//! ViewRight file grants (paths, not bytes) and ViewRight `commands/clipboard.rs` (typed
//! files/image/text/empty result, CF_DIB to BMP, temp-file grants, one-hour purge).
//!
//! * [`read`] / [`write`] are the raw transaction API. macOS uses `NSPasteboard` (one primary item
//!   carrying every representation, plus one item per file, which is how Finder lays files out);
//!   Windows uses the Win32 clipboard (`CF_UNICODETEXT`, `HTML Format`, `PNG`, `CF_HDROP`).
//!   `Board::Named` targets a private pasteboard so tests and apps can stage content without
//!   touching what the user copied (macOS only).
//! * [`take_item`] is the typed contract the `rk_shell_clipboard_item` command returns. Priority is
//!   files, then a bitmap (written to a granted temp file), then text, then nothing. Files and
//!   temp images are granted through the [`PathBroker`]; nothing else reaches the renderer.
//!   [`item_from_snapshot`] runs the same policy on any snapshot, so it is testable on every host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::broker::PathBroker;
use crate::error::ShellError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Board {
    General,
    Named(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardContent {
    pub text: Option<String>,
    pub html: Option<String>,
    pub png: Option<Vec<u8>>,
    /// Absolute paths.
    pub files: Vec<String>,
    /// App-defined formats, keyed by UTI / registered format name.
    pub custom: BTreeMap<String, Vec<u8>>,
}

/// Default age after which an earlier clipboard temp file is purged.
pub const DEFAULT_PURGE_AFTER: Duration = Duration::from_secs(60 * 60);

/// Typed result of reading the clipboard for an "open" or "paste" action. Serialises as
/// `{"kind": "files" | "image" | "text" | "none", ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ClipboardItem {
    /// Files copied in the file manager. Each path is granted to the broker.
    Files { paths: Vec<String> },
    /// A bitmap, written to a granted temp file (`.png` when the clipboard had PNG, else `.bmp`).
    Image { path: String },
    /// Plain Unicode text.
    Text { text: String },
    /// Nothing usable.
    None,
}

/// A bitmap taken from the clipboard, with the extension its temp file should carry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipboardImage {
    pub extension: String,
    pub bytes: Vec<u8>,
}

/// What the platform clipboard held, before the priority policy runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipboardSnapshot {
    pub files: Vec<PathBuf>,
    pub image: Option<ClipboardImage>,
    pub text: Option<String>,
}

/// Clipboard temp-file policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPolicy {
    /// Temp files older than this are removed before a new one is written.
    pub purge_after: Duration,
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            purge_after: DEFAULT_PURGE_AFTER,
        }
    }
}

/// The per-app temp directory for clipboard images: `<temp>/<app_id>-clipboard`.
pub fn clipboard_temp_dir(app_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{app_id}-clipboard"))
}

pub fn write(board: &Board, c: &ClipboardContent) -> Result<(), String> {
    imp::write(board, c)
}
pub fn read(board: &Board) -> Result<ClipboardContent, String> {
    imp::read(board)
}
/// Raw type/format names currently on the board (diagnostics).
pub fn formats(board: &Board) -> Result<Vec<String>, String> {
    imp::formats(board)
}

/// Read the clipboard and apply the priority policy: files, then a bitmap, then text.
pub fn take_item(
    broker: &PathBroker,
    temp_dir: &Path,
    policy: ClipboardPolicy,
) -> Result<ClipboardItem, ShellError> {
    let snapshot = platform_snapshot(temp_dir)?;
    item_from_snapshot(&snapshot, broker, temp_dir, policy)
}

/// Apply the priority policy to a snapshot: grant the files that exist, write a bitmap to a granted
/// temp file, otherwise return non-empty text. Old temp files are purged first.
pub fn item_from_snapshot(
    snapshot: &ClipboardSnapshot,
    broker: &PathBroker,
    temp_dir: &Path,
    policy: ClipboardPolicy,
) -> Result<ClipboardItem, ShellError> {
    if temp_dir.is_dir() {
        purge_stale(temp_dir, policy.purge_after);
    }
    let granted: Vec<String> = snapshot
        .files
        .iter()
        .filter_map(|p| broker.grant_file(p).ok())
        .collect();
    if !granted.is_empty() {
        return Ok(ClipboardItem::Files { paths: granted });
    }
    if let Some(image) = snapshot.image.as_ref().filter(|i| !i.bytes.is_empty()) {
        let path = write_temp(temp_dir, &image.extension, &image.bytes)?;
        return match broker.grant_file(&path) {
            Ok(display) => Ok(ClipboardItem::Image { path: display }),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                Err(e)
            }
        };
    }
    match snapshot.text.as_deref() {
        Some(text) if !text.is_empty() => Ok(ClipboardItem::Text {
            text: text.to_owned(),
        }),
        _ => Ok(ClipboardItem::None),
    }
}

/// Remove regular files in `dir` last modified more than `older_than` ago. Returns how many were
/// removed. A missing directory or an unreadable entry is skipped, never an error.
pub fn purge_stale(dir: &Path, older_than: Duration) -> usize {
    let Some(cutoff) = SystemTime::now().checked_sub(older_than) else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.filter_map(Result::ok) {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified < cutoff && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Write `bytes` to a fresh `clipboard-<stamp>.<ext>` file in `dir` (created if needed).
fn write_temp(dir: &Path, extension: &str, bytes: &[u8]) -> Result<PathBuf, ShellError> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let ext: String = extension
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    for _ in 0..8 {
        let path = dir.join(format!("clipboard-{}.{ext}", unique_stamp()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ShellError::Other(
        "could not create a clipboard temp file".into(),
    ))
}

fn unique_stamp() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Convert a `CF_DIB` blob (a BITMAPINFO plus pixels) to a complete BMP file by prepending the
/// 14-byte file header. Returns `None` for a blob too short or inconsistent to be a DIB.
pub fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let header_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]) as usize;
    if header_size < 40 || dib.len() < header_size {
        return None;
    }
    let bit_count = u16::from_le_bytes([dib[14], dib[15]]) as usize;
    let compression = u32::from_le_bytes([dib[16], dib[17], dib[18], dib[19]]);
    let clr_used = u32::from_le_bytes([dib[32], dib[33], dib[34], dib[35]]) as usize;
    // BI_BITFIELDS (3) and BI_ALPHABITFIELDS (6) store colour masks after the header.
    let mask_bytes = match compression {
        3 => 12,
        6 => 16,
        _ => 0,
    };
    let palette_colors = if bit_count <= 8 {
        if clr_used != 0 {
            clr_used
        } else {
            1usize << bit_count
        }
    } else {
        clr_used
    };
    let off_bits = 14 + header_size + mask_bytes + palette_colors * 4;
    if off_bits > 14 + dib.len() {
        return None;
    }
    let file_size = 14 + dib.len();
    let mut out = Vec::with_capacity(file_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(off_bits as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

#[cfg(target_os = "macos")]
fn platform_snapshot(temp_dir: &Path) -> Result<ClipboardSnapshot, ShellError> {
    let general = Board::General;
    let raw = read(&general)?;
    let types = formats(&general)?;
    let mut files: Vec<PathBuf> = raw.files.iter().map(PathBuf::from).collect();
    // The API saw a file-URL item but produced no path: ask AppKit through osascript.
    if files.is_empty() && types.iter().any(|t| t == "public.file-url") {
        files = macos_osascript::file_paths()
            .into_iter()
            .map(PathBuf::from)
            .collect();
    }
    let image = match raw.png {
        Some(png) => Some(ClipboardImage {
            extension: "png".into(),
            bytes: png,
        }),
        // Only a non-PNG bitmap (TIFF, or an image type) needs AppleScript's PNG coercion.
        None if types
            .iter()
            .any(|t| t == "public.tiff" || t == "public.image") =>
        {
            macos_osascript::png_bytes(temp_dir).map(|bytes| ClipboardImage {
                extension: "png".into(),
                bytes,
            })
        }
        None => None,
    };
    Ok(ClipboardSnapshot {
        files,
        image,
        text: raw.text,
    })
}

#[cfg(windows)]
fn platform_snapshot(_temp_dir: &Path) -> Result<ClipboardSnapshot, ShellError> {
    let raw = read(&Board::General)?;
    // PNG when the copier provided it (lossless, alpha); otherwise the classic CF_DIB as BMP.
    let image = match raw.png {
        Some(png) => Some(ClipboardImage {
            extension: "png".into(),
            bytes: png,
        }),
        None => imp::dib_bytes()?
            .and_then(|dib| dib_to_bmp(&dib))
            .map(|bytes| ClipboardImage {
                extension: "bmp".into(),
                bytes,
            }),
    };
    Ok(ClipboardSnapshot {
        files: raw.files.iter().map(PathBuf::from).collect(),
        image,
        text: raw.text,
    })
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_snapshot(_temp_dir: &Path) -> Result<ClipboardSnapshot, ShellError> {
    Ok(ClipboardSnapshot::default())
}

#[cfg(target_os = "macos")]
mod macos_osascript {
    //! AppleScript fallbacks for the two reads the AppKit bindings here cannot do directly: a
    //! file URL the API could not turn into a path, and a PNG coerced from a TIFF or other bitmap.
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn run(lines: &[&str]) -> Option<String> {
        let mut cmd = Command::new("/usr/bin/osascript");
        for line in lines {
            cmd.arg("-e").arg(line);
        }
        let output = cmd.output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    pub fn file_paths() -> Vec<String> {
        let out = run(&[
            "use framework \"AppKit\"",
            "use scripting additions",
            "set pb to current application's NSPasteboard's generalPasteboard()",
            "set urlClass to current application's NSURL",
            "set urls to pb's readObjectsForClasses:{urlClass} options:(missing value)",
            "if urls is missing value then return \"\"",
            "set out to {}",
            "repeat with u in urls",
            "if (u's isFileURL()) as boolean then set end of out to (u's |path|()) as text",
            "end repeat",
            "set AppleScript's text item delimiters to linefeed",
            "return out as text",
        ])
        .unwrap_or_default();
        out.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// The clipboard image as PNG bytes, staged in `dir` and removed after reading.
    pub fn png_bytes(dir: &Path) -> Option<Vec<u8>> {
        std::fs::create_dir_all(dir).ok()?;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!(
            "clipboard-{nanos}-{}-coerced.png",
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let quoted = format!(
            "\"{}\"",
            path.to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        );
        let set_out_path = format!("set outPath to POSIX file {quoted}");
        let wrote = run(&[
            "try",
            "set imageData to (the clipboard as «class PNGf»)",
            set_out_path.as_str(),
            "set fileRef to open for access outPath with write permission",
            "set eof fileRef to 0",
            "write imageData to fileRef",
            "close access fileRef",
            "return \"ok\"",
            "on error",
            "try",
            "close access outPath",
            "end try",
            "return \"\"",
            "end try",
        ])
        .is_some_and(|s| s == "ok");
        let bytes = if wrote && path.is_file() {
            std::fs::read(&path).ok()
        } else {
            None
        };
        let _ = std::fs::remove_file(&path);
        bytes.filter(|b| !b.is_empty())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
    use objc2_foundation::{ns_string, NSArray, NSData, NSString, NSURL};

    const T_TEXT: &str = "public.utf8-plain-text";
    const T_HTML: &str = "public.html";
    const T_PNG: &str = "public.png";
    const T_FILE: &str = "public.file-url";

    fn board(b: &Board) -> Retained<NSPasteboard> {
        match b {
            Board::General => NSPasteboard::generalPasteboard(),
            Board::Named(n) => NSPasteboard::pasteboardWithName(&NSString::from_str(n)),
        }
    }

    pub fn write(b: &Board, c: &ClipboardContent) -> Result<(), String> {
        let pb = board(b);
        pb.clearContents();
        let mut items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = Vec::new();
        let has_primary =
            c.text.is_some() || c.html.is_some() || c.png.is_some() || !c.custom.is_empty();
        if has_primary {
            let item = NSPasteboardItem::new();
            if let Some(t) = &c.text {
                item.setString_forType(&NSString::from_str(t), &NSString::from_str(T_TEXT));
            }
            if let Some(h) = &c.html {
                item.setString_forType(&NSString::from_str(h), &NSString::from_str(T_HTML));
            }
            if let Some(p) = &c.png {
                item.setData_forType(&NSData::with_bytes(p), &NSString::from_str(T_PNG));
            }
            for (k, v) in &c.custom {
                item.setData_forType(&NSData::with_bytes(v), &NSString::from_str(k));
            }
            items.push(ProtocolObject::from_retained(item));
        }
        for f in &c.files {
            let url = NSURL::fileURLWithPath(&NSString::from_str(f));
            let abs = url
                .absoluteString()
                .ok_or_else(|| format!("no file URL for {f}"))?;
            let item = NSPasteboardItem::new();
            item.setString_forType(&abs, &NSString::from_str(T_FILE));
            items.push(ProtocolObject::from_retained(item));
        }
        if items.is_empty() {
            return Ok(());
        }
        let array = NSArray::from_retained_slice(&items);
        if pb.writeObjects(&array) {
            Ok(())
        } else {
            Err("NSPasteboard rejected the content".into())
        }
    }

    pub fn read(b: &Board) -> Result<ClipboardContent, String> {
        let pb = board(b);
        let mut out = ClipboardContent::default();
        let Some(items) = pb.pasteboardItems() else {
            return Ok(out);
        };
        for item in items.iter() {
            let types: Vec<String> = item.types().iter().map(|t| t.to_string()).collect();
            for t in &types {
                let ty = NSString::from_str(t);
                match t.as_str() {
                    T_TEXT if out.text.is_none() => {
                        out.text = item.stringForType(&ty).map(|s| s.to_string())
                    }
                    T_HTML if out.html.is_none() => {
                        out.html = item.stringForType(&ty).map(|s| s.to_string())
                    }
                    T_PNG if out.png.is_none() => {
                        out.png = item.dataForType(&ty).map(|d| d.to_vec())
                    }
                    T_FILE => {
                        if let Some(s) = item.stringForType(&ty) {
                            if let Some(url) = NSURL::URLWithString(&s) {
                                if let Some(p) = url.path() {
                                    out.files.push(p.to_string());
                                }
                            }
                        }
                    }
                    _ => {
                        let dynamic = t.starts_with("dyn.");
                        let known = matches!(
                            t.as_str(),
                            T_TEXT
                                | T_HTML
                                | T_PNG
                                | T_FILE
                                | "NSStringPboardType"
                                | "public.url"
                                | "public.url-name"
                                | "public.utf16-external-plain-text"
                        );
                        if !known && !dynamic {
                            if let Some(d) = item.dataForType(&ty) {
                                out.custom.entry(t.clone()).or_insert_with(|| d.to_vec());
                            }
                        }
                    }
                }
            }
        }
        let _ = ns_string!("");
        Ok(out)
    }

    pub fn formats(b: &Board) -> Result<Vec<String>, String> {
        Ok(board(b)
            .types()
            .map(|t| t.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default())
    }
}

#[cfg(windows)]
mod imp {
    //! NOT compiled on the macOS host: verify with `rightkit cargo check -p rightkit-shell` on the
    //! Windows agent (see README).
    use super::*;
    use crate::winapi::windows::core::PCWSTR;
    use crate::winapi::windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
    use crate::winapi::windows::Win32::System::DataExchange::*;
    use crate::winapi::windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
    };
    use crate::winapi::windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;
    const CF_DIB: u32 = 8;

    struct Open;
    impl Open {
        fn new() -> Result<Self, String> {
            // The clipboard can be briefly held by another process: retry a few times.
            for _ in 0..10 {
                if unsafe { OpenClipboard(Some(HWND::default())) }.is_ok() {
                    return Ok(Open);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err("clipboard is busy".into())
        }
    }
    impl Drop for Open {
        fn drop(&mut self) {
            let _ = unsafe { CloseClipboard() };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    fn format(name: &str) -> u32 {
        let w = wide(name);
        unsafe { RegisterClipboardFormatW(PCWSTR(w.as_ptr())) }
    }
    fn set(fmt: u32, bytes: &[u8]) -> Result<(), String> {
        unsafe {
            let h: HGLOBAL =
                GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)).map_err(|e| e.to_string())?;
            let p = GlobalLock(h) as *mut u8;
            if p.is_null() {
                return Err("GlobalLock failed".into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            let _ = GlobalUnlock(h);
            // Ownership of `h` passes to the clipboard on success.
            SetClipboardData(fmt, Some(HANDLE(h.0)))
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }
    fn get(fmt: u32) -> Option<Vec<u8>> {
        unsafe {
            let h = GetClipboardData(fmt).ok()?;
            let g = HGLOBAL(h.0);
            let size = GlobalSize(g);
            let p = GlobalLock(g) as *const u8;
            if p.is_null() {
                return None;
            }
            let v = std::slice::from_raw_parts(p, size).to_vec();
            let _ = GlobalUnlock(g);
            Some(v)
        }
    }
    fn utf16_bytes(s: &str) -> Vec<u8> {
        wide(s).into_iter().flat_map(|u| u.to_le_bytes()).collect()
    }
    /// CF_HTML wrapper with correct byte offsets.
    fn cf_html(fragment: &str) -> Vec<u8> {
        let pre = "<html><body><!--StartFragment-->";
        let post = "<!--EndFragment--></body></html>";
        let header = |a: usize, b: usize, c: usize, d: usize| {
            format!("Version:0.9\r\nStartHTML:{a:010}\r\nEndHTML:{b:010}\r\nStartFragment:{c:010}\r\nEndFragment:{d:010}\r\n")
        };
        let hl = header(0, 0, 0, 0).len();
        let sh = hl;
        let sf = hl + pre.len();
        let ef = sf + fragment.len();
        let eh = ef + post.len();
        format!("{}{pre}{fragment}{post}", header(sh, eh, sf, ef)).into_bytes()
    }
    fn hdrop(files: &[String]) -> Vec<u8> {
        // DROPFILES { pFiles: u32 = 20, pt: POINT(0,0), fNC: BOOL(0), fWide: BOOL(1) } + double-NUL list.
        let mut out = vec![0u8; 20];
        out[0..4].copy_from_slice(&20u32.to_le_bytes());
        out[16..20].copy_from_slice(&1u32.to_le_bytes());
        for f in files {
            out.extend(utf16_bytes(f));
        }
        out.extend([0, 0]);
        out
    }

    pub fn write(b: &Board, c: &ClipboardContent) -> Result<(), String> {
        if matches!(b, Board::Named(_)) {
            return Err("named boards are macOS-only".into());
        }
        let _open = Open::new()?;
        unsafe { EmptyClipboard() }.map_err(|e| e.to_string())?;
        if let Some(t) = &c.text {
            set(CF_UNICODETEXT, &utf16_bytes(t))?;
        }
        if let Some(h) = &c.html {
            set(format("HTML Format"), &cf_html(h))?;
        }
        if let Some(p) = &c.png {
            set(format("PNG"), p)?;
        }
        if !c.files.is_empty() {
            set(CF_HDROP, &hdrop(&c.files))?;
        }
        for (k, v) in &c.custom {
            set(format(k), v)?;
        }
        Ok(())
    }

    pub fn read(b: &Board) -> Result<ClipboardContent, String> {
        if matches!(b, Board::Named(_)) {
            return Err("named boards are macOS-only".into());
        }
        let _open = Open::new()?;
        let mut out = ClipboardContent::default();
        if let Some(raw) = get(CF_UNICODETEXT) {
            let u: Vec<u16> = raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .take_while(|u| *u != 0)
                .collect();
            out.text = Some(String::from_utf16_lossy(&u));
        }
        if let Some(raw) = get(format("HTML Format")) {
            let s = String::from_utf8_lossy(&raw).into_owned();
            let field = |k: &str| {
                s.split("\r\n")
                    .find_map(|l| l.strip_prefix(k))
                    .and_then(|v| v.trim().parse::<usize>().ok())
            };
            if let (Some(a), Some(z)) = (field("StartFragment:"), field("EndFragment:")) {
                out.html = s.get(a..z).map(str::to_owned);
            }
        }
        out.png = get(format("PNG"));
        unsafe {
            if let Ok(h) = GetClipboardData(CF_HDROP) {
                let drop = HDROP(h.0);
                let n = DragQueryFileW(drop, u32::MAX, None);
                for i in 0..n {
                    let len = DragQueryFileW(drop, i, None) as usize;
                    let mut buf = vec![0u16; len + 1];
                    DragQueryFileW(drop, i, Some(&mut buf));
                    out.files.push(String::from_utf16_lossy(&buf[..len]));
                }
            }
        }
        Ok(out)
    }

    /// The raw `CF_DIB` blob (BITMAPINFO plus pixels), copied out under its own open/close.
    pub fn dib_bytes() -> Result<Option<Vec<u8>>, String> {
        let _open = Open::new()?;
        Ok(get(CF_DIB))
    }

    pub fn formats(_b: &Board) -> Result<Vec<String>, String> {
        let _open = Open::new()?;
        let mut out = Vec::new();
        let mut f = 0u32;
        loop {
            f = unsafe { EnumClipboardFormats(f) };
            if f == 0 {
                break;
            }
            let mut buf = [0u16; 128];
            let n = unsafe { GetClipboardFormatNameW(f, &mut buf) };
            out.push(if n > 0 {
                String::from_utf16_lossy(&buf[..n as usize])
            } else {
                format!("cf-{f}")
            });
        }
        Ok(out)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use super::*;
    pub fn write(_: &Board, _: &ClipboardContent) -> Result<(), String> {
        Err("rich clipboard is not implemented on this platform".into())
    }
    pub fn read(_: &Board) -> Result<ClipboardContent, String> {
        Err("rich clipboard is not implemented on this platform".into())
    }
    pub fn formats(_: &Board) -> Result<Vec<String>, String> {
        Err("rich clipboard is not implemented on this platform".into())
    }
}
