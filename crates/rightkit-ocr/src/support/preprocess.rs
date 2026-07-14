use image::RgbImage;

/// Det normalization: ImageNet mean/std — verified empirically (2026-05-29).
/// Returns flat `Vec<f32>` in CHW order: [3][H][W].
pub(crate) fn det_chw_data(img: &RgbImage, w: u32, h: u32) -> Vec<f32> {
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let n = (w * h) as usize;
    let mut out = vec![0f32; 3 * n];
    for (y, row) in img.rows().enumerate() {
        for (x, px) in row.enumerate() {
            for c in 0..3usize {
                out[c * n + y * w as usize + x] = (px.0[c] as f32 / 255.0 - MEAN[c]) / STD[c];
            }
        }
    }
    out
}

/// Rec normalization: (pixel/255 − 0.5) / 0.5 — verified empirically (2026-05-29).
/// Returns flat `Vec<f32>` in CHW order: [3][48][W].
pub(crate) fn rec_chw_data(img: &RgbImage, w: u32) -> Vec<f32> {
    const H: u32 = 48;
    let n = (w * H) as usize;
    let mut out = vec![0f32; 3 * n];
    for (y, row) in img.rows().enumerate() {
        for (x, px) in row.enumerate() {
            for c in 0..3usize {
                out[c * n + y * w as usize + x] = (px.0[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }
    out
}

/// Hard cap on any single image dimension: detector-net input side AND working
/// width cap (downscale only — never upscale). Screenshots wider than this are
/// scaled to fit; narrower ones are detected at native resolution.
const DET_MAX_DIM: u32 = 2048;
// Aliases used at the two distinct call sites; keeping them as named bindings
// makes clear the semantic is the same value applied to two contexts.
const DET_MAX_SIDE: u32 = DET_MAX_DIM;
pub(crate) const DET_MAX_W: u32 = DET_MAX_DIM;
/// Maximum band height before an image is split into vertical tiles.
pub(crate) const DET_TILE_H: u32 = 1600;
/// Vertical overlap between adjacent bands (≥ a tall text line) so no line is
/// split across a seam.
pub(crate) const DET_BAND_OVERLAP: u32 = 200;

/// Det input size for a single band: keep native resolution (never upscale),
/// cap each side at [`DET_MAX_SIDE`], round both dims up to a multiple of 32.
///
/// The old pipeline force-shrank every image to a 960px long side, which
/// destroyed small body text on full-page screenshots — whole lines fell below
/// the detector's resolution and were never found. Bands are pre-capped by the
/// caller (`detect`), so this normally just rounds up to ×32.
pub(crate) fn det_net_size(orig_w: u32, orig_h: u32) -> (u32, u32) {
    let cap = |v: u32| v.clamp(32, DET_MAX_SIDE);
    let round32 = |v: u32| cap(v).div_ceil(32) * 32;
    (round32(orig_w), round32(orig_h))
}
