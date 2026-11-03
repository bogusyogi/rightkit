//! PNG frame conversion for the ffmpeg `image2pipe` input.
//!
//! The LGPL ffmpeg builds in the approved catalog have no PNG decoder, so a captured PNG is
//! decoded here and rewritten as a PPM (P6, RGB) frame, or, when the output keeps alpha, a PAM
//! (P7, `RGB_ALPHA`) frame. Both are lossless and ffmpeg's native `ppm` and `pam` decoders read
//! them.

use crate::render::RenderError;

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Width and height from a PNG's IHDR chunk, without decoding the image.
pub fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != PNG_SIGNATURE || &png[12..16] != b"IHDR" {
        return None;
    }
    let be = |at: usize| u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
    Some((be(16), be(20)))
}

fn decode(png: &[u8]) -> Result<image::DynamicImage, RenderError> {
    image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|e| RenderError::Decode(format!("not a PNG frame: {e}")))
}

/// Decode a PNG and write it as a binary PPM (`P6`, 8-bit RGB). Alpha is dropped, not
/// composited: use [`png_to_pam`] when the output keeps transparency.
pub fn png_to_ppm(png: &[u8]) -> Result<Vec<u8>, RenderError> {
    let rgb = decode(png)?.to_rgb8();
    let (w, h) = rgb.dimensions();
    let header = format!("P6\n{w} {h}\n255\n");
    let mut out = Vec::with_capacity(header.len() + rgb.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&rgb);
    Ok(out)
}

/// Decode a PNG and write it as a PAM (`P7`, 8-bit `RGB_ALPHA`, straight alpha). A PNG without
/// an alpha channel gets an opaque one.
pub fn png_to_pam(png: &[u8]) -> Result<Vec<u8>, RenderError> {
    let rgba = decode(png)?.to_rgba8();
    let (w, h) = rgba.dimensions();
    let header =
        format!("P7\nWIDTH {w}\nHEIGHT {h}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n");
    let mut out = Vec::with_capacity(header.len() + rgba.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&rgba);
    Ok(out)
}
