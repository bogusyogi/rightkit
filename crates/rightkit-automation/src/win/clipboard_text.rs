//! Eager Win32 clipboard text encodings.

use windows::core::PCSTR;
use windows::Win32::Globalization::{
    GetLocaleInfoW, GetThreadLocale, WideCharToMultiByte, LOCALE_IDEFAULTANSICODEPAGE,
    LOCALE_IDEFAULTCODEPAGE,
};

/// Encodes the Unicode, ANSI, OEM and locale formats before opening the clipboard.
/// Publishing all four under one open prevents automatic text format synthesis on close.
pub fn eager_text_formats(text: &str) -> Option<[(u32, Vec<u8>); 4]> {
    const CF_TEXT: u32 = 1;
    const CF_OEMTEXT: u32 = 7;
    const CF_UNICODETEXT: u32 = 13;
    const CF_LOCALE: u32 = 16;

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let end = wide.iter().position(|&unit| unit == 0)?;
    // SAFETY: reads the calling thread's locale without mutating it.
    let locale = unsafe { GetThreadLocale() };
    let ansi = multibyte(
        &wide[..=end],
        code_page(locale, LOCALE_IDEFAULTANSICODEPAGE)?,
    )?;
    let oem = multibyte(&wide[..=end], code_page(locale, LOCALE_IDEFAULTCODEPAGE)?)?;
    let unicode = wide.into_iter().flat_map(u16::to_le_bytes).collect();
    Some([
        (CF_UNICODETEXT, unicode),
        (CF_TEXT, ansi),
        (CF_OEMTEXT, oem),
        (CF_LOCALE, locale.to_le_bytes().to_vec()),
    ])
}

fn code_page(locale: u32, kind: u32) -> Option<u32> {
    // Windows guarantees at most six UTF-16 units including the terminator.
    let mut value = [0; 6];
    // SAFETY: the sized buffer is valid for the duration of the call.
    let written =
        usize::try_from(unsafe { GetLocaleInfoW(locale, kind, Some(&mut value)) }).ok()?;
    let digits = value.get(..written.checked_sub(1)?)?;
    String::from_utf16(digits).ok()?.parse().ok()
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
