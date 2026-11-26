pub(crate) fn is_light(bg: (u8, u8, u8)) -> bool {
    let (r, g, b) = bg;
    let y = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    y > 128.0
}

pub(crate) fn blend(fg: (u8, u8, u8), bg: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    let r = (fg.0 as f32 * alpha + bg.0 as f32 * (1.0 - alpha)) as u8;
    let g = (fg.1 as f32 * alpha + bg.1 as f32 * (1.0 - alpha)) as u8;
    let b = (fg.2 as f32 * alpha + bg.2 as f32 * (1.0 - alpha)) as u8;
    (r, g, b)
}

/// Returns the perceptual color distance between two RGB colors: rightkit-theme's
/// CIE76 delta E (sRGB to CIELAB, D65).
pub(crate) fn perceptual_distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
    let rgb = |c: (u8, u8, u8)| rightkit_theme::color::Rgb {
        r: f64::from(c.0),
        g: f64::from(c.1),
        b: f64::from(c.2),
    };
    rightkit_theme::color::delta_e76(rgb(a), rgb(b)) as f32
}
