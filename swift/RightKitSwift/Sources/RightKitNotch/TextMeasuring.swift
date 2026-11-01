import SwiftUI

/// Text metrics the notch layout needs outside of any view, so the layout maths
/// carries no AppKit. The Mac implementation is `AppKitTextMeasurer`
/// (`TextMeasuring+AppKit.swift`); layout runs before and outside views, so it
/// is injected through `TextMeasuringRegistry`, not the environment.
protocol TextMeasuring {
    /// Unrounded advance width of one line of system-font text.
    func width(of text: String, size: CGFloat, weight: Font.Weight, monospacedDigits: Bool) -> CGFloat
    /// The system font's line box: ascender - descender + leading, rounded up.
    func lineHeight(size: CGFloat, weight: Font.Weight) -> CGFloat
    /// Height of `text` wrapped to `width` in the system font (not rounded to lines).
    func wrappedHeight(of text: String, size: CGFloat, weight: Font.Weight, width: CGFloat) -> CGFloat
}

enum TextMeasuringRegistry {
    /// Set once at launch by the platform (`AppDelegate`: `AppKitTextMeasurer`). Until then a
    /// character-count estimate, so the face compiles and lays out without any platform text engine.
    nonisolated(unsafe) static var shared: TextMeasuring = EstimatedTextMeasurer()
}

/// The registry's default: a rough advance (0.55 em per character) and a 1.2 em line box.
struct EstimatedTextMeasurer: TextMeasuring {
    func width(of text: String, size: CGFloat, weight: Font.Weight, monospacedDigits: Bool) -> CGFloat {
        CGFloat(text.count) * size * 0.55
    }
    func lineHeight(size: CGFloat, weight: Font.Weight) -> CGFloat { ceil(size * 1.2) }
    func wrappedHeight(of text: String, size: CGFloat, weight: Font.Weight, width: CGFloat) -> CGFloat {
        let perLine = max(width, 1)
        let lines = max(1, ceil(self.width(of: text, size: size, weight: weight, monospacedDigits: false) / perLine))
        return lines * lineHeight(size: size, weight: weight)
    }
}
