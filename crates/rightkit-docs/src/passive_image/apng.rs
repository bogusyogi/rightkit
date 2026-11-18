use std::io::{BufRead, Seek};
use std::ops::Range;

use super::{
    catch_unwind, next_png_chunk, png_error, png_header, record_exif, remaining, sanitized_buffer,
    zeroed, BoundedOutput, Canvas, Cursor, Decompress, Encoding, InputLimit, Malformed,
    MediaTypeMismatch, Orientation, OutputLimit, PassiveImageLimits, PngChunk, PngFormat,
    PngStream, Result, UnsupportedMediaType, WorkingLimit, CODEC_OVERHEAD, PNG_SIGNATURE,
};

const PNG_MAX_INTEGER: u32 = 0x7fff_ffff;

/// Per-call limits for an animated PNG preview.
#[derive(Debug, Clone, Copy)]
pub struct ApngPreviewLimits {
    pub image: PassiveImageLimits,
    /// Maximum number of animation frames. A separate default image is excluded.
    pub max_frames: u32,
    /// Maximum canvas pixels times the number of decoded images, including a separate poster.
    pub max_total_canvas_pixels: u64,
}

/// Freshly encoded animated PNG bytes.
#[derive(Debug)]
pub struct CanonicalApng {
    pub width: u32,
    pub height: u32,
    pub frame_count: u32,
    /// Total plays. Zero means indefinite playback.
    pub num_plays: u32,
    pub has_separate_poster: bool,
    pub bytes: Vec<u8>,
}

impl CanonicalApng {
    pub const MIME: &str = "image/png";
}

/// Re-encode one APNG with its raw frame samples, display controls and optional separate poster.
/// Accepts `image/png` and `image/apng`, case-insensitively, with optional parameters.
/// Normalizes unused color-key bits and discards padding after zlib in the final IDAT chunk.
/// Drops source metadata other than palette and transparency, and rejects EXIF orientation
/// that would require changing frame geometry. Send only the returned bytes to a renderer.
pub fn canonicalize_apng(
    bytes: &[u8],
    declared_mime: &str,
    limits: ApngPreviewLimits,
) -> Result<CanonicalApng> {
    if bytes.len() > limits.image.max_input_bytes {
        return Err(InputLimit);
    }
    let mime = declared_mime.split(';').next().unwrap_or_default().trim();
    if !mime.eq_ignore_ascii_case("image/png") && !mime.eq_ignore_ascii_case("image/apng") {
        return Err(UnsupportedMediaType);
    }
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err(MediaTypeMismatch);
    }
    catch_unwind(|| {
        let (header, canvas) = png_header(bytes)?;
        let pixels = canvas.pixels(limits.image)?;
        let format = PngFormat::from_header(header)?;
        let validated = validate(bytes, canvas, pixels as u64, &format, limits)?;
        encode(validated, canvas, &format, limits.image)
    })
    .map_err(|_| Malformed)?
}

struct Validated<'a> {
    clean: Vec<u8>,
    animation: png::AnimationControl,
    has_separate_poster: bool,
    palette: Option<&'a [u8]>,
    transparency: Option<Range<usize>>,
    decoder_rows: usize,
}

#[derive(Default)]
struct StreamWork {
    filtered_total: u64,
    decoder_rows: usize,
}

