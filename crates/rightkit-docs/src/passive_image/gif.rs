use std::io::Write;

use weezl::{decode::Decoder, encode::Encoder, BitOrder, LzwStatus};

use super::{
    catch_unwind, remaining, zeroed, BoundedOutput, Canvas, Encoding, InputLimit, Malformed,
    MediaTypeMismatch, OutputLimit, PassiveImageLimits, Result, UnsupportedMediaType, WorkingLimit,
};

// Covers weezl's bounded 12-bit dictionaries, including encoder table growth.
const GIF_CODEC_OVERHEAD: usize = 512 * 1024;

/// Per-call limits for a GIF preview.
#[derive(Debug, Clone, Copy)]
pub struct GifPreviewLimits {
    pub image: PassiveImageLimits,
    /// Maximum number of image frames.
    pub max_frames: u32,
    /// Maximum canvas pixels times the number of frames.
    pub max_total_canvas_pixels: u64,
}

/// Freshly encoded GIF bytes.
#[derive(Debug)]
pub struct CanonicalGif {
    pub width: u32,
    pub height: u32,
    pub frame_count: u32,
    /// Repetitions after the first play. Zero means indefinite playback.
    /// `None` means the source has no loop extension.
    pub loop_count: Option<u16>,
    pub bytes: Vec<u8>,
}

impl CanonicalGif {
    pub const MIME: &str = "image/gif";
}

/// Re-encode indexed GIF frames with their palettes, geometry and playback controls.
/// Accepts `image/gif`, case-insensitively, with optional parameters.
/// Keeps loop count and drops buffering hints, comments, other application metadata,
/// and padding after LZW end codes.
/// Rejects plain-text graphics, unknown graphics/control extensions and incomplete frames.
/// Send only the returned bytes to a renderer.
pub fn canonicalize_gif(
    bytes: &[u8],
    declared_mime: &str,
    limits: GifPreviewLimits,
) -> Result<CanonicalGif> {
    if bytes.len() > limits.image.max_input_bytes {
        return Err(InputLimit);
    }
    let mime = declared_mime.split(';').next().unwrap_or_default().trim();
    if !mime.eq_ignore_ascii_case(CanonicalGif::MIME) {
        return Err(UnsupportedMediaType);
    }
    if !bytes.starts_with(b"GIF87a") && !bytes.starts_with(b"GIF89a") {
        return Err(MediaTypeMismatch);
    }
    catch_unwind(|| transcode(&bytes[6..], limits)).map_err(|_| Malformed)?
}

fn transcode(mut input: &[u8], limits: GifPreviewLimits) -> Result<CanonicalGif> {
    let screen = take(&mut input, 7)?;
    let width = read_u16(screen, 0);
    let height = read_u16(screen, 2);
    let canvas = Canvas {
        width: u32::from(width),
        height: u32::from(height),
    };
    let canvas_pixels = canvas.pixels(limits.image)?;
    let global_palette = palette(&mut input, screen[4])?;
    let background = if global_palette.is_empty() {
        0
    } else {
        screen[5]
    };
    if !global_palette.is_empty() && usize::from(background) >= global_palette.len() / 3 {
        return Err(Malformed);
    }

    // One full indexed frame plus a sentinel sample for detecting excess decoded pixels.
    let working_cap = remaining(
        limits.image,
        canvas_pixels.checked_add(1 + GIF_CODEC_OVERHEAD),
    )?;
    let mut output = BoundedOutput {
        bytes: Vec::new(),
        cap: working_cap.min(limits.image.max_output_bytes),
        exhausted: false,
        allocation_failed: false,
    };
    let result = (|| {
        write(&mut output, b"GIF89a")?;
        write(&mut output, &width.to_le_bytes())?;
        write(&mut output, &height.to_le_bytes())?;
        write(&mut output, &[screen[4], background, screen[6]])?;
        write(&mut output, global_palette)?;
        transcode_blocks(
            &mut input,
            &mut output,
            width,
            height,
            global_palette,
            limits,
        )
    })();
    if output.allocation_failed || (output.exhausted && working_cap < limits.image.max_output_bytes)
    {
        return Err(WorkingLimit);
    }
    if output.exhausted {
        return Err(OutputLimit);
    }
    let (frame_count, loop_count) = result?;
    Ok(CanonicalGif {
        width: canvas.width,
        height: canvas.height,
        frame_count,
        loop_count,
        bytes: output.bytes,
    })
}

