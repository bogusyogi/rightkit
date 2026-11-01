import SwiftUI

/// The dark values are sampled from `docs/design/frame-124-hover-tooltip.png`,
/// not invented, and they differ slightly from the hexes written in the design
/// spec — the frame is the source of truth, so the sampled values win.
///
/// The light values are not sampled from anything, because there is no light
/// frame: they exist only because the glass surface (`NotchSurfaceStyle.glass`)
/// hands light and dark to the Mac's Appearance settings, and white ink on
/// light glass is unreadable. They were picked for at least 3:1 against white.
/// `solid` pins the panel to `darkAqua`, so it keeps the frame's hexes exactly.
///
/// The two tracks are translucent white / black rather than a fixed grey so
/// they darken or lighten whatever is behind them instead of looking painted
/// on top of the glass. Their dark alphas are the ones that composite to the
/// frame's `#303030` / `#2D2D2D` over black, so the solid style is unchanged.
///
/// `notch` and `card` stay pure black: they paint the resting pill, which has
/// to read as the bezel, and the whole surface in the solid style.
public enum Palette {
    public static let notch         = Color.black                    // #000000
    public static let card          = Color.black                    // #000000
    public static let ringTrack     = Color(dark: 0xFFFFFF, light: 0x000000,
                                     darkOpacity: 0.188, lightOpacity: 0.16)
    public static let barTrack      = Color(dark: 0xFFFFFF, light: 0x000000,
                                     darkOpacity: 0.176, lightOpacity: 0.15)

    /// Named so `UsageBand.rampColor` can interpolate between them per-appearance rather
    /// than blending two already-resolved `Color`s (which would mix in whichever appearance
    /// happened to be current when the `Color` was built, not the one it draws in).
    public static let amplePair: (dark: UInt32, light: UInt32) = (0x2E6B4A, 0x2A6E47)   // muted dark green: present, not loud
    public static let watchPair: (dark: UInt32, light: UInt32) = (0xC2570F, 0xB4530F)   // dark orange
    /// Already 3.5:1 on white, so the warning colour is the same in both.
    public static let criticalPair: (dark: UInt32, light: UInt32) = (0xA51D24, 0x9B1B1B) // deep red

    public static let ample         = Color(dark: amplePair.dark, light: amplePair.light)
    public static let watch         = Color(dark: watchPair.dark, light: watchPair.light)
    public static let critical      = Color(dark: criticalPair.dark, light: criticalPair.light)

    // Generation-speed bands are independent of cloud quota usage.

    /// The one wash of our own laid *under* the system's glass, and only for
    /// `NotchSurfaceStyle.darkGlass`: that style exists because a black notch
    /// was asked for regardless of the Mac's Appearance, which the untinted
    /// `glass` cannot promise. It is a background beneath `Glass.clear`, not a
    /// `Glass.tint` — tinting only colourises an adaptive material and made the
    /// surface lighter, so the darkening has to happen behind the glass. 0.45
    /// was the first value tried and read too light through `Glass.clear` on a
    /// real Mac (macOS 27) against a light desktop; 0.60 is what the user
    /// chose by eye instead — opaque enough to be the bezel, thin enough not
    /// to be `solid`.
    public static let darkGlassDim = Color.black.opacity(0.60)

    /// Tooltip copy retains the frame's #808080 secondary ink in Dark glass.
    /// It needs a deeper local backing than the notch itself when a light
    /// desktop is visible through `Glass.clear`, otherwise the two greys merge.
    public static let darkGlassTooltipDim = Color.black.opacity(0.80)

    /// A tooltip-only wash for standard Liquid Glass in dark appearance. It is
    /// intentionally weaker than `darkGlassDim`: regular glass stays visibly
    /// distinct from the user-selected always-dark surface.
    public static let liquidGlassTooltipDim = Color.black.opacity(0.35)

    public static let textPrimary   = Color(dark: 0xFFFFFF, light: 0x000000)
    public static let textSecondary = Color(dark: 0x808080, light: 0x6B6B6B)
    /// Used only by dark, standard Liquid Glass tooltips. Other surfaces keep
    /// `textSecondary`, including their frame-accurate #808080 dark ink.
    public static let readableTooltipTextSecondary = Color(dark: 0xC2C2C2, light: 0x6B6B6B)
}

extension Color {
    init(hex: UInt32) {
        self.init(
            .sRGB,
            red:   Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue:  Double(hex & 0xFF) / 255,
            opacity: 1
        )
    }
}

private struct ReduceTransparencyKey: EnvironmentKey {
    static let defaultValue: Bool = false
}

extension EnvironmentValues {
    /// True when macOS Accessibility "Reduce Transparency" is enabled in system settings,
    /// or explicitly overridden via `.environment(\.codenotchReduceTransparency, ...)`.
    var codenotchReduceTransparency: Bool {
        get { self[ReduceTransparencyKey.self] || self.accessibilityReduceTransparency }
        set { self[ReduceTransparencyKey.self] = newValue }
    }
}

private struct HeadlessGlassKey: EnvironmentKey {
    static let defaultValue: Bool = false
}

private struct TooltipSecondaryInkKey: EnvironmentKey {
    static let defaultValue = Palette.textSecondary
}

extension EnvironmentValues {
    /// Secondary ink resolved for the current tooltip surface. This stays
    /// frame-accurate unless ordinary dark Liquid Glass needs extra contrast.
    var tooltipSecondaryInk: Color {
        get { self[TooltipSecondaryInkKey.self] }
        set { self[TooltipSecondaryInkKey.self] = newValue }
    }
}

extension EnvironmentValues {
    /// Draw the glass path with the system material left out. Tests only; the
    /// app never sets it.
    ///
    /// `ImageRenderer` cannot draw the material faithfully: in a cold process
    /// it paints `glassEffect` as nothing at all, and once any test has shown
    /// a live `NotchPanel` it paints it as an opaque flat grey over its ZStack
    /// siblings for the rest of the process. Either way the pixels say nothing
    /// about the product. So the glass pixel tests render everything *around*
    /// the material — the transparent body fill, the `darkGlass` dim, the
    /// opaque hardware band — which is the part that is ours to get wrong.
    /// See TASKS.md, "The hardware's band stays black".
    var codenotchHeadlessGlass: Bool {
        get { self[HeadlessGlassKey.self] }
        set { self[HeadlessGlassKey.self] = newValue }
    }
}

extension Palette {
    /// A continuous point between two palette anchors: each appearance's colour is interpolated
    /// in sRGB channels (a linear per-channel lerp, the arithmetic the Windows ramp does too) and
    /// handed to the appearance-resolving `Color(dark:light:)` seam, so the ramp follows the
    /// appearance it is drawn in. Channels round to 8 bits.
    public static func ramp(from: (dark: UInt32, light: UInt32), to: (dark: UInt32, light: UInt32), fraction: Double) -> Color {
        let t = min(max(fraction, 0), 1)
        func lerp(_ a: UInt32, _ b: UInt32) -> UInt32 {
            func channel(_ shift: UInt32) -> UInt32 {
                let x = Double((a >> shift) & 0xFF), y = Double((b >> shift) & 0xFF)
                return UInt32((x + (y - x) * t).rounded())
            }
            return (channel(16) << 16) | (channel(8) << 8) | channel(0)
        }
        return Color(dark: lerp(from.dark, to.dark), light: lerp(from.light, to.light))
    }
}
