//! Decode declared passive image bytes and encode fresh PNG pixels.

use std::io::{self, Cursor, Write};
use std::panic::catch_unwind;

use flate2::{Decompress, FlushDecompress, Status};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::metadata::Orientation;
use image::{ColorType, ImageEncoder};
use thiserror::Error;
use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
use zune_jpeg::JpegDecoder;

mod apng;
mod gif;

pub use apng::{canonicalize_apng, ApngPreviewLimits, CanonicalApng};
pub use gif::{canonicalize_gif, CanonicalGif, GifPreviewLimits};

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const PNG_END: &[u8] = b"\0\0\0\0IEND\xaeB\x60\x82";
const JPEG_SIGNATURE: &[u8] = b"\xff\xd8";
const CODEC_OVERHEAD: usize = 256 * 1024;
const JPEG_MAX_SCANS: usize = 100;

/// Per-call limits for canonicalization.
#[derive(Debug, Clone, Copy)]
pub struct PassiveImageLimits {
    pub max_input_bytes: usize,
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: u64,
    /// Caps crate-owned buffers and gates codec work using checked estimates.
    /// Codec-internal allocation limits are best effort.
    pub max_working_bytes: usize,
    pub max_output_bytes: usize,
}

/// A freshly encoded static PNG. Width and height reflect EXIF orientation.
#[derive(Debug)]
pub struct CanonicalPng {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

impl CanonicalPng {
    pub const MIME: &str = "image/png";
}

#[derive(Debug, Error)]
pub enum PassiveImageError {
    #[error("unsupported passive image media type")]
    UnsupportedMediaType,
    #[error("passive image bytes do not match the declared media type")]
    MediaTypeMismatch,
    #[error("passive image exceeds the input byte limit")]
    InputLimit,
    #[error("passive image exceeds the canvas limit")]
    CanvasLimit,
    #[error("passive image exceeds the working byte budget")]
    WorkingLimit,
    #[error("canonical image exceeds the output byte limit")]
    OutputLimit,
    #[error("animated or multi-picture images are unsupported")]
    MultipleImages,
    #[error("malformed or unsupported passive image")]
    Malformed,
    #[error("failed to encode canonical image")]
    Encoding,
}

use PassiveImageError::{
    CanvasLimit, Encoding, InputLimit, Malformed, MediaTypeMismatch, MultipleImages, OutputLimit,
    UnsupportedMediaType, WorkingLimit,
};

type Result<T> = std::result::Result<T, PassiveImageError>;

/// Canonicalize one static image. Accepts PNG and JPEG media types, case-insensitively,
/// with optional parameters. Only the newly encoded bytes should reach a renderer.
pub fn canonicalize(
    bytes: &[u8],
    declared_mime: &str,
    limits: PassiveImageLimits,
) -> Result<CanonicalPng> {
    if bytes.len() > limits.max_input_bytes {
        return Err(InputLimit);
    }
    let mime = declared_mime.split(';').next().unwrap_or_default().trim();
    let png = mime.eq_ignore_ascii_case("image/png");
    let signature = if png {
        PNG_SIGNATURE
    } else if mime.eq_ignore_ascii_case("image/jpeg") {
        JPEG_SIGNATURE
    } else {
        return Err(UnsupportedMediaType);
    };
    if !bytes.starts_with(signature) {
        return Err(MediaTypeMismatch);
    }

    catch_unwind(|| {
        let (raster, orientation) = if png {
            decode_png(bytes, limits)?
        } else {
            decode_jpeg(bytes, limits)?
        };
        encode_png(raster.orient(orientation, limits)?, limits)
    })
    .map_err(|_| Malformed)?
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Canvas {
    width: u32,
    height: u32,
}

impl Canvas {
    fn pixels(self, limits: PassiveImageLimits) -> Result<usize> {
        let pixels = u64::from(self.width) * u64::from(self.height);
        if pixels == 0
            || self.width > limits.max_width
            || self.height > limits.max_height
            || pixels > limits.max_pixels
        {
            return Err(CanvasLimit);
        }
        usize::try_from(pixels).map_err(|_| WorkingLimit)
    }

    fn oriented(self, orientation: Orientation, limits: PassiveImageLimits) -> Result<Self> {
        let canvas = if orientation.to_exif() >= 5 {
            Self {
                width: self.height,
                height: self.width,
            }
        } else {
            self
        };
        canvas.pixels(limits)?;
        Ok(canvas)
    }
}

struct Raster {
    canvas: Canvas,
    color: ColorType,
    bytes: Vec<u8>,
}

impl Raster {
    fn orient(self, orientation: Orientation, limits: PassiveImageLimits) -> Result<Self> {
        if orientation == Orientation::NoTransforms {
            return Ok(self);
        }

        let canvas = self.canvas.oriented(orientation, limits)?;
        remaining(limits, self.bytes.len().checked_mul(2))?;
        let mut bytes = zeroed(self.bytes.len())?;
        let width = self.canvas.width as usize;
        let height = self.canvas.height as usize;
        let stride = usize::from(self.color.bytes_per_pixel());
        for (index, pixel) in bytes.chunks_exact_mut(stride).enumerate() {
            let x = index % canvas.width as usize;
            let y = index / canvas.width as usize;
            let (source_x, source_y) = match orientation {
                Orientation::FlipHorizontal => (width - 1 - x, y),
                Orientation::Rotate180 => (width - 1 - x, height - 1 - y),
                Orientation::FlipVertical => (x, height - 1 - y),
                Orientation::Rotate90FlipH => (y, x),
                Orientation::Rotate90 => (y, height - 1 - x),
                Orientation::Rotate270FlipH => (width - 1 - y, height - 1 - x),
                Orientation::Rotate270 => (width - 1 - y, x),
                Orientation::NoTransforms => (x, y),
            };
            let offset = (source_y * width + source_x) * stride;
            pixel.copy_from_slice(&self.bytes[offset..offset + stride]);
        }
        Ok(Self {
            canvas,
            color: self.color,
            bytes,
        })
    }
}

fn remaining(limits: PassiveImageLimits, reserved: Option<usize>) -> Result<usize> {
    limits
        .max_working_bytes
        .checked_sub(reserved.ok_or(WorkingLimit)?)
        .ok_or(WorkingLimit)
}

fn zeroed(size: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|_| WorkingLimit)?;
    bytes.resize(size, 0);
    Ok(bytes)
}

fn png_error(error: png::DecodingError) -> PassiveImageError {
    match error {
        png::DecodingError::LimitsExceeded => WorkingLimit,
        _ => Malformed,
    }
}

fn sanitized_buffer(input_len: usize, limits: PassiveImageLimits) -> Result<Vec<u8>> {
    remaining(limits, Some(input_len))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(input_len)
        .map_err(|_| WorkingLimit)?;
    Ok(bytes)
}

fn png_header(bytes: &[u8]) -> Result<(&[u8], Canvas)> {
    // IHDR has a fixed size and must be first.
    let header = bytes.get(8..33).ok_or(Malformed)?;
    if !header.starts_with(b"\0\0\0\x0dIHDR") {
        return Err(Malformed);
    }
    let canvas = Canvas {
        width: u32::from_be_bytes(header[8..12].try_into().map_err(|_| Malformed)?),
        height: u32::from_be_bytes(header[12..16].try_into().map_err(|_| Malformed)?),
    };
    if canvas.width > 0x7fff_ffff || canvas.height > 0x7fff_ffff {
        return Err(Malformed);
    }
    Ok((header, canvas))
}

struct PngChunk<'a> {
    kind: &'a [u8],
    data: &'a [u8],
    bytes: &'a [u8],
}

