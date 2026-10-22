//! Straight-alpha colours for the painter and the blend maths that turns them into the
//! premultiplied `0xAARRGGBB` pixels `UpdateLayeredWindow` expects.

/// An 8-bit-per-channel colour with straight (non-premultiplied) alpha.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba::new(0, 0, 0, 0);
    pub const BLACK: Rgba = Rgba::new(0, 0, 0, 255);
    pub const WHITE: Rgba = Rgba::new(255, 255, 255, 255);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Opaque colour.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::new(r, g, b, 255)
    }

    /// Opaque colour from `0xRRGGBB`.
    pub const fn from_hex(rgb: u32) -> Self {
        Self::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255)
    }

    /// The same colour with alpha `a` in `0.0..=1.0` (replaces the alpha).
    pub fn with_alpha(self, a: f32) -> Self {
        Self {
            a: (a.clamp(0.0, 1.0) * 255.0).round() as u8,
            ..self
        }
    }

    /// The same colour with its alpha multiplied by `k` in `0.0..=1.0`.
    pub fn scaled_alpha(self, k: f32) -> Self {
        Self {
            a: (f32::from(self.a) * k.clamp(0.0, 1.0)).round() as u8,
            ..self
        }
    }

    /// Linear blend of two colours, `t` in `0.0..=1.0` (0 is `self`).
    pub fn lerp(self, other: Rgba, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Self::new(
            mix(self.r, other.r),
            mix(self.g, other.g),
            mix(self.b, other.b),
            mix(self.a, other.a),
        )
    }
}

#[inline]
fn pack(a: f32, r: f32, g: f32, b: f32) -> u32 {
    let q = |v: f32| v.round().clamp(0.0, 255.0) as u32;
    (q(a) << 24) | (q(r) << 16) | (q(g) << 8) | q(b)
}

/// Source-over of `color` at `coverage` (`0.0..=1.0`) onto a premultiplied destination pixel.
#[inline]
pub(crate) fn blend(dst: &mut u32, color: Rgba, coverage: f32) {
    let a = (f32::from(color.a) / 255.0) * coverage.clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let d = *dst;
    let inv = 1.0 - a;
    let channel = |shift: u32| ((d >> shift) & 0xFF) as f32;
    *dst = pack(
        255.0 * a + channel(24) * inv,
        f32::from(color.r) * a + channel(16) * inv,
        f32::from(color.g) * a + channel(8) * inv,
        f32::from(color.b) * a + channel(0) * inv,
    );
}

/// Source-over of a premultiplied source pixel onto a premultiplied destination pixel.
#[inline]
pub(crate) fn blend_premul(dst: &mut u32, src: u32) {
    let sa = (src >> 24) & 0xFF;
    if sa == 0 {
        return;
    }
    if sa == 255 {
        *dst = src;
        return;
    }
    let inv = 255 - sa;
    let d = *dst;
    let mix = |shift: u32| {
        let s = (src >> shift) & 0xFF;
        let dd = (d >> shift) & 0xFF;
        (s + (dd * inv + 127) / 255).min(255)
    };
    *dst = (mix(24) << 24) | (mix(16) << 16) | (mix(8) << 8) | mix(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_alpha() {
        let c = Rgba::from_hex(0xFF5630);
        assert_eq!((c.r, c.g, c.b, c.a), (0xFF, 0x56, 0x30, 255));
        assert_eq!(c.with_alpha(0.5).a, 128);
        assert_eq!(c.with_alpha(2.0).a, 255);
        assert_eq!(c.with_alpha(0.5).scaled_alpha(0.5).a, 64);
    }

    #[test]
    fn opaque_blend_replaces_and_half_blend_premultiplies() {
        let mut px = 0u32;
        blend(&mut px, Rgba::rgb(200, 100, 50), 1.0);
        assert_eq!(px, 0xFFC8_6432_u32);
        let mut px = 0u32;
        blend(&mut px, Rgba::rgb(200, 100, 50), 0.5);
        // Premultiplied: colour channels are halved along with alpha.
        assert_eq!(px >> 24, 128);
        assert_eq!((px >> 16) & 0xFF, 100);
        assert_eq!((px >> 8) & 0xFF, 50);
        assert_eq!(px & 0xFF, 25);
    }

    #[test]
    fn zero_coverage_is_a_no_op() {
        let mut px = 0x80_10_20_30u32;
        blend(&mut px, Rgba::WHITE, 0.0);
        assert_eq!(px, 0x80_10_20_30);
    }

    #[test]
    fn premultiplied_source_over() {
        let mut dst = 0xFF_00_00_FFu32;
        blend_premul(&mut dst, 0x80_80_00_00); // 50% red, premultiplied
        assert_eq!(dst >> 24, 255);
        assert_eq!((dst >> 16) & 0xFF, 0x80);
        assert!(dst & 0xFF >= 0x7F && dst & 0xFF <= 0x80);
    }

    #[test]
    fn lerp_endpoints() {
        let a = Rgba::rgb(0, 0, 0);
        let b = Rgba::rgb(200, 100, 50);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        assert_eq!(a.lerp(b, 0.5), Rgba::rgb(100, 50, 25));
    }
}