fn validate<'a>(
    bytes: &'a [u8],
    canvas: Canvas,
    pixels: u64,
    format: &PngFormat,
    limits: ApngPreviewLimits,
) -> Result<Validated<'a>> {
    remaining(limits.image, bytes.len().checked_add(CODEC_OVERHEAD))?;
    let mut clean = sanitized_buffer(bytes.len(), limits.image)?;
    clean.extend_from_slice(PNG_SIGNATURE);
    let mut chunks = &bytes[PNG_SIGNATURE.len()..];
    let header = next_png_chunk(&mut chunks)?;
    clean.extend_from_slice(header.bytes);

    let mut animation = None;
    let mut palette: Option<&[u8]> = None;
    let mut transparency = None;
    let mut orientation = None;
    let mut in_idat = false;
    let mut idat_closed = false;
    let mut separate_poster = false;
    let mut frames = 0;
    let mut sequence = 0;
    let mut stream = None;
    let mut work = StreamWork::default();
    let mut scratch = [0; 8192];

    loop {
        let PngChunk {
            kind,
            data,
            bytes: chunk,
        } = next_png_chunk(&mut chunks)?;
        if in_idat && kind != b"IDAT" {
            in_idat = false;
            idat_closed = true;
        }
        match kind {
            b"acTL" if animation.is_none() && !idat_closed && data.len() == 8 => {
                let num_frames = png_integer(&data[..4])?;
                let num_plays = png_integer(&data[4..])?;
                if num_frames == 0 {
                    return Err(Malformed);
                }
                if num_frames > limits.max_frames {
                    return Err(WorkingLimit);
                }
                animation = Some(png::AnimationControl {
                    num_frames,
                    num_plays,
                });
            }
            b"PLTE" if !idat_closed && transparency.is_none() && palette.is_none() => {
                format.check_palette(data.len())?;
                palette = Some(data);
            }
            b"tRNS" if !idat_closed && transparency.is_none() => {
                let mut color_key = [0; 6];
                let data = normalize_transparency(
                    data,
                    palette.map(|palette| palette.len() / 3),
                    format,
                    &mut color_key,
                )?;
                let start = clean.len() + 8;
                append_chunk(&mut clean, b"tRNS", data);
                transparency = Some(start..start + data.len());
                continue;
            }
            b"eXIf" if !idat_closed => {
                record_exif(&mut orientation, data)?;
                if orientation != Some(Orientation::NoTransforms) {
                    return Err(Malformed);
                }
                continue;
            }
            b"fcTL" if data.len() == 26 => {
                check_sequence(&mut sequence, &data[..4])?;
                let frame = frame_canvas(data, canvas, !idat_closed)?;
                frames += 1;
                if idat_closed {
                    let control = animation.ok_or(Malformed)?;
                    if frames > control.num_frames {
                        return Err(Malformed);
                    }
                    finish_stream(stream.take(), &mut scratch)?;
                    stream = Some(start_stream(format, frame, &mut work)?);
                } else if frames != 1 {
                    return Err(Malformed);
                }
            }
            b"IDAT" if !idat_closed => {
                if !in_idat {
                    let control = animation.ok_or(Malformed)?;
                    if format.color == 3 && palette.is_none() {
                        return Err(Malformed);
                    }
                    separate_poster = frames == 0;
                    let images = u64::from(control.num_frames) + u64::from(separate_poster);
                    let total_pixels = pixels.checked_mul(images).ok_or(WorkingLimit)?;
                    if total_pixels > limits.max_total_canvas_pixels {
                        return Err(WorkingLimit);
                    }
                    stream = Some(start_stream(format, canvas, &mut work)?);
                    in_idat = true;
                }
                let consumed = stream
                    .as_mut()
                    .ok_or(Malformed)?
                    .consume_prefix(data, &mut scratch)?;
                if consumed != data.len() {
                    // The allowance applies only to the final IDAT, even if the next is empty.
                    if matches!(chunks.get(4..8), Some(b"IDAT")) {
                        return Err(Malformed);
                    }
                    append_chunk(&mut clean, b"IDAT", &data[..consumed]);
                    continue;
                }
            }
            b"fdAT" if idat_closed && data.len() >= 4 => {
                if frames <= u32::from(!separate_poster) {
                    return Err(Malformed);
                }
                check_sequence(&mut sequence, &data[..4])?;
                stream
                    .as_mut()
                    .ok_or(Malformed)?
                    .consume(&data[4..], &mut scratch)?;
            }
            b"IEND" if idat_closed && data.is_empty() && chunks.is_empty() => {
                let animation = animation.ok_or(Malformed)?;
                if frames != animation.num_frames {
                    return Err(Malformed);
                }
                finish_stream(stream, &mut scratch)?;
                clean.extend_from_slice(chunk);
                return Ok(Validated {
                    clean,
                    animation,
                    has_separate_poster: separate_poster,
                    palette,
                    transparency,
                    decoder_rows: work.decoder_rows,
                });
            }
            b"acTL" | b"fcTL" | b"fdAT" | b"tRNS" | b"eXIf" => return Err(Malformed),
            _ if kind[0].is_ascii_uppercase() => return Err(Malformed),
            _ => continue,
        }
        clean.extend_from_slice(chunk);
    }
}

fn normalize_transparency<'a>(
    data: &'a [u8],
    palette_entries: Option<usize>,
    format: &PngFormat,
    color_key: &'a mut [u8; 6],
) -> Result<&'a [u8]> {
    let data: &[u8] = if matches!(format.color, 0 | 2) && format.depth < 16 {
        let normalized = color_key.get_mut(..data.len()).ok_or(Malformed)?;
        normalized.copy_from_slice(data);
        let mask = (1u16 << format.depth) - 1;
        for sample in normalized.chunks_exact_mut(2) {
            let value = u16::from_be_bytes([sample[0], sample[1]]) & mask;
            sample.copy_from_slice(&value.to_be_bytes());
        }
        normalized
    } else {
        data
    };
    format.check_transparency(data, palette_entries)?;
    Ok(data)
}

fn append_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = output.len();
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let crc = crc32fast::hash(&output[start..]);
    output.extend_from_slice(&crc.to_be_bytes());
}

