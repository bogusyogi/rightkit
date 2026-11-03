import Foundation

/// WCAG 2.x relative luminance, contrast ratio and readable ink for sRGB colours (rightkit#847).
///
/// The maths are those of `@rightkit/theme` (`luminance`, `contrastRatio`, `readableOn`) and of the
/// Rust crate `rightkit-contrast`, so every surface picks the same ink for the same colour. Channels
/// are linearised before weighting; a threshold on the gamma-encoded channels
/// (`0.2126*r + 0.7152*g + 0.0722*b > 0.55`) is not WCAG luminance and is not what this returns.
public enum RightKitContrast {
    /// An sRGB colour, components 0...255.
    public struct RGB: Sendable, Equatable, Hashable {
        public var red: UInt8
        public var green: UInt8
        public var blue: UInt8

        public init(red: UInt8, green: UInt8, blue: UInt8) {
            self.red = red
            self.green = green
            self.blue = blue
        }

        /// Components in 0...1 (clamped and rounded to the nearest 8-bit value).
        public init(unitRed red: Double, green: Double, blue: Double) {
            func byte(_ value: Double) -> UInt8 {
                guard value.isFinite else { return 0 }
                return UInt8((min(max(value, 0), 1) * 255).rounded())
            }
            self.init(red: byte(red), green: byte(green), blue: byte(blue))
        }

        /// Parse `#RGB`, `#RRGGBB` or `#RRGGBBAA` (the `#` is required; alpha is dropped). `nil` otherwise.
        public init?(hex text: String) {
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed.hasPrefix("#") else { return nil }
            var digits = String(trimmed.dropFirst())
            guard digits.allSatisfy({ $0.isHexDigit && $0.isASCII }) else { return nil }
            if digits.count == 3 {
                digits = digits.map { "\($0)\($0)" }.joined()
            } else if digits.count != 6 && digits.count != 8 {
                return nil
            }
            guard let value = UInt32(digits.prefix(6), radix: 16) else { return nil }
            self.init(
                red: UInt8((value >> 16) & 0xFF),
                green: UInt8((value >> 8) & 0xFF),
                blue: UInt8(value & 0xFF)
            )
        }

        /// `#RRGGBB`, upper case.
        public var hex: String {
            String(format: "#%02X%02X%02X", red, green, blue)
        }

        public static let white = RGB(red: 0xFF, green: 0xFF, blue: 0xFF)
        public static let black = RGB(red: 0x00, green: 0x00, blue: 0x00)
        /// The theme package's default dark ink, `#1A1208`.
        public static let defaultDark = RGB(red: 0x1A, green: 0x12, blue: 0x08)
    }

    private static func linear(_ channel: UInt8) -> Double {
        let s = Double(channel) / 255.0
        return s <= 0.03928 ? s / 12.92 : pow((s + 0.055) / 1.055, 2.4)
    }

    /// WCAG 2.x relative luminance, 0 (black) to 1 (white).
    public static func relativeLuminance(_ color: RGB) -> Double {
        0.2126 * linear(color.red) + 0.7152 * linear(color.green) + 0.0722 * linear(color.blue)
    }

    /// WCAG 2.x contrast ratio, 1 to 21, symmetric.
    public static func contrastRatio(_ a: RGB, _ b: RGB) -> Double {
        let la = relativeLuminance(a)
        let lb = relativeLuminance(b)
        return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)
    }

    /// Whichever of `light` and `dark` reads better on `background`; a tie picks `light`.
    public static func readableOn(_ background: RGB, light: RGB = .white, dark: RGB = .defaultDark) -> RGB {
        contrastRatio(light, background) >= contrastRatio(dark, background) ? light : dark
    }

    /// Whether `background` is light: black ink reads better on it than white ink.
    public static func isLight(_ background: RGB) -> Bool {
        readableOn(background, light: .white, dark: .black) == .black
    }
}