fn next_png_chunk<'a>(chunks: &mut &'a [u8]) -> Result<PngChunk<'a>> {
    let source = *chunks;
    let header = source.get(..8).ok_or(Malformed)?;
    let size = u32::from_be_bytes(header[..4].try_into().map_err(|_| Malformed)?) as usize;
    let end = size.checked_add(12).ok_or(Malformed)?;
    let chunk = source.get(..end).ok_or(Malformed)?;
    let kind = &header[4..8];
    let data = &chunk[8..end - 4];
    if size > 0x7fff_ffff
        || !kind.iter().all(u8::is_ascii_alphabetic)
        || !kind[2].is_ascii_uppercase()
    {
        return Err(Malformed);
    }
    let crc = u32::from_be_bytes(chunk[end - 4..].try_into().map_err(|_| Malformed)?);
    if crc32fast::hash(&chunk[4..end - 4]) != crc {
        return Err(Malformed);
    }
    *chunks = &source[end..];
    Ok(PngChunk {
        kind,
        data,
        bytes: chunk,
    })
}

fn decode_png(bytes: &[u8], limits: PassiveImageLimits) -> Result<(Raster, Orientation)> {
    let (header, canvas) = png_header(bytes)?;
    let pixels = canvas.pixels(limits)?;
    let decoder_bytes = remaining(
        limits,
        pixels
            .checked_mul(8)
            .and_then(|size| size.checked_add(bytes.len()))
            .and_then(|size| size.checked_add(CODEC_OVERHEAD)),
    )?;

    let format = PngFormat::from_header(header)?;
    let mut stream = PngStream {
        inflater: Decompress::new(true),
        expected: format.filtered_bytes(canvas)?,
        ended: false,
    };
    let mut clean = sanitized_buffer(bytes.len(), limits)?;
    clean.extend_from_slice(PNG_SIGNATURE);
    let mut exif = None;
    let mut chunks = &bytes[8..];
    let mut in_data = false;
    let mut data_ended = false;
    let mut palette: Option<&[u8]> = None;
    let mut transparency = None;
    let mut scratch = [0; 8192];
    loop {
        let PngChunk {
            kind,
            data,
            bytes: chunk,
        } = next_png_chunk(&mut chunks)?;
        if kind != b"IDAT" && in_data {
            data_ended = true;
        }
        match kind {
            b"IHDR" if clean.len() == PNG_SIGNATURE.len() => clean.extend_from_slice(chunk),
            b"PLTE" if !in_data && transparency.is_none() && palette.is_none() => {
                format.check_palette(data.len())?;
                palette = Some(data);
                clean.extend_from_slice(chunk);
            }
            b"tRNS" if !in_data && transparency.is_none() => {
                format.check_transparency(data, palette.map(|palette| palette.len() / 3))?;
                transparency = Some(data);
                clean.extend_from_slice(chunk);
            }
            b"IDAT" if !data_ended => {
                if format.color == 3 && palette.is_none() {
                    return Err(Malformed);
                }
                stream.consume(data, &mut scratch)?;
                in_data = true;
                clean.extend_from_slice(chunk);
            }
            b"IEND" if data.is_empty() && in_data && chunks.is_empty() => {
                stream.consume(&[], &mut scratch)?;
                if !stream.ended {
                    return Err(Malformed);
                }
                clean.extend_from_slice(chunk);
                break;
            }
            b"IDAT" | b"IEND" | b"tRNS" => return Err(Malformed),
            b"acTL" | b"fcTL" | b"fdAT" => return Err(MultipleImages),
            b"eXIf" => record_exif(&mut exif, data)?,
            _ if kind[0].is_ascii_uppercase() => return Err(Malformed),
            _ => (),
        }
    }

    drop(stream);
    let orientation = exif.unwrap_or(Orientation::NoTransforms);
    canvas.oriented(orientation, limits)?;
    let mut options = png::DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    let mut decoder = png::Decoder::new_with_options(Cursor::new(clean), options);
    decoder.set_limits(png::Limits {
        bytes: decoder_bytes,
    });
    let indexed = format.color == 3;
    if !indexed {
        decoder.set_transformations(png::Transformations::EXPAND);
    }
    let mut reader = decoder.read_info().map_err(png_error)?;
    let output_color = reader.output_color_type();
    let size = if indexed {
        if output_color.0 != png::ColorType::Indexed || output_color.1 as u8 != format.depth {
            return Err(Malformed);
        }
        packed_row_bytes(canvas.width as usize, format.depth)?
            .checked_mul(canvas.height as usize)
            .ok_or(WorkingLimit)?
    } else {
        pixels
            .checked_mul(usize::from(png_color(output_color)?.bytes_per_pixel()))
            .ok_or(WorkingLimit)?
    };
    if reader.output_buffer_size() != Some(size) {
        return Err(Malformed);
    }
    let mut bytes = zeroed(size)?;
    let frame = reader.next_frame(&mut bytes).map_err(png_error)?;
    reader.finish().map_err(png_error)?;
    if (frame.width, frame.height) != (canvas.width, canvas.height)
        || (frame.color_type, frame.bit_depth) != output_color
        || frame.buffer_size() != size
    {
        return Err(Malformed);
    }
    drop(reader);

    let color = if indexed {
        let (expanded, color) = expand_palette(
            &bytes,
            canvas,
            format.depth,
            palette.ok_or(Malformed)?,
            transparency,
        )?;
        bytes = expanded;
        color
    } else {
        png_color(output_color)?
    };

    // png outputs 16-bit samples in network order. The image encoder accepts
    // native endian and converts them back when writing.
    if cfg!(target_endian = "little") && frame.bit_depth == png::BitDepth::Sixteen {
        for sample in bytes.chunks_exact_mut(2) {
            sample.swap(0, 1);
        }
    }
    Ok((
        Raster {
            canvas,
            color,
            bytes,
        },
        orientation,
    ))
}

