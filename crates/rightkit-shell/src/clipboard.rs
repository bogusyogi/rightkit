//! Rich clipboard: text, HTML, PNG image, file lists and app-defined formats, in one transaction.
//!
//! Donors: HeardRight `clipboard` + delivery (text/HTML/image, paste-sequence discipline) and
//! ViewRight file grants (paths, not bytes). macOS uses `NSPasteboard` (one primary item that
//! carries every representation, plus one item per file, which is how Finder lays files out);
//! Windows uses the Win32 clipboard (`CF_UNICODETEXT`, `HTML Format`, `PNG`, `CF_HDROP`).
//! `Board::Named` targets a private pasteboard so tests and apps can stage content without
//! touching what the user copied (macOS; on Windows only the general clipboard exists).

use std::collections::BTreeMap;

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
    //! NOT compiled on this (macOS) host: verify with `rightkit cargo check -p rightkit-shell` on
    //! the Windows agent (see API.md).
    use super::*;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;

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
                .chunks_exact(2)
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
