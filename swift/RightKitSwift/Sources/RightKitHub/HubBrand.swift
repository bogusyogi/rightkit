import SwiftUI

/// A product's brand, applied over the suite defaults. It mirrors `brand.json` in `@rightkit/theme`:
/// the accent (`brand-action`, with an optional dark one) and per-appearance overrides of the
/// tokens in `OVERRIDABLE_TOKENS`, named the same way ("ink-2", "ok-dot", "accent-ink", ...).
///
///     let theme = HubTheme.standard.branded(
///         HubBrand(accent: HubColor(hex: 0x0A84FF), light: ["ink": HubColor(hex: 0x1D1D1F)])
///     )
///
/// Derived tokens follow what they derive from, as in the shell's CSS: the accent's soft fill and
/// focus ring keep their alpha for the new accent; "ok-dot" and "warn-dot" carry their soft fills;
/// "bad" carries its soft fill and `danger`. A name that is not overridable is ignored here and
/// listed by `unknownTokens`.
public struct HubBrand: Sendable, Equatable {
    /// The accent in the light appearance (`brand-action`). Nil keeps the suite accent.
    public var accent: HubColor?
    /// The accent in the dark appearance (`dark["brand-action"]`). Nil uses `accent`.
    public var darkAccent: HubColor?
    /// Token overrides for the light appearance, keyed by `OVERRIDABLE_TOKENS` name.
    public var light: [String: HubColor]
    /// Token overrides for the dark appearance, keyed by `OVERRIDABLE_TOKENS` name.
    public var dark: [String: HubColor]

    public init(
        accent: HubColor? = nil,
        darkAccent: HubColor? = nil,
        light: [String: HubColor] = [:],
        dark: [String: HubColor] = [:]
    ) {
        self.accent = accent
        self.darkAccent = darkAccent
        self.light = light
        self.dark = dark
    }

    /// Override names that are not in `HubTheme.overridableTokens`; nothing is applied for them.
    public var unknownTokens: [String] {
        let known = Set(HubTheme.overridableTokens)
        return Set(light.keys).union(dark.keys).subtracting(known).sorted()
    }
}

public extension HubPalette {
    /// This palette with an accent and token overrides applied. See `HubBrand` for what follows what.
    func applying(accent newAccent: HubColor?, tokens: [String: HubColor]) -> HubPalette {
        var p = self
        if let newAccent {
            p.accent = newAccent
            p.accentSoft = HubColor(
                red: newAccent.red, green: newAccent.green, blue: newAccent.blue, opacity: accentSoft.opacity)
            p.focus = HubColor(
                red: newAccent.red, green: newAccent.green, blue: newAccent.blue, opacity: focus.opacity)
        }
        for (name, color) in tokens {
            switch name {
            case "chrome": p.chrome = color
            case "plane": p.plane = color
            case "elevated": p.elevated = color
            case "panel": p.panel = color
            case "sunk": p.sunk = color
            case "ink": p.ink = color
            case "ink-2": p.ink2 = color
            case "ink-3": p.ink3 = color
            case "ok": p.ok = color
            case "warn": p.warn = color
            case "bad":
                p.bad = color
                p.danger = color
                p.badSoft = HubColor(red: color.red, green: color.green, blue: color.blue, opacity: badSoft.opacity)
            case "ok-dot":
                p.okDot = color
                p.okSoft = HubColor(red: color.red, green: color.green, blue: color.blue, opacity: okSoft.opacity)
            case "warn-dot":
                p.warnDot = color
                p.warnSoft = HubColor(red: color.red, green: color.green, blue: color.blue, opacity: warnSoft.opacity)
            case "line": p.line = color
            case "line-strong": p.lineStrong = color
            case "hairline": p.hairline = color
            case "accent-ink": p.accentInk = color
            default: break
            }
        }
        return p
    }
}

public extension HubTheme {
    /// This theme with a brand applied to both appearances. Everything but the palettes is kept.
    func branded(_ brand: HubBrand) -> HubTheme {
        var themed = self
        themed.light = light.applying(accent: brand.accent, tokens: brand.light)
        themed.dark = dark.applying(accent: brand.darkAccent ?? brand.accent, tokens: brand.dark)
        return themed
    }
}