struct PngFormat {
    color: u8,
    depth: u8,
    channels: usize,
    interlaced: bool,
}

impl PngFormat {
    fn from_header(header: &[u8]) -> Result<Self> {
        let (depth, color) = (header[16], header[17]);
        let channels = match (color, depth) {
            (0, 1 | 2 | 4 | 8 | 16) | (3, 1 | 2 | 4 | 8) => 1,
            (2, 8 | 16) => 3,
            (4, 8 | 16) => 2,
            (6, 8 | 16) => 4,
            _ => return Err(Malformed),
        };
        if header[18] != 0 || header[19] != 0 || header[20] > 1 {
            return Err(Malformed);
        }
        Ok(Self {
            color,
            depth,
            channels,
            interlaced: header[20] == 1,
        })
    }

    fn check_palette(&self, size: usize) -> Result<()> {
        let entries = size / 3;
        let maximum = if self.color == 3 {
            1 << self.depth
        } else {
            256
        };
        if matches!(self.color, 0 | 4) || size % 3 != 0 || entries == 0 || entries > maximum {
            return Err(Malformed);
        }
        Ok(())
    }

    fn check_transparency(&self, data: &[u8], palette: Option<usize>) -> Result<()> {
        if self.color == 3 {
            return if !data.is_empty() && palette.is_some_and(|entries| data.len() <= entries) {
                Ok(())
            } else {
                Err(Malformed)
            };
        }
        if !matches!(self.color, 0 | 2)
            || data.len() != self.channels * 2
            || !data.chunks_exact(2).all(|sample| {
                u32::from(u16::from_be_bytes([sample[0], sample[1]])) < 1 << self.depth
            })
        {
            return Err(Malformed);
        }
        Ok(())
    }

