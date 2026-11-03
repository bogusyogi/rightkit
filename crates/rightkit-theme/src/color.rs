//! Colour arithmetic shared by the resolver: hex in, hex out. Port of `packages/theme/src/color.ts`;
//! alpha is ignored for contrast and dropped by [`to_hex`].

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

/// True for `#RGB`, `#RRGGBB` and `#RRGGBBAA` (surrounding whitespace allowed).
pub fn is_hex(value: &str) -> bool {
    parse_hex(value).is_some()
}

pub fn parse_hex(value: &str) -> Option<Rgb> {
    let digits = value.trim().strip_prefix('#')?;
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let full: String = match digits.len() {
        3 => digits.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => digits[..6].to_owned(),
        _ => return None,
    };
    let n = u32::from_str_radix(&full, 16).ok()?;
    Some(Rgb {
        r: f64::from((n >> 16) & 255),
        g: f64::from((n >> 8) & 255),
        b: f64::from(n & 255),
    })
}

/// `#RRGGBB`, upper case; channels rounded and clamped to 0..=255.
pub fn to_hex(c: Rgb) -> String {
    let ch = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    format!("#{:02X}{:02X}{:02X}", ch(c.r), ch(c.g), ch(c.b))
}

fn channel(v: f64) -> f64 {
    let s = v / 255.0;
    if s <= 0.03928 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

pub fn luminance(c: Rgb) -> f64 {
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}

/// WCAG 2.x contrast ratio, 1..=21.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (l1, l2) = (luminance(a), luminance(b));
    (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
}

/// sRGB-space mix of two colours; `t` is the share of `b`.
pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    Rgb {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
    }
}

/// Whichever of white or `#1A1208` reads better on `background`.
pub fn readable_on(background: Rgb) -> Rgb {
    let light = Rgb {
        r: 255.0,
        g: 255.0,
        b: 255.0,
    };
    let dark = Rgb {
        r: 26.0,
        g: 18.0,
        b: 8.0,
    };
    if contrast_ratio(light, background) >= contrast_ratio(dark, background) {
        light
    } else {
        dark
    }
}

/// Move `fg` toward white (dark `bg`) or black (light `bg`) just far enough to reach `min` contrast
/// against `bg`, keeping the hue. `fg` is returned unchanged when it already passes, and the extreme
/// when nothing in between reaches `min`. Same 24-step bisection as the TypeScript resolver, with the
/// mixed colour rounded to a hex channel at every step, as `mix` returns hex there.
pub fn ensure_contrast(fg: Rgb, bg: Rgb, min: f64) -> Rgb {
    let rounded = |c: Rgb| parse_hex(&to_hex(c)).unwrap_or(c);
    let fg = rounded(fg);
    if contrast_ratio(fg, bg) >= min {
        return fg;
    }
    let target = if luminance(bg) < 0.5 {
        Rgb {
            r: 255.0,
            g: 255.0,
            b: 255.0,
        }
    } else {
        Rgb {
            r: 0.0,
            g: 0.0,
            b: 0.0,
        }
    };
    if contrast_ratio(target, bg) < min {
        return target;
    }
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if contrast_ratio(rounded(mix(fg, target, mid)), bg) >= min {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    rounded(mix(fg, target, hi))
}