fn transcode_blocks(
    input: &mut &[u8],
    output: &mut BoundedOutput,
    width: u16,
    height: u16,
    global_palette: &[u8],
    limits: GifPreviewLimits,
) -> Result<(u32, Option<u16>)> {
    let mut control = None;
    let mut loop_count = None;
    let mut frames = 0u32;
    let canvas_pixels = u64::from(width) * u64::from(height);
    loop {
        match take(input, 1)?[0] {
            0x21 => match take(input, 1)?[0] {
                0xf9 if control.is_none() => control = Some(read_control(input)?),
                0xff => {
                    if let Some((identifier, count)) = read_application(input)? {
                        if loop_count.replace(count).is_some() {
                            return Err(Malformed);
                        }
                        write(output, &[0x21, 0xff, 11])?;
                        write(output, identifier)?;
                        write(output, &[3, 1])?;
                        write(output, &count.to_le_bytes())?;
                        write(output, &[0])?;
                    }
                }
                // Comments and unknown special-purpose extensions do not scope a GCE.
                0xfa..=0xfe => skip_sub_blocks(input)?,
                // Plain-text graphics and unknown control/graphic extensions may affect playback.
                _ => return Err(Malformed),
            },
            0x2c => {
                frames = frames.checked_add(1).ok_or(WorkingLimit)?;
                if frames > limits.max_frames
                    || u64::from(frames) > limits.max_total_canvas_pixels / canvas_pixels
                {
                    return Err(WorkingLimit);
                }
                transcode_frame(input, output, width, height, global_palette, control.take())?;
            }
            0x3b if frames != 0 && control.is_none() && input.is_empty() => {
                write(output, &[0x3b])?;
                return Ok((frames, loop_count));
            }
            _ => return Err(Malformed),
        }
    }
}

struct GraphicControl {
    disposal: u8,
    waits_for_input: bool,
    delay: u16,
    transparent: Option<u8>,
}

fn read_control(input: &mut &[u8]) -> Result<GraphicControl> {
    let block = sub_block(input)?;
    if block.len() != 4 || !sub_block(input)?.is_empty() {
        return Err(Malformed);
    }
    let flags = block[0];
    if flags >= 0x10 {
        return Err(Malformed);
    }
    Ok(GraphicControl {
        disposal: flags >> 2,
        waits_for_input: flags & 2 != 0,
        delay: read_u16(block, 1),
        transparent: (flags & 1 != 0).then_some(block[3]),
    })
}

fn read_application(input: &mut &[u8]) -> Result<Option<(&'static [u8; 11], u16)>> {
    let identifier = match sub_block(input)? {
        b"NETSCAPE2.0" => b"NETSCAPE2.0",
        b"ANIMEXTS1.0" => b"ANIMEXTS1.0",
        block if block.len() == 11 => {
            skip_sub_blocks(input)?;
            return Ok(None);
        }
        _ => return Err(Malformed),
    };
    let mut repeat = None;
    loop {
        match sub_block(input)? {
            [] => return Ok(repeat.map(|count| (identifier, count))),
            [code, low, high, ..] => match *code & 7 {
                1 => repeat = Some(u16::from_le_bytes([*low, *high])),
                2 => {} // Buffering hints do not affect playback once all bytes are available.
                _ => return Err(Malformed),
            },
            _ => return Err(Malformed),
        }
    }
}

fn transcode_frame(
    input: &mut &[u8],
    output: &mut BoundedOutput,
    width: u16,
    height: u16,
    global_palette: &[u8],
    control: Option<GraphicControl>,
) -> Result<()> {
    let descriptor = take(input, 9)?;
    let left = read_u16(descriptor, 0);
    let top = read_u16(descriptor, 2);
    let frame_width = read_u16(descriptor, 4);
    let frame_height = read_u16(descriptor, 6);
    let flags = descriptor[8];
    if frame_width == 0
        || frame_height == 0
        || width
            .checked_sub(frame_width)
            .is_none_or(|space| left > space)
        || height
            .checked_sub(frame_height)
            .is_none_or(|space| top > space)
        || flags & 0x18 != 0
    {
        return Err(Malformed);
    }
    let local_palette = palette(input, flags)?;
    let active_palette = if local_palette.is_empty() {
        global_palette
    } else {
        local_palette
    };
    let colors = active_palette.len() / 3;
    if colors == 0 {
        return Err(Malformed);
    }

    if let Some(control) = control {
        if control
            .transparent
            .is_some_and(|index| usize::from(index) >= colors)
        {
            return Err(Malformed);
        }
        let flags = (control.disposal << 2)
            | (u8::from(control.waits_for_input) << 1)
            | u8::from(control.transparent.is_some());
        write(output, &[0x21, 0xf9, 4, flags])?;
        write(output, &control.delay.to_le_bytes())?;
        write(output, &[control.transparent.unwrap_or_default(), 0])?;
    }

    // Keep interlaced indices in source scan order and preserve the interlace flag.
    let pixels = usize::from(frame_width) * usize::from(frame_height);
    let mut indices = zeroed(pixels + 1)?;
    decode_indices(input, &mut indices)?;
    if indices[..pixels]
        .iter()
        .any(|&index| usize::from(index) >= colors)
    {
        return Err(Malformed);
    }

    write(output, &[0x2c])?;
    write(output, &left.to_le_bytes())?;
    write(output, &top.to_le_bytes())?;
    write(output, &frame_width.to_le_bytes())?;
    write(output, &frame_height.to_le_bytes())?;
    write(output, &[flags])?;
    write(output, local_palette)?;
    encode_indices(output, &indices[..pixels], colors)
}