    fn filtered_bytes(&self, canvas: Canvas) -> Result<u64> {
        const ADAM7: [(u64, u64, u64, u64); 7] = [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ];
        let passes: &[_] = if self.interlaced {
            &ADAM7
        } else {
            &[(0, 0, 1, 1)]
        };
        let bits = self.channels as u64 * u64::from(self.depth);
        passes
            .iter()
            .try_fold(0u64, |total, &(x, y, step_x, step_y)| {
                let width = u64::from(canvas.width).saturating_sub(x).div_ceil(step_x);
                let height = u64::from(canvas.height).saturating_sub(y).div_ceil(step_y);
                if width == 0 || height == 0 {
                    return Ok(total);
                }
                // One filter byte precedes each packed row, separately per pass.
                let row = (width * bits).div_ceil(8) + 1;
                row.checked_mul(height)
                    .and_then(|size| total.checked_add(size))
                    .ok_or(WorkingLimit)
            })
    }
}

struct PngStream {
    inflater: Decompress,
    expected: u64,
    ended: bool,
}

impl PngStream {
    fn consume(&mut self, data: &[u8], scratch: &mut [u8]) -> Result<()> {
        let consumed = self.consume_prefix(data, scratch)?;
        (consumed == data.len()).then_some(()).ok_or(Malformed)
    }

