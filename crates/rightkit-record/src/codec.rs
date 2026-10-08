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

#[cfg(test)]
mod tests {
    use super::*;

    /// Packed BGRA image of one colour; `stride_px` may exceed `width`.
    fn solid(width: usize, height: usize, stride_px: usize, rgb: [u8; 3]) -> Vec<u8> {
        let mut out = vec![0u8; stride_px * 4 * height];
        for y in 0..height {
            for x in 0..width {
                let at = (y * stride_px + x) * 4;
                out[at] = rgb[2];
                out[at + 1] = rgb[1];
                out[at + 2] = rgb[0];
                out[at + 3] = 255;
            }
        }
        out
    }

    #[test]
    fn codec_names_round_trip_and_reject_unknown() {
        for codec in [Codec::H264, Codec::Hevc] {
            assert_eq!(Codec::parse(codec.as_str()), Some(codec));
        }
        assert_eq!(Codec::parse("h265"), None);
        assert_eq!(Codec::parse("bmp"), None, "the bmp debug path is gone");
        assert_eq!(Codec::parse(""), None);
    }

    #[test]
    fn even_dimension_rounds_down_and_rejects_unusable_sizes() {
        assert_eq!(even_dimension(1964), Some(1964));
        assert_eq!(even_dimension(1963), Some(1962));
        assert_eq!(even_dimension(2), Some(2));
        assert_eq!(even_dimension(3), Some(2));
        assert_eq!(even_dimension(1), None);
        assert_eq!(even_dimension(0), None);
    }

    #[test]
    fn sample_time_is_relative_to_first_kept_frame_in_100ns_units() {
        let first = 9_000_000_000u64; // ticks 90_000_000
        assert_eq!(sample_time_100ns(first, first), 0);
        // One 60 Hz interval later: 16_666_700 ns is 166_667 units.
        assert_eq!(sample_time_100ns(first + 16_666_700, first), 166_667);
        // A gap spanning dropped frames is kept as elapsed time, not an index count.
        assert_eq!(sample_time_100ns(first + 50_000_000, first), 500_000);
    }

    #[test]
    fn sample_time_saturates_when_a_frame_precedes_the_first() {
        assert_eq!(sample_time_100ns(100, 500), 0);
    }

    #[test]
    fn sample_duration_matches_requested_fps() {
        assert_eq!(sample_duration_100ns(30), 333_333);
        assert_eq!(sample_duration_100ns(60), 166_666);
    }

    #[test]
    fn bitrate_is_scaled_and_clamped() {
        assert_eq!(bitrate_bps(1964, 1282, 60), 15_107_088);
        assert_eq!(bitrate_bps(320, 240, 30), 2_000_000);
        assert_eq!(bitrate_bps(7680, 4320, 60), 40_000_000);
    }

    #[test]
    fn nv12_black_and_white_hit_limited_range_endpoints() {
        let mut dst = vec![0u8; nv12_len(4, 2)];
        bgra_to_nv12(&solid(4, 2, 4, [0, 0, 0]), 16, 4, 2, &mut dst).unwrap();
        assert!(dst[..8].iter().all(|&y| y == 16));
        assert!(dst[8..].iter().all(|&c| c == 128));

        bgra_to_nv12(&solid(4, 2, 4, [255, 255, 255]), 16, 4, 2, &mut dst).unwrap();
        assert!(dst[..8].iter().all(|&y| y == 235));
        assert_eq!(dst[8], 127, "U of white is within one code of neutral");
        assert_eq!(dst[9], 128);
    }

    #[test]
    fn nv12_pure_red_matches_bt709_values() {
        let mut dst = vec![0u8; nv12_len(2, 2)];
        bgra_to_nv12(&solid(2, 2, 2, [255, 0, 0]), 8, 2, 2, &mut dst).unwrap();
        assert_eq!(dst[0], 63);
        assert_eq!(&dst[4..6], &[102, 240]);
    }

    #[test]
    fn nv12_crops_top_left_region_of_a_wider_source() {
        // 6-pixel-wide source; columns 4 and 5 are red and fall outside the 4-wide output.
        let mut src = solid(4, 2, 6, [0, 0, 0]);
        let red = solid(2, 2, 6, [255, 0, 0]);
        for y in 0..2 {
            for x in 0..2 {
                let at = (y * 6 + 4 + x) * 4;
                src[at..at + 4].copy_from_slice(&red[(y * 6 + x) * 4..(y * 6 + x) * 4 + 4]);
            }
        }
        let mut dst = vec![0u8; nv12_len(4, 2)];
        bgra_to_nv12(&src, 24, 4, 2, &mut dst).unwrap();
        assert!(
            dst[..8].iter().all(|&y| y == 16),
            "red columns must not leak into the crop"
        );
        assert!(dst[8..].iter().all(|&c| c == 128));
    }

    #[test]
    fn nv12_rejects_odd_or_short_inputs() {
        let mut dst = vec![0u8; nv12_len(4, 4)];
        let src = solid(4, 4, 4, [0, 0, 0]);
        assert!(bgra_to_nv12(&src, 16, 3, 4, &mut dst).is_err());
        assert!(bgra_to_nv12(&src, 16, 4, 3, &mut dst).is_err());
        assert!(bgra_to_nv12(&src[..20], 16, 4, 4, &mut dst).is_err());
        assert!(bgra_to_nv12(&src, 16, 4, 4, &mut dst[..10]).is_err());
    }
}