fn decode_indices(input: &mut &[u8], mut indices: &mut [u8]) -> Result<()> {
    let code_size = take(input, 1)?[0];
    if !(2..=8).contains(&code_size) {
        return Err(Malformed);
    }
    let mut decoder = Decoder::new(BitOrder::Lsb, code_size);
    let mut block: &[u8] = &[];
    let mut at_terminator = false;
    let mut buffered_steps = 0;
    loop {
        if block.is_empty() && !at_terminator {
            block = sub_block(input)?;
            at_terminator = block.is_empty();
        }
        let decoded = decoder.decode_bytes(block, indices);
        block = &block[decoded.consumed_in..];
        indices = &mut indices[decoded.consumed_out..];
        match decoded.status.map_err(|_| Malformed)? {
            LzwStatus::Ok | LzwStatus::NoProgress if !indices.is_empty() => {
                if decoded.consumed_in != 0 || decoded.consumed_out != 0 {
                    buffered_steps = 0;
                } else {
                    // A buffered clear code can advance the decoder without moving either
                    // slice. At most 21 three-bit codes fit in its 64-bit buffer.
                    buffered_steps += 1;
                    if buffered_steps > 64 / 3 {
                        return Err(Malformed);
                    }
                }
            }
            LzwStatus::Done if indices.len() == 1 => {
                if !at_terminator {
                    skip_sub_blocks(input)?;
                }
                return Ok(());
            }
            _ => return Err(Malformed),
        }
    }
}

fn encode_indices(output: &mut BoundedOutput, mut indices: &[u8], colors: usize) -> Result<()> {
    let code_size = colors.max(4).next_power_of_two().trailing_zeros() as u8;
    write(output, &[code_size])?;
    let mut encoder = Encoder::new(BitOrder::Lsb, code_size);
    encoder.finish();
    let mut block = [0; 255];
    loop {
        let encoded = encoder.encode_bytes(indices, &mut block);
        indices = &indices[encoded.consumed_in..];
        if encoded.consumed_out != 0 {
            write(output, &[encoded.consumed_out as u8])?;
            write(output, &block[..encoded.consumed_out])?;
        }
        match encoded.status.map_err(|_| Encoding)? {
            LzwStatus::Ok => {}
            LzwStatus::Done if indices.is_empty() => return write(output, &[0]),
            _ => return Err(Encoding),
        }
    }
}

fn write(output: &mut BoundedOutput, bytes: &[u8]) -> Result<()> {
    output.write_all(bytes).map_err(|_| Encoding)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn take<'a>(input: &mut &'a [u8], length: usize) -> Result<&'a [u8]> {
    let bytes = *input;
    let (head, tail) = bytes.split_at_checked(length).ok_or(Malformed)?;
    *input = tail;
    Ok(head)
}

fn palette<'a>(input: &mut &'a [u8], flags: u8) -> Result<&'a [u8]> {
    if flags & 0x80 == 0 {
        return Ok(&[]);
    }
    take(input, 3 * (2 << (flags & 7)))
}

fn sub_block<'a>(input: &mut &'a [u8]) -> Result<&'a [u8]> {
    let length = usize::from(take(input, 1)?[0]);
    take(input, length)
}

fn skip_sub_blocks(input: &mut &[u8]) -> Result<()> {
    while !sub_block(input)?.is_empty() {}
    Ok(())
}