    fn consume_prefix(&mut self, mut data: &[u8], scratch: &mut [u8]) -> Result<usize> {
        if self.ended {
            return Ok(0);
        }
        let input_len = data.len();
        loop {
            let input_before = self.inflater.total_in();
            let output_before = self.inflater.total_out();
            let status = self
                .inflater
                .decompress(data, scratch, FlushDecompress::None)
                .map_err(|_| Malformed)?;
            let consumed = (self.inflater.total_in() - input_before) as usize;
            data = &data[consumed..];
            let output = self.inflater.total_out();
            if output > self.expected {
                return Err(Malformed);
            }
            if status == Status::StreamEnd {
                if output != self.expected {
                    return Err(Malformed);
                }
                self.ended = true;
                return Ok(input_len - data.len());
            }
            if consumed == 0 && output == output_before {
                return if data.is_empty() {
                    Ok(input_len)
                } else {
                    Err(Malformed)
                };
            }
        }
    }
}

fn png_color((color, depth): (png::ColorType, png::BitDepth)) -> Result<ColorType> {
    use png::BitDepth::{Eight, Sixteen};
    use png::ColorType::{Grayscale, GrayscaleAlpha, Rgb, Rgba};

    match (color, depth) {
        (Grayscale, Eight) => Ok(ColorType::L8),
        (Grayscale, Sixteen) => Ok(ColorType::L16),
        (GrayscaleAlpha, Eight) => Ok(ColorType::La8),
        (GrayscaleAlpha, Sixteen) => Ok(ColorType::La16),
        (Rgb, Eight) => Ok(ColorType::Rgb8),
        (Rgb, Sixteen) => Ok(ColorType::Rgb16),
        (Rgba, Eight) => Ok(ColorType::Rgba8),
        (Rgba, Sixteen) => Ok(ColorType::Rgba16),
        _ => Err(Malformed),
    }
}

fn packed_row_bytes(width: usize, depth: u8) -> Result<usize> {
    width
        .checked_mul(usize::from(depth))
        .map(|bits| bits.div_ceil(8))
        .ok_or(WorkingLimit)
}

fn expand_palette(
    packed: &[u8],
    canvas: Canvas,
    depth: u8,
    palette: &[u8],
    transparency: Option<&[u8]>,
) -> Result<(Vec<u8>, ColorType)> {
    let color = if transparency.is_some() {
        ColorType::Rgba8
    } else {
        ColorType::Rgb8
    };
    let stride = usize::from(color.bytes_per_pixel());
    let width = canvas.width as usize;
    let src_row = packed_row_bytes(width, depth)?;
    let dst_row = width.checked_mul(stride).ok_or(WorkingLimit)?;
    let size = dst_row
        .checked_mul(canvas.height as usize)
        .ok_or(WorkingLimit)?;
    let mut bytes = zeroed(size)?;
    let mask = ((1u16 << depth) - 1) as u8;
    for (src, dst) in packed
        .chunks_exact(src_row)
        .zip(bytes.chunks_exact_mut(dst_row))
    {
        for (x, pixel) in dst.chunks_exact_mut(stride).enumerate() {
            let bit = x * usize::from(depth);
            let shift = 8 - depth - (bit % 8) as u8;
            let index = usize::from((src[bit / 8] >> shift) & mask);
            let start = index * 3;
            pixel[..3].copy_from_slice(palette.get(start..start + 3).ok_or(Malformed)?);
            if let Some(alpha) = transparency {
                pixel[3] = alpha.get(index).copied().unwrap_or(255);
            }
        }
    }
    Ok((bytes, color))
}

