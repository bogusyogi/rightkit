//! Codec choice and the pure encoding helpers: even dimensions, 100 ns sample
//! timing, bitrate, and BGRA to NV12 conversion. Nothing here touches a window,
//! a device or an encoder, so every helper is unit tested directly.

pub use crate::backend::Codec;

/// Rounds a dimension down to the nearest even value. The encoder needs even
/// sizes for 4:2:0 chroma, so this crops at most one column or row on the
/// right or bottom edge. `None` when nothing usable remains.
pub fn even_dimension(value: u32) -> Option<u32> {
    let even = value & !1;
    (even >= 2).then_some(even)
}

/// Sample time in 100 ns units, relative to the first kept frame. `host_ns` is
/// the frame's SystemRelativeTime, already converted to nanoseconds (ticks * 100),
/// so the division is exact. Never uses wall clock or the frame index.
pub fn sample_time_100ns(host_ns: u64, first_host_ns: u64) -> i64 {
    (host_ns.saturating_sub(first_host_ns) / 100) as i64
}

/// Nominal sample duration in 100 ns units for the requested fps.
pub fn sample_duration_100ns(fps: u32) -> i64 {
    10_000_000 / i64::from(fps)
}

/// Average bitrate: about 0.1 bit per pixel per frame, clamped to 2 to 40 Mbit/s.
pub fn bitrate_bps(width: u32, height: u32, fps: u32) -> u32 {
    let bps = u64::from(width) * u64::from(height) * u64::from(fps) / 10;
    bps.clamp(2_000_000, 40_000_000) as u32
}

/// Byte length of one NV12 frame (full-resolution Y plane, then interleaved 2x2-subsampled UV).
pub fn nv12_len(width: usize, height: usize) -> usize {
    width * height * 3 / 2
}

/// Converts packed BGRA to NV12 (BT.709, limited range) over the top-left
/// `width` x `height` region. `src_stride` is the source row length in bytes.
/// Chroma is the average of each 2x2 block. Width and height must be even.
pub fn bgra_to_nv12(
    src: &[u8],
    src_stride: usize,
    width: usize,
    height: usize,
    dst: &mut [u8],
) -> Result<(), &'static str> {
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err("NV12 needs non-zero, even dimensions");
    }
    if src_stride < width * 4 || src.len() < src_stride * height {
        return Err("source buffer is smaller than the requested frame");
    }
    let len = nv12_len(width, height);
    if dst.len() < len {
        return Err("NV12 destination is too small");
    }
    let luma_len = width * height;
    let (luma, chroma) = dst[..len].split_at_mut(luma_len);

    for row in 0..height {
        let src_row = &src[row * src_stride..row * src_stride + width * 4];
        let out = &mut luma[row * width..(row + 1) * width];
        let (pixels, _) = src_row.as_chunks::<4>();
        for (px, o) in pixels.iter().zip(out.iter_mut()) {
            *o = luma_of(i32::from(px[2]), i32::from(px[1]), i32::from(px[0]));
        }
    }

    for cy in 0..height / 2 {
        let top = &src[2 * cy * src_stride..];
        let bottom = &src[(2 * cy + 1) * src_stride..];
        for cx in 0..width / 2 {
            // Sums of B, G, R over the 2x2 block, then the rounded average.
            let mut sum = [0i32; 3];
            for line in [top, bottom] {
                for dx in 0..2 {
                    let at = (2 * cx + dx) * 4;
                    sum[0] += i32::from(line[at]);
                    sum[1] += i32::from(line[at + 1]);
                    sum[2] += i32::from(line[at + 2]);
                }
            }
            let b = (sum[0] + 2) >> 2;
            let g = (sum[1] + 2) >> 2;
            let r = (sum[2] + 2) >> 2;
            let at = cy * width + cx * 2;
            chroma[at] = chroma_u(r, g, b);
            chroma[at + 1] = chroma_v(r, g, b);
        }
    }
    Ok(())
}

fn luma_of(r: i32, g: i32, b: i32) -> u8 {
    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8
}

fn chroma_u(r: i32, g: i32, b: i32) -> u8 {
    (((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128) as u8
}

fn chroma_v(r: i32, g: i32, b: i32) -> u8 {
    (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128) as u8
}
