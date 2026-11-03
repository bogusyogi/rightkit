# rightkit-contrast

Colour contrast for Rust apps, with no dependencies: WCAG 2.x relative luminance, contrast ratio and the ink that reads best on a background (rightkit#847). It carries the same maths as `@rightkit/theme` (`luminance`, `contrastRatio`, `readableOn`) and `RightKitUI.RightKitContrast` in RightKitSwift, so every surface picks the same ink for the same colour.

Catalog capability: `color-contrast` (declaration-only).

- `Rgb::new`, `Rgb::from_hex` (`#RGB`, `#RRGGBB`, `#RRGGBBAA`; alpha dropped), `Rgb::to_hex`.
- `relative_luminance(Rgb) -> f64` (0..1, channels linearised).
- `contrast_ratio(a, b) -> f64` (1..21).
- `readable_on(background, light, dark) -> Rgb` (ties pick `light`).
- `is_light(background) -> bool`: black ink beats white ink on it.

## Not here

Alpha compositing (composite first), colour spaces other than sRGB, palette generation (`@rightkit/theme`), video grading (`rightkit-color`).

A luminance taken on gamma-encoded channels (`0.2126*r + 0.7152*g + 0.0722*b > 0.55`) is not WCAG luminance. Apps that carried that test (Pulse's clip strip, Windows `clip_strip/draw.rs` and macOS `ClipInk.isLight`) call `is_light` or `readable_on` instead.
