//! Eager Win32 clipboard text encodings.

use windows::core::PCSTR;
use windows::Win32::Globalization::{GetThreadLocale, WideCharToMultiByte};

/// Encodes the Unicode, ANSI, OEM and locale formats before opening the clipboard.
/// Publishing all four under one open prevents automatic text format synthesis on close.
pub fn eager_text_formats(text: &str) -> Option<[(u32, Vec<u8>); 4]> {
    const CF_TEXT: u32 = 1;
    const CF_OEMTEXT: u32 = 7;
    const CF_UNICODETEXT: u32 = 13;
    const CF_LOCALE: u32 = 16;

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let end = wide.iter().position(|&unit| unit == 0)?;
    let ansi = multibyte(&wide[..=end], 0)?;
    let oem = multibyte(&wide[..=end], 1)?;
    // SAFETY: reads the calling thread's locale without mutating it.
    let locale = unsafe { GetThreadLocale() }.to_le_bytes().to_vec();
    let unicode = wide.into_iter().flat_map(u16::to_le_bytes).collect();
    Some([
        (CF_UNICODETEXT, unicode),
        (CF_TEXT, ansi),
        (CF_OEMTEXT, oem),
        (CF_LOCALE, locale),
    ])
}

fn multibyte(wide: &[u16], codepage: u32) -> Option<Vec<u8>> {
    i32::try_from(wide.len()).ok()?;
    // SAFETY: these calls read the supplied slice and use a sized buffer when encoding.
    let size = unsafe { WideCharToMultiByte(codepage, 0, wide, None, PCSTR::null(), None) };
    if size <= 0 {
        return None;
    }
    let mut bytes = vec![0; size as usize];
    let written =
        unsafe { WideCharToMultiByte(codepage, 0, wide, Some(&mut bytes), PCSTR::null(), None) };
    (written == size).then_some(bytes)
}