fn png_integer(bytes: &[u8]) -> Result<u32> {
    let value = u32::from_be_bytes(bytes.try_into().map_err(|_| Malformed)?);
    (value <= PNG_MAX_INTEGER).then_some(value).ok_or(Malformed)
}

fn check_sequence(sequence: &mut u32, bytes: &[u8]) -> Result<()> {
    if png_integer(bytes)? != *sequence {
        return Err(Malformed);
    }
    *sequence += 1;
    Ok(())
}

fn frame_canvas(data: &[u8], canvas: Canvas, first_idat_frame: bool) -> Result<Canvas> {
    let width = png_integer(&data[4..8])?;
    let height = png_integer(&data[8..12])?;
    let x = png_integer(&data[12..16])?;
    let y = png_integer(&data[16..20])?;
    let frame = Canvas { width, height };
    if width == 0
        || height == 0
        || width + x > canvas.width
        || height + y > canvas.height
        || (first_idat_frame && frame != canvas)
        || png::DisposeOp::from_u8(data[24]).is_none()
        || png::BlendOp::from_u8(data[25]).is_none()
    {
        return Err(Malformed);
    }
    Ok(frame)
}

fn start_stream(format: &PngFormat, canvas: Canvas, work: &mut StreamWork) -> Result<PngStream> {
    let expected = format.filtered_bytes(canvas)?;
    work.filtered_total = work
        .filtered_total
        .checked_add(expected)
        .ok_or(WorkingLimit)?;
    let bits = format.channels * format.depth as usize;
    work.decoder_rows = work
        .decoder_rows
        .checked_add(row_bytes(canvas.width, bits)?)
        .ok_or(WorkingLimit)?;
    Ok(PngStream {
        inflater: Decompress::new(true),
        expected,
        ended: false,
    })
}

fn finish_stream(stream: Option<PngStream>, scratch: &mut [u8]) -> Result<()> {
    let mut stream = stream.ok_or(Malformed)?;
    stream.consume(&[], scratch)?;
    stream.ended.then_some(()).ok_or(Malformed)
}

fn row_bytes(width: u32, bits: usize) -> Result<usize> {
    (width as usize)
        .checked_mul(bits)
        .map(|bits| bits.div_ceil(8))
        .ok_or(WorkingLimit)
}

