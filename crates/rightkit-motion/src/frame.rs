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
    let header = format!("P7\nWIDTH {w}\nHEIGHT {h}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n");
    let mut out = Vec::with_capacity(header.len() + rgba.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&rgba);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(img: &image::DynamicImage) -> Vec<u8> {
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    #[test]
    fn a_png_frame_becomes_a_p6_ppm() {
        let img = image::RgbImage::from_pixel(3, 2, image::Rgb([10, 20, 30]));
        let ppm = png_to_ppm(&png_of(&img.into())).unwrap();
        let header = b"P6\n3 2\n255\n";
        assert!(ppm.starts_with(header));
        assert_eq!(ppm.len(), header.len() + 3 * 2 * 3);
        assert_eq!(&ppm[header.len()..header.len() + 3], &[10, 20, 30]);
    }

    #[test]
    fn a_png_with_alpha_becomes_a_p7_pam_with_straight_alpha() {
        let mut img = image::RgbaImage::from_pixel(2, 2, image::Rgba([200, 100, 50, 255]));
        img.put_pixel(1, 0, image::Rgba([1, 2, 3, 0]));
        let pam = png_to_pam(&png_of(&img.into())).unwrap();
        let header = b"P7\nWIDTH 2\nHEIGHT 2\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n";
        assert!(pam.starts_with(header));
        assert_eq!(pam.len(), header.len() + 2 * 2 * 4);
        let px = &pam[header.len()..];
        assert_eq!(&px[0..4], &[200, 100, 50, 255]);
        assert_eq!(&px[4..8], &[1, 2, 3, 0], "a transparent pixel keeps its straight RGB");
    }

    #[test]
    fn an_opaque_png_gets_an_opaque_pam_alpha() {
        let img = image::RgbImage::from_pixel(1, 1, image::Rgb([9, 8, 7]));
        let pam = png_to_pam(&png_of(&img.into())).unwrap();
        assert_eq!(&pam[pam.len() - 4..], &[9, 8, 7, 255]);
    }

    #[test]
    fn non_png_bytes_are_a_typed_decode_error() {
        for result in [png_to_ppm(b"not a png"), png_to_pam(b"not a png")] {
            let error = result.unwrap_err();
            assert_eq!(error.code(), "capture_decode");
        }
    }

    #[test]
    fn png_size_is_read_from_the_header() {
        let img = image::RgbImage::from_pixel(7, 5, image::Rgb([1, 2, 3]));
        assert_eq!(png_dimensions(&png_of(&img.into())), Some((7, 5)));
        assert_eq!(png_dimensions(b"nope"), None);
    }
}