fn decode_jpeg(bytes: &[u8], limits: PassiveImageLimits) -> Result<(Raster, Orientation)> {
    let (clean, canvas, orientation) = preflight_jpeg(bytes, limits)?;
    let pixels = canvas.pixels(limits)?;
    canvas.oriented(orientation, limits)?;

    // JPEG coefficients can persist for the whole padded canvas, including for
    // sequential files with noninterleaved scans. Allow up to four components.
    let padded_width = (canvas.width as usize).next_multiple_of(32);
    let padded_height = (canvas.height as usize).next_multiple_of(32);
    remaining(
        limits,
        padded_width
            .checked_mul(padded_height)
            .and_then(|size| size.checked_mul(11))
            .and_then(|size| size.checked_add(padded_width.checked_mul(2048)?))
            .and_then(|size| size.checked_add(clean.capacity()))
            .and_then(|size| size.checked_add(CODEC_OVERHEAD)),
    )?;

    let options = DecoderOptions::default()
        .set_strict_mode(true)
        .set_max_width(canvas.width as usize)
        .set_max_height(canvas.height as usize)
        .jpeg_set_max_scans(JPEG_MAX_SCANS)
        .jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(clean.as_slice()), options);
    decoder.decode_headers().map_err(|_| Malformed)?;
    if decoder.dimensions() != Some((canvas.width as usize, canvas.height as usize))
        || decoder.output_colorspace() != Some(ColorSpace::RGB)
    {
        return Err(Malformed);
    }
    let mut bytes = zeroed(pixels.checked_mul(3).ok_or(WorkingLimit)?)?;
    decoder.decode_into(&mut bytes).map_err(|_| Malformed)?;
    Ok((
        Raster {
            canvas,
            color: ColorType::Rgb8,
            bytes,
        },
        orientation,
    ))
}

fn preflight_jpeg(
    bytes: &[u8],
    limits: PassiveImageLimits,
) -> Result<(Vec<u8>, Canvas, Orientation)> {
    let mut clean = sanitized_buffer(bytes.len(), limits)?;
    clean.extend_from_slice(JPEG_SIGNATURE);
    let mut rest = &bytes[2..];
    let mut frame: Option<JpegFrame> = None;
    let mut exif = None;
    let mut scans = 0;
    loop {
        let marker_prefix = rest.iter().take_while(|&&byte| byte == 0xff).count();
        if marker_prefix == 0 {
            return Err(Malformed);
        }
        let marker = *rest.get(marker_prefix).ok_or(Malformed)?;
        rest = &rest[marker_prefix + 1..];
        if marker == 0xd9 {
            if scans == 0 || !rest.is_empty() {
                return Err(Malformed);
            }
            let frame = frame.ok_or(Malformed)?;
            if frame
                .sequential_seen
                .is_some_and(|seen| seen != (1 << frame.components) - 1)
            {
                return Err(Malformed);
            }
            clean.extend_from_slice(&[0xff, marker]);
            return Ok((
                clean,
                frame.canvas,
                exif.unwrap_or(Orientation::NoTransforms),
            ));
        }

        let length = usize::from(u16::from_be_bytes(
            rest.get(..2)
                .ok_or(Malformed)?
                .try_into()
                .map_err(|_| Malformed)?,
        ));
        let segment = rest
            .get(..length)
            .filter(|segment| segment.len() >= 2)
            .ok_or(Malformed)?;
        let data = &segment[2..];
        rest = &rest[length..];
        match marker {
            0xc0..=0xc2 if frame.is_none() => {
                frame = Some(JpegFrame::parse(data, marker != 0xc2, limits)?);
            }
            0xda if scans < JPEG_MAX_SCANS => {
                frame.as_mut().ok_or(Malformed)?.add_scan(data)?;
                scans += 1;
            }
            0xc4 | 0xdb | 0xdd => (),
            // Preserve Adobe color interpretation, discard other APP data and comments.
            0xee if data.starts_with(b"Adobe") && data.len() == 12 => (),
            0xee if data.starts_with(b"Adobe") => return Err(Malformed),
            0xe1 if data.starts_with(b"Exif\0\0") => {
                record_exif(&mut exif, &data[6..])?;
                continue;
            }
            0xe2 if data.starts_with(b"MPF\0") => return Err(MultipleImages),
            0xe0..=0xef | 0xfe => continue,
            _ => return Err(Malformed),
        }
        clean.extend_from_slice(&[0xff, marker]);
        clean.extend_from_slice(segment);
        if marker == 0xda {
            let tail = jpeg_scan_tail(rest)?;
            if rest.len() == tail.len() {
                return Err(Malformed);
            }
            clean.extend_from_slice(&rest[..rest.len() - tail.len()]);
            rest = tail;
        }
    }
}