fn encode(
    validated: Validated<'_>,
    canvas: Canvas,
    format: &PngFormat,
    limits: PassiveImageLimits,
) -> Result<CanonicalApng> {
    let Validated {
        clean,
        animation,
        has_separate_poster,
        palette,
        transparency,
        decoder_rows,
    } = validated;
    let color = png::ColorType::from_u8(format.color).ok_or(Malformed)?;
    let depth = png::BitDepth::from_u8(format.depth).ok_or(Malformed)?;
    let bits = format.channels * usize::from(format.depth);
    let stride = row_bytes(canvas.width, bits)?;
    let frame_bytes = stride
        .checked_mul(canvas.height as usize)
        .ok_or(WorkingLimit)?;
    let filtered_bytes = frame_bytes
        .checked_add(canvas.height as usize)
        .ok_or(WorkingLimit)?;
    // The pinned decoder charges one row per frame to its cumulative limit,
    // even though each frame reuses its row buffers.
    let decoder_reserve = stride
        .checked_mul(8)
        .and_then(|size| size.checked_add(CODEC_OVERHEAD))
        .ok_or(WorkingLimit)?;
    let decoder_bytes = decoder_reserve
        .checked_add(decoder_rows)
        .ok_or(WorkingLimit)?;
    // The pinned writer stages one compressed frame and an fdAT copy.
    // Reserve for both, vector growth, compression fallback and scanline filtering.
    let working_output_cap = remaining(
        limits,
        filtered_bytes
            .checked_mul(8)
            .and_then(|size| size.checked_add(frame_bytes))
            .and_then(|size| size.checked_add(clean.capacity()))
            .and_then(|size| size.checked_add(decoder_reserve))
            .and_then(|size| size.checked_add(CODEC_OVERHEAD)),
    )?;
    let mut output = BoundedOutput {
        bytes: Vec::new(),
        cap: limits.max_output_bytes.min(working_output_cap),
        exhausted: false,
        allocation_failed: false,
    };
    let mut options = png::DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    let mut decoder = png::Decoder::new_with_options(Cursor::new(clean.as_slice()), options);
    decoder.set_limits(png::Limits {
        bytes: decoder_bytes,
    });
    let mut reader = decoder.read_info().map_err(png_error)?;
    let decoded_animation = reader.info().animation_control.ok_or(Malformed)?;
    if reader.output_color_type() != (color, depth)
        || decoded_animation.num_frames != animation.num_frames
        || decoded_animation.num_plays != animation.num_plays
        || reader.info().frame_control.is_none() != has_separate_poster
    {
        return Err(Malformed);
    }

    let result = (|| {
        let mut encoder = png::Encoder::new(&mut output, canvas.width, canvas.height);
        encoder
            .set_animated(animation.num_frames, animation.num_plays)
            .map_err(|_| Encoding)?;
        encoder
            .set_sep_def_img(has_separate_poster)
            .map_err(|_| Encoding)?;
        encoder.validate_sequence(true);
        encoder.set_color(color);
        encoder.set_depth(depth);
        encoder.set_compression(png::Compression::Fast);
        if let Some(palette) = palette.filter(|_| color == png::ColorType::Indexed) {
            encoder.set_palette(palette);
        }
        if let Some(transparency) = transparency {
            encoder.set_trns(&clean[transparency]);
        }
        let mut writer = encoder.write_header().map_err(|_| Encoding)?;
        let mut raw = zeroed(frame_bytes)?;
        let images = animation.num_frames + u32::from(has_separate_poster);
        for index in 0..images {
            let frame = if index == 0 {
                reader.info().frame_control
            } else {
                Some(*reader.next_frame_info().map_err(png_error)?)
            };
            let frame = if let Some(frame) = frame {
                writer.reset_frame_position().map_err(|_| Encoding)?;
                writer
                    .set_frame_dimension(frame.width, frame.height)
                    .map_err(|_| Encoding)?;
                writer
                    .set_frame_position(frame.x_offset, frame.y_offset)
                    .map_err(|_| Encoding)?;
                writer
                    .set_frame_delay(frame.delay_num, frame.delay_den)
                    .map_err(|_| Encoding)?;
                writer
                    .set_dispose_op(frame.dispose_op)
                    .map_err(|_| Encoding)?;
                writer.set_blend_op(frame.blend_op).map_err(|_| Encoding)?;
                Canvas {
                    width: frame.width,
                    height: frame.height,
                }
            } else {
                canvas
            };
            let stride = reader.output_line_size(frame.width).ok_or(WorkingLimit)?;
            let len = stride
                .checked_mul(frame.height as usize)
                .ok_or(WorkingLimit)?;
            let raw = raw.get_mut(..len).ok_or(Malformed)?;
            decode_frame(&mut reader, raw, stride, bits as u8)?;
            clear_row_padding(raw, stride, frame.width as usize, bits);
            if let Some(palette) = palette.filter(|_| color == png::ColorType::Indexed) {
                check_indices(
                    raw,
                    stride,
                    frame.width as usize,
                    format.depth,
                    palette.len() / 3,
                )?;
            }
            writer.write_image_data(raw).map_err(|_| Encoding)?;
        }
        reader.finish().map_err(png_error)?;
        writer.finish().map_err(|_| Encoding)
    })();

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
    result?;
    Ok(CanonicalApng {
        width: canvas.width,
        height: canvas.height,
        frame_count: animation.num_frames,
        num_plays: animation.num_plays,
        has_separate_poster,
        bytes: output.bytes,
    })
}

fn decode_frame(
    reader: &mut png::Reader<impl BufRead + Seek>,
    raw: &mut [u8],
    stride: usize,
    bits: u8,
) -> Result<()> {
    // Adam7 writes parts of each destination byte, so clear pixels from the previous frame.
    let interlaced = reader.info().interlaced;
    if interlaced {
        raw.fill(0);
    }
    let mut next = 0;
    while let Some(row) = reader.next_interlaced_row().map_err(png_error)? {
        match row.interlace() {
            png::InterlaceInfo::Adam7(info) => {
                png::expand_interlaced_row(raw, stride, row.data(), info, bits);
            }
            png::InterlaceInfo::Null(_) => {
                raw.get_mut(next..next + stride)
                    .ok_or(Malformed)?
                    .copy_from_slice(row.data());
                next += stride;
            }
        }
    }
    if !interlaced && next != raw.len() {
        return Err(Malformed);
    }
    Ok(())
}

fn check_indices(raw: &[u8], stride: usize, width: usize, depth: u8, entries: usize) -> Result<()> {
    let depth = usize::from(depth);
    let mask = (1u16 << depth) - 1;
    for row in raw.chunks_exact(stride) {
        for x in 0..width {
            let bit = x * depth;
            let index = (u16::from(row[bit / 8]) >> (8 - depth - bit % 8)) & mask;
            if usize::from(index) >= entries {
                return Err(Malformed);
            }
        }
    }
    Ok(())
}

fn clear_row_padding(raw: &mut [u8], stride: usize, width: usize, bits: usize) {
    let used = width * bits % 8;
    if used != 0 {
        for row in raw.chunks_exact_mut(stride) {
            row[stride - 1] &= 0xff << (8 - used);
        }
    }
}
