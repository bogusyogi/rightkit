#if os(macOS)
import AppKit
import SwiftUI

// Mac side of the colour seam: same signatures as the shared Palette.swift
// expects, resolved per appearance through dynamic NSColor. The Windows build
// compiles SwiftRight's file of the same signatures instead.

extension Color {
    /// Resolved against whatever appearance is current when the colour is
    /// drawn, which for the notch is the panel's: `nil` for glass (so the Mac
    /// decides) and `darkAqua` for solid.
    init(dark: UInt32, light: UInt32, darkOpacity: Double = 1, lightOpacity: Double = 1) {
        let darkColor = NSColor(hex: dark).withAlphaComponent(darkOpacity)
        let lightColor = NSColor(hex: light).withAlphaComponent(lightOpacity)
        self.init(nsColor: NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? darkColor : lightColor
        })
    }
}

// `Palette.ramp` lives in DesignSystem/Palette.swift (pure SwiftUI, over `Color(dark:light:)`).

extension NSColor {
    convenience init(hex: UInt32) {
        self.init(
            srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
            green:   CGFloat((hex >> 8) & 0xFF) / 255,
            blue:    CGFloat(hex & 0xFF) / 255,
            alpha:   1
        )
    }

    /// A linear per-channel sRGB lerp — the same arithmetic the Windows ramp does in JS, kept
    /// deliberately simple rather than going through `blended(withFraction:of:)`, whose
    /// blending colour space is not something either side of a Mac/Windows parity claim
    /// should depend on.
    func blendedByChannel(with other: NSColor, fraction: CGFloat) -> NSColor {
        guard let a = usingColorSpace(.sRGB), let b = other.usingColorSpace(.sRGB) else { return self }
        func lerp(_ x: CGFloat, _ y: CGFloat) -> CGFloat { x + (y - x) * fraction }
        return NSColor(
            srgbRed: lerp(a.redComponent, b.redComponent),
            green:   lerp(a.greenComponent, b.greenComponent),
            blue:    lerp(a.blueComponent, b.blueComponent),
            alpha:   1
        )
    }
}
#endif
