//! The compressed part of PNG validation: the exact decoded length a header implies, a bounded
//! inflate of the concatenated IDAT stream, and the per-scanline filter bytes.
//!
//! Everything here is pure and cross-platform. The only allocation that depends on untrusted input
//! is the inflate output buffer, and it is sized from the header's expected decoded length (itself
//! capped by `MAX_DECODED_BYTES`) plus one byte, never from the compressed data. A zlib bomb
//! therefore stops after `expected + 1` output bytes, whatever the compressed size.

use crate::limits::Reject;
use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
use miniz_oxide::inflate::TINFLStatus;

/// Largest decoded (inflated) IDAT stream accepted. The largest header the crate admits is
/// 262,144 pixels (`limits::MAX_PIXELS`); at 16-bit RGBA (8 bytes a pixel) that is 2 MiB plus one
/// filter byte per scanline, and Adam7 adds a few bytes per pass row, so 4 MiB covers every
/// admitted header. A header that implies more is rejected before any buffer exists.
pub(crate) const MAX_DECODED_BYTES: u64 = 4 * 1024 * 1024;

/// The IHDR fields the decoded length depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    pub width: u32,
    pub height: u32,
    pub depth: u8,
    pub color: u8,
    /// 0 none, 1 Adam7. Any other value is not a PNG.
    pub interlace: u8,
}

impl Header {
    /// Bits per pixel: samples per pixel for the colour type times the bit depth.
    pub(crate) fn bits_per_pixel(&self) -> Option<u64> {
        let samples: u64 = match self.color {
            0 | 3 => 1,
            4 => 2,
            2 => 3,
            6 => 4,
            _ => return None,
        };
        Some(samples * u64::from(self.depth))
    }
}

/// Adam7 passes as `(x start, y start, x step, y step)`.
const ADAM7: [(u64, u64, u64, u64); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// Pixels of one pass along one axis: `ceil((size - start) / step)`, 0 when the pass starts
/// outside the image.
fn pass_extent(size: u64, start: u64, step: u64) -> u64 {
    if size <= start {
        0
    } else {
        (size - start).div_ceil(step)
    }
}

/// The scanline layout of the decoded stream as `(rows, bytes per row including the filter
/// byte)` per non-empty pass: one entry without interlacing, up to seven with Adam7. All
/// arithmetic is checked; overflow is `Reject::Arithmetic`.
pub(crate) fn scanlines(header: &Header) -> Result<Vec<(u64, u64)>, Reject> {
    let bits = header.bits_per_pixel().ok_or(Reject::PngStructure)?;
    if header.width == 0 || header.height == 0 {
        return Err(Reject::Dimensions);
    }
    // bytes of one row of `pixels` pixels, plus the filter byte
    let stride = |pixels: u64| -> Result<u64, Reject> {
        pixels
            .checked_mul(bits)
            .and_then(|b| b.checked_add(7))
            .map(|b| b / 8)
            .and_then(|b| b.checked_add(1))
            .ok_or(Reject::Arithmetic)
    };
    let (width, height) = (u64::from(header.width), u64::from(header.height));
    match header.interlace {
        0 => Ok(vec![(height, stride(width)?)]),
        1 => {
            let mut passes = Vec::with_capacity(ADAM7.len());
            for (x0, y0, dx, dy) in ADAM7 {
                let (pw, ph) = (pass_extent(width, x0, dx), pass_extent(height, y0, dy));
                if pw > 0 && ph > 0 {
                    passes.push((ph, stride(pw)?));
                }
            }
            Ok(passes)
        }
        _ => Err(Reject::PngStructure),
    }
}

/// Exact decoded length the header implies, with checked arithmetic. Over
/// `MAX_DECODED_BYTES` is `Reject::PngLength`.
pub(crate) fn expected_len(header: &Header) -> Result<usize, Reject> {
    let mut total: u64 = 0;
    for (rows, stride) in scanlines(header)? {
        let part = rows.checked_mul(stride).ok_or(Reject::Arithmetic)?;
        total = total.checked_add(part).ok_or(Reject::Arithmetic)?;
    }
    if total > MAX_DECODED_BYTES {
        return Err(Reject::PngLength);
    }
    usize::try_from(total).map_err(|_| Reject::Arithmetic)
}

/// Inflate a zlib stream whose decoded length must be exactly `expected` bytes.
///
/// The output buffer is `expected + 1` bytes: that one extra byte is how "more data than the header
/// allows" is told apart from "exactly right", and it is the hard output cap (a bomb cannot write
/// past it). The zlib header, the deflate data and the Adler-32 checksum must all be valid and the
/// stream must end exactly at the end of `zlib` (no trailing bytes). Errors: `PngInflate` for a bad
/// or truncated stream, a bad checksum or trailing bytes; `PngLength` for more or fewer decoded
/// bytes than `expected`.
pub(crate) fn inflate_exact(zlib: &[u8], expected: usize) -> Result<Vec<u8>, Reject> {
    if expected as u64 > MAX_DECODED_BYTES {
        return Err(Reject::PngLength);
    }
    let mut out = vec![0u8; expected + 1];
    let mut state = Box::<DecompressorOxide>::default();
    let flags = inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER
        | inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let (status, used, produced) = decompress(&mut state, zlib, &mut out, 0, flags);
    match status {
        TINFLStatus::Done => {}
        TINFLStatus::HasMoreOutput => return Err(Reject::PngLength),
        // Failed, Adler32Mismatch, truncated input, bad parameter: all a damaged stream.
        _ => return Err(Reject::PngInflate),
    }
    if used != zlib.len() {
        return Err(Reject::PngInflate); // bytes after the end of the zlib stream
    }
    if produced != expected {
        return Err(Reject::PngLength);
    }
    out.truncate(expected);
    Ok(out)
}

/// Every scanline starts with a filter type byte that must be 0..=4. `decoded` is exactly
/// `expected_len(header)` bytes long.
pub(crate) fn check_filters(header: &Header, decoded: &[u8]) -> Result<(), Reject> {
    let mut offset: usize = 0;
    for (rows, stride) in scanlines(header)? {
        let stride = usize::try_from(stride).map_err(|_| Reject::Arithmetic)?;
        for _ in 0..rows {
            match decoded.get(offset) {
                Some(&filter) if filter <= 4 => {}
                _ => return Err(Reject::PngStructure),
            }
            offset = offset.checked_add(stride).ok_or(Reject::Arithmetic)?;
        }
    }
    if offset != decoded.len() {
        return Err(Reject::PngLength);
    }
    Ok(())
}

/// Full check of an IDAT stream against its header: exact decoded length, valid zlib, filter bytes.
pub(crate) fn validate_idat(header: &Header, zlib: &[u8]) -> Result<(), Reject> {
    let expected = expected_len(header)?;
    let decoded = inflate_exact(zlib, expected)?;
    check_filters(header, &decoded)
}
