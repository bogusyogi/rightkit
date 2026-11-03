//! Colour contrast owner (rightkit#847): WCAG 2.x relative luminance, contrast ratio and the ink
//! (text or glyph colour) that reads best on a background, for sRGB colours.
//!
//! The maths are those of `@rightkit/theme` (`luminance`, `contrastRatio`, `readableOn` in
//! `src/color.ts`) and of `RightKitUI.RightKitContrast` in RightKitSwift, so a Rust, a Swift and a
//! TypeScript surface pick the same ink for the same colour. Channels are linearised (the sRGB
//! transfer function) before weighting; a luminance computed on the gamma-encoded channels is not
//! WCAG luminance and is not what this crate returns.
//!
//! Alpha is ignored, as in the theme package: callers composite a translucent colour over its
//! background first.
//!
//! `readable_on(Rgb::from_hex("#F2C200").unwrap(), Rgb::WHITE, Rgb::DEFAULT_DARK)` is the dark ink.

/// An sRGB colour, 8 bits per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const WHITE: Rgb = Rgb::new(0xFF, 0xFF, 0xFF);
    pub const BLACK: Rgb = Rgb::new(0x00, 0x00, 0x00);
    /// The theme package's default dark ink (`#1A1208`).
    pub const DEFAULT_DARK: Rgb = Rgb::new(0x1A, 0x12, 0x08);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse `#RGB`, `#RRGGBB` or `#RRGGBBAA` (the `#` is required, case-insensitive, surrounding
    /// whitespace ignored; alpha is discarded). `None` for anything else.
    pub fn from_hex(text: &str) -> Option<Rgb> {
        let digits = text.trim().strip_prefix('#')?;
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let value = |s: &str| u8::from_str_radix(s, 16).ok();
        match digits.len() {
            3 => {
                let n = |i: usize| value(&digits[i..=i]).map(|v| v * 17);
                Some(Rgb::new(n(0)?, n(1)?, n(2)?))
            }
            6 | 8 => Some(Rgb::new(
                value(&digits[0..2])?,
                value(&digits[2..4])?,
                value(&digits[4..6])?,
            )),
            _ => None,
        }
    }

    /// `#RRGGBB`, upper case.
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

/// The sRGB transfer function for one 8-bit channel, to linear light (constants as in the theme package).
fn linear(channel: u8) -> f64 {
    let s = f64::from(channel) / 255.0;
    if s <= 0.03928 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.x relative luminance, 0.0 (black) to 1.0 (white).
pub fn relative_luminance(c: Rgb) -> f64 {
    0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
}

/// WCAG 2.x contrast ratio between two colours, 1.0 to 21.0, symmetric.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Whichever of `light` and `dark` reads better on `background`; a tie picks `light`
/// (`readableOn` in the theme package).
pub fn readable_on(background: Rgb, light: Rgb, dark: Rgb) -> Rgb {
    if contrast_ratio(light, background) >= contrast_ratio(dark, background) {
        light
    } else {
        dark
    }
}

/// Whether `background` is light: black ink reads better on it than white ink. This is the WCAG
/// replacement for a "luminance above 0.55" test on gamma-encoded channels; it switches where the
/// two inks have equal contrast (relative luminance about 0.179).
pub fn is_light(background: Rgb) -> bool {
    readable_on(background, Rgb::WHITE, Rgb::BLACK) == Rgb::BLACK
}
