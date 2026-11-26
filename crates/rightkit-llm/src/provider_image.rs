//! Scale an image down to a provider's longest-edge limit (`provider-image` feature).
//!
//! A model provider refuses or silently downsamples an image whose longest edge is over its
//! limit. A host that reads image files for a model calls [`scale_image_to_max_edge`] with the
//! limit of the route it is about to call; the limit is the caller's, this module holds none.

use std::io::Cursor;

use ::image::{imageops::FilterType, load_from_memory_with_format, ImageFormat};

/// A PNG or JPEG whose longest edge is over `max_edge`, scaled to fit inside `max_edge` by
/// `max_edge` (the aspect ratio kept, a triangle filter) and re-encoded in the same format.
///
/// `bytes` is the encoded image, `mime` its media type (`image/png` or `image/jpeg`; anything else
/// is left alone) and `width` and `height` its pixel size as the caller already read it from the
/// header, so an image that needs no scaling is never decoded. Returns the new bytes with the new
/// width and height, or `None` when no scaling is needed (the longest edge is at most
/// `max_edge`), the format is not PNG or JPEG, or the image cannot be decoded or encoded; the
/// caller then sends the original bytes.
pub fn scale_image_to_max_edge(
    bytes: &[u8],
    mime: &str,
    width: u32,
    height: u32,
    max_edge: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    if width.max(height) <= max_edge {
        return None;
    }
    let format = match mime {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        _ => return None,
    };
    let decoded = load_from_memory_with_format(bytes, format).ok()?;
    let scaled = decoded.resize(max_edge, max_edge, FilterType::Triangle);
    let (w, h) = (scaled.width(), scaled.height());
    let mut out = Cursor::new(Vec::new());
    scaled.write_to(&mut out, format).ok()?;
    Some((out.into_inner(), w, h))
}