struct JpegFrame {
    canvas: Canvas,
    components: usize,
    ids: [u8; 4],
    sequential_seen: Option<u8>,
}

impl JpegFrame {
    fn parse(data: &[u8], sequential: bool, limits: PassiveImageLimits) -> Result<Self> {
        let &[precision, h1, h2, w1, w2, components, ref sampling @ ..] = data else {
            return Err(Malformed);
        };
        let components = usize::from(components);
        if precision != 8 || !matches!(components, 1 | 3 | 4) || sampling.len() != components * 3 {
            return Err(Malformed);
        }
        let mut ids = [0; 4];
        for (index, sample) in sampling.chunks_exact(3).enumerate() {
            if ids[..index].contains(&sample[0])
                || !matches!(sample[1] >> 4, 1 | 2 | 4)
                || !matches!(sample[1] & 15, 1 | 2 | 4)
            {
                return Err(Malformed);
            }
            ids[index] = sample[0];
        }
        let canvas = Canvas {
            width: u32::from(u16::from_be_bytes([w1, w2])),
            height: u32::from(u16::from_be_bytes([h1, h2])),
        };
        // The decoder rounds u16 dimensions upward for MCU calculations.
        if canvas.width > 65504 || canvas.height > 65504 {
            return Err(CanvasLimit);
        }
        canvas.pixels(limits)?;
        Ok(Self {
            canvas,
            components,
            ids,
            sequential_seen: sequential.then_some(0),
        })
    }

    fn add_scan(&mut self, data: &[u8]) -> Result<()> {
        let (&count, parameters) = data.split_first().ok_or(Malformed)?;
        let count = usize::from(count);
        if count == 0 || count > self.components || parameters.len() != count * 2 + 3 {
            return Err(Malformed);
        }
        let (selectors, spectral) = parameters.split_at(count * 2);
        let mut scan = 0;
        for selector in selectors.chunks_exact(2) {
            let index = self.ids[..self.components]
                .iter()
                .position(|id| *id == selector[0])
                .ok_or(Malformed)?;
            let bit = 1u8 << index;
            if scan & bit != 0 {
                return Err(Malformed);
            }
            scan |= bit;
        }
        if let Some(seen) = &mut self.sequential_seen {
            if *seen & scan != 0 || spectral != [0, 63, 0] {
                return Err(Malformed);
            }
            *seen |= scan;
        }
        Ok(())
    }
}

fn jpeg_scan_tail(mut bytes: &[u8]) -> Result<&[u8]> {
    loop {
        let start = bytes
            .iter()
            .position(|&byte| byte == 0xff)
            .ok_or(Malformed)?;
        let marker = &bytes[start..];
        let prefix = marker.iter().take_while(|&&byte| byte == 0xff).count();
        match marker.get(prefix).copied().ok_or(Malformed)? {
            0 if prefix != 1 => return Err(Malformed),
            0 | 0xd0..=0xd7 => bytes = &marker[prefix + 1..],
            _ => return Ok(marker),
        }
    }
}

