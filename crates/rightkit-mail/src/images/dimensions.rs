//! Remote image checks: allowed MIME types, magic bytes matching the MIME, and decoded
//! dimension limits read from the header only (PNG, GIF, JPEG, WebP), so a hostile image is
//! refused before it is decoded.

/// Largest width or height accepted, in pixels.
pub const MAX_DIMENSION: u32 = 16_384;
/// Largest decoded pixel count accepted.
pub const MAX_DECODED_PIXELS: u64 = 50_000_000;

pub fn is_allowed_image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

pub fn image_magic_matches(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => false,
    }
}

pub fn image_dimensions_within_limit(mime: &str, bytes: &[u8]) -> bool {
    let Some((width, height)) = image_dimensions(mime, bytes) else {
        return false;
    };
    dimensions_within_limit(width, height)
}

pub fn dimensions_within_limit(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_DIMENSION
        && height <= MAX_DIMENSION
        && u64::from(width) * u64::from(height) <= MAX_DECODED_PIXELS
}

fn image_dimensions(mime: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    match mime {
        "image/png" => png_dimensions(bytes),
        "image/jpeg" => jpeg_dimensions(bytes),
        "image/gif" => gif_dimensions(bytes),
        "image/webp" => webp_dimensions(bytes),
        _ => None,
    }
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

fn gif_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 10 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return None;
    }
    let width = u16::from_le_bytes(bytes[6..8].try_into().ok()?);
    let height = u16::from_le_bytes(bytes[8..10].try_into().ok()?);
    Some((u32::from(width), u32::from(height)))
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || !bytes.starts_with(b"\xff\xd8") {
        return None;
    }
    let mut i = 2usize;
    while i + 3 < bytes.len() {
        if bytes[i] != 0xff {
            i += 1;
            continue;
        }
        while i < bytes.len() && bytes[i] == 0xff {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let marker = bytes[i];
        i += 1;
        if marker == 0xd8 || marker == 0xd9 || (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            continue;
        }
        if i + 1 >= bytes.len() {
            return None;
        }
        let len = u16::from_be_bytes(bytes[i..i + 2].try_into().ok()?) as usize;
        if len < 2 || i + len > bytes.len() {
            return None;
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            if len < 7 {
                return None;
            }
            let height = u16::from_be_bytes(bytes[i + 3..i + 5].try_into().ok()?);
            let width = u16::from_be_bytes(bytes[i + 5..i + 7].try_into().ok()?);
            return Some((u32::from(width), u32::from(height)));
        }
        i += len;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 30 || !bytes.starts_with(b"RIFF") || &bytes[8..12] != b"WEBP" {
        return None;
    }
    match &bytes[12..16] {
        b"VP8X" => {
            let width = 1 + read_u24_le(&bytes[24..27])?;
            let height = 1 + read_u24_le(&bytes[27..30])?;
            Some((width, height))
        }
        b"VP8L" => {
            if bytes[20] != 0x2f {
                return None;
            }
            let b1 = bytes[21] as u32;
            let b2 = bytes[22] as u32;
            let b3 = bytes[23] as u32;
            let b4 = bytes[24] as u32;
            let width = 1 + b1 + ((b2 & 0x3f) << 8);
            let height = 1 + (b2 >> 6) + (b3 << 2) + ((b4 & 0x0f) << 10);
            Some((width, height))
        }
        b"VP8 " => {
            if &bytes[23..26] != b"\x9d\x01\x2a" {
                return None;
            }
            let width = u16::from_le_bytes(bytes[26..28].try_into().ok()?) & 0x3fff;
            let height = u16::from_le_bytes(bytes[28..30].try_into().ok()?) & 0x3fff;
            Some((u32::from(width), u32::from(height)))
        }
        _ => None,
    }
}

fn read_u24_le(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 3 {
        return None;
    }
    Some(bytes[0] as u32 | ((bytes[1] as u32) << 8) | ((bytes[2] as u32) << 16))
}
