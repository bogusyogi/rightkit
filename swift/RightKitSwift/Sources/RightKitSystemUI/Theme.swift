#if os(macOS)
import RightKitHub
import SwiftUI

/// Pulse's brand on the shared hub theme, taken from the Tauri hub (`hub/src/styles.css`).
///
/// - Accent: `--rk-brand: #0a84ff` and `--rk-brand-dark: #0a84ff`, the same blue in both appearances.
/// - Palette: the Tauri hub overrides no suite token (`brand.json` `tokens` is empty; `styles.css` sets
///   only `--rk-brand`, `--rk-brand-dark` and the wordmark font), so the palette is the suite default
///   (the cream chrome and plane) and nothing is overridden but the accent. `accent-ink` stays white,
///   as there.
/// - Sidebar: `sidebarWidth={170}` in `hub/src/App.tsx`.
/// - Title: the Tauri hub draws page titles and the wordmark in Tanker (`hub/public/fonts/Tanker-400.woff2`,
///   18 px and 20 px, weight 400, tracking 0.01 em, skipped here: Windows has no tracking). `Hub.registerFonts(services:)`
///   registers the bundled copy; `titleStyle` and `wordmarkStyle` (PulseFonts.swift) then name it, and
///   the shell does not uppercase (Tanker draws capitals itself). Until it is registered, or when the
///   platform cannot read the file, the stand-in is the system font, heavy and condensed, in capitals.
extension Hub {
    /// The Pulse hub theme. Pass it as `HubContext(theme:)`. It reads whether Tanker is registered each
    /// time it is asked, so call `registerFonts(services:)` before building the context.
    public static var theme: HubTheme {
        var base = HubTheme.standard
        base.layout.sidebarWidth = 170
        base.headerTitle = titleStyle
        let blue = HubColor(hex: 0x0A84FF)
        return base.branded(HubBrand(accent: blue, darkAccent: blue))
    }
}

/// The appearance the footer button asks for. A STUB: it records the choice and nothing applies it.
///
/// TODO(notch host): the notch owns the hub window, so it should observe `PulseAppearance.shared`, set the
/// window's appearance (and `.preferredColorScheme` on the scene), and persist the choice with the notch's
/// settings. Until then the button changes this value only.
@MainActor
@Observable
public final class PulseAppearance {
    public enum Preference: String, Sendable {
        case system, light, dark
    }

    public static let shared = PulseAppearance()

    /// What the user last chose with the footer button; `.system` until the first press.
    public var preference: Preference = .system

    public init() {}

    /// Chooses the opposite of the appearance now showing.
    public func toggle(from current: ColorScheme) {
        preference = current == .dark ? .light : .dark
    }
}

/// The sidebar footer of the Tauri hub: the PULSE wordmark in the accent, and a light/dark button.
/// Hand it to `HubShell(footer:)`.
public struct SidebarFooter: View {
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    public init() {}

    private var palette: HubPalette { theme.palette(scheme) }

    public var body: some View {
        HStack(spacing: 2) {
            Text(verbatim: "PULSE")
                .font(Hub.wordmarkStyle.font)
                .foregroundStyle(palette.accent.color)
                .lineLimit(1)
            Spacer(minLength: 0)
            Button {
                PulseAppearance.shared.toggle(from: scheme)
            } label: {
                Image(systemName: "circle.lefthalf.filled")
                    .foregroundStyle(palette.ink2.color)
                    .frame(width: 28, height: 28)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Appearance")
            .accessibilityHint("Switches between light and dark")
        }
    }
}
#endif