fn record_exif(exif: &mut Option<Orientation>, tiff: &[u8]) -> Result<()> {
    if exif.is_some() {
        return Err(Malformed);
    }
    let little_endian = match tiff.get(..4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return Err(Malformed),
    };
    let read_short = |bytes: [u8; 2]| {
        if little_endian {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        }
    };
    let read_long = |bytes: [u8; 4]| {
        if little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    };
    let offset = read_long(
        tiff.get(4..8)
            .ok_or(Malformed)?
            .try_into()
            .map_err(|_| Malformed)?,
    ) as usize;
    if offset < 8 {
        return Err(Malformed);
    }
    let ifd = tiff.get(offset..).ok_or(Malformed)?;
    let entries = usize::from(read_short(
        ifd.get(..2)
            .ok_or(Malformed)?
            .try_into()
            .map_err(|_| Malformed)?,
    ));
    let end = 2 + 12 * entries;
    if ifd.len() < end + 4 {
        return Err(Malformed);
    }
    let mut found = None;
    for entry in ifd[2..end].chunks_exact(12) {
        if read_short([entry[0], entry[1]]) == 0x0112 {
            if found.is_some()
                || read_short([entry[2], entry[3]]) != 3
                || read_long([entry[4], entry[5], entry[6], entry[7]]) != 1
            {
                return Err(Malformed);
            }
            let value = u8::try_from(read_short([entry[8], entry[9]])).map_err(|_| Malformed)?;
            found = Some(Orientation::from_exif(value).ok_or(Malformed)?);
        }
    }
    *exif = Some(found.unwrap_or(Orientation::NoTransforms));
    Ok(())
}

fn encode_png(raster: Raster, limits: PassiveImageLimits) -> Result<CanonicalPng> {
    // Reserve for samples, 16-bit byte-order conversion, rows, and compressed
    // staging. image's encoder uses png's whole-frame writer.
    let working_output_cap = remaining(
        limits,
        raster
            .bytes
            .len()
            .checked_mul(8)
            .and_then(|size| size.checked_add(CODEC_OVERHEAD)),
    )?;
    let mut output = BoundedOutput {
        bytes: Vec::new(),
        cap: limits.max_output_bytes.min(working_output_cap),
        exhausted: false,
        allocation_failed: false,
    };
    let encoded =
        PngEncoder::new_with_quality(&mut output, CompressionType::Fast, FilterType::Adaptive)
            .write_image(
                &raster.bytes,
                raster.canvas.width,
                raster.canvas.height,
                raster.color.into(),
            );
    if output.exhausted {
        return Err(if limits.max_output_bytes <= working_output_cap {
            OutputLimit
        } else {
            WorkingLimit
        });
    }
    if output.allocation_failed {
        return Err(WorkingLimit);
    }
    encoded.map_err(|_| Encoding)?;
    // The underlying PNG writer may report a final write error only from Drop.
    if !output.bytes.ends_with(PNG_END) {
        return Err(Encoding);
    }
    Ok(CanonicalPng {
        width: raster.canvas.width,
        height: raster.canvas.height,
        bytes: output.bytes,
    })
}

struct BoundedOutput {
    bytes: Vec<u8>,
    cap: usize,
    exhausted: bool,
    allocation_failed: bool,
}

impl Write for BoundedOutput {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let length = self.bytes.len().checked_add(data.len());
        let Some(length) = length.filter(|&length| length <= self.cap) else {
            self.exhausted = true;
            return Err(io::Error::other("canonical image byte limit"));
        };
        if length > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(length)
                .min(self.cap);
            if let Err(error) = self.bytes.try_reserve_exact(capacity - self.bytes.len()) {
                self.allocation_failed = true;
                return Err(io::Error::other(error));
            }
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
