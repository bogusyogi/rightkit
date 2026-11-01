#if os(iOS)
import SwiftUI
import UIKit

/// Pulse hub tokens (docs: scratch redesign-astra DESIGN.md, "Tokens"), dark first with the light set.
/// Every colour is dynamic: it follows the window's appearance and Increase Contrast.
enum Theme {
    static let page = Color.pulse(dark: 0x0C0C0C, light: 0xECECF0)
    static let card = Color.pulse(dark: 0x171717, light: 0xFFFFFF)
    static let raised = Color.pulse(dark: 0x232323, light: 0xF1F1F5)
    /// Inputs and tracks.
    static let fill = Color.pulse(dark: 0xFFFFFF, light: 0x000000, darkAlpha: 0.07, lightAlpha: 0.05)
    static let fillPressed = Color.pulse(dark: 0xFFFFFF, light: 0x000000, darkAlpha: 0.12, lightAlpha: 0.09)
    static let hair = Color.pulse(dark: 0xFFFFFF, light: 0x000000, darkAlpha: 0.08, lightAlpha: 0.09,
                                  contrastDarkAlpha: 0.2, contrastLightAlpha: 0.22)

    static let ink = Color.pulse(dark: 0xF4F4F4, light: 0x1C1C1E)
    static let ink2 = Color.pulse(dark: 0xB3B3B3, light: 0x55555C, contrastDark: 0xD6D6D6, contrastLight: 0x3A3A40)
    static let ink3 = Color.pulse(dark: 0x949494, light: 0x66666E, contrastDark: 0xB3B3B3, contrastLight: 0x55555C)

    static let ok = Color.pulse(dark: 0x3CCF6E, light: 0x136A2E)
    static let warn = Color.pulse(dark: 0xF3C23B, light: 0x774A00)
    static let bad = Color.pulse(dark: 0xFF7A5C, light: 0xC2301A)
    static let info = Color.pulse(dark: 0x6AA6FF, light: 0x0B5CD0)

    /// Your own message bubble (the hub chat's muted blue-grey card).
    static let mine = Color.pulse(dark: 0x26324A, light: 0xDCE6F7)
    /// Label on a primary (ink-filled) button.
    static let onInk = Color.pulse(dark: 0x0C0C0C, light: 0xFFFFFF)

    static let cardRadius: CGFloat = 16
    static let controlRadius: CGFloat = 12
    static let gutter: CGFloat = 16
}

extension Color {
    static func pulse(
        dark: UInt32, light: UInt32,
        darkAlpha: CGFloat = 1, lightAlpha: CGFloat = 1,
        contrastDark: UInt32? = nil, contrastLight: UInt32? = nil,
        contrastDarkAlpha: CGFloat? = nil, contrastLightAlpha: CGFloat? = nil
    ) -> Color {
        Color(uiColor: UIColor { traits in
            let high = traits.accessibilityContrast == .high
            if traits.userInterfaceStyle == .dark {
                return UIColor(hex: high ? (contrastDark ?? dark) : dark,
                               alpha: high ? (contrastDarkAlpha ?? darkAlpha) : darkAlpha)
            }
            return UIColor(hex: high ? (contrastLight ?? light) : light,
                           alpha: high ? (contrastLightAlpha ?? lightAlpha) : lightAlpha)
        })
    }
}

extension UIColor {
    convenience init(hex: UInt32, alpha: CGFloat = 1) {
        self.init(red: CGFloat((hex >> 16) & 0xFF) / 255,
                  green: CGFloat((hex >> 8) & 0xFF) / 255,
                  blue: CGFloat(hex & 0xFF) / 255,
                  alpha: alpha)
    }
}

// MARK: Surfaces

extension View {
    /// A quiet hub card: 16 pt continuous corners on `--card`.
    func card(padding: CGFloat = 16) -> some View {
        self.padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous))
    }

    /// The page behind every screen.
    func pageBackground() -> some View {
        background(Theme.page.ignoresSafeArea())
    }
}

/// A section title above a card, like "Claude chats".
struct SectionTitle: View {
    let text: String
    var detail: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(text).font(.headline).foregroundStyle(Theme.ink)
            Spacer()
            if let detail {
                Text(detail).font(.footnote).foregroundStyle(Theme.ink3)
            }
        }
        .padding(.horizontal, 4)
        .accessibilityAddTraits(.isHeader)
    }
}

/// A hairline between rows inside a card.
struct Hairline: View {
    var body: some View {
        Rectangle().fill(Theme.hair).frame(height: 1).accessibilityHidden(true)
    }
}

// MARK: Status

public enum StatusTone: Sendable, Equatable {
    case ok, warn, bad, info, idle

    public var color: Color {
        switch self {
        case .ok: Theme.ok
        case .warn: Theme.warn
        case .bad: Theme.bad
        case .info: Theme.info
        case .idle: Theme.ink3
        }
    }
}

struct StatusDot: View {
    let tone: StatusTone
    var size: CGFloat = 8

    var body: some View {
        Circle().fill(tone.color).frame(width: size, height: size).accessibilityHidden(true)
    }
}

/// A small dot followed by plain text: the hub's way of showing state.
struct StatusText: View {
    let tone: StatusTone
    let text: String
    var font: Font = .subheadline

    var body: some View {
        HStack(spacing: 6) {
            StatusDot(tone: tone)
            Text(text).font(font).foregroundStyle(Theme.ink2)
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: Buttons

/// Solid ink fill, dark label: the one strongest action on a screen.
struct PrimaryButtonStyle: ButtonStyle {
    var fullWidth = true
    @Environment(\.isEnabled) private var enabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.body.weight(.semibold))
            .foregroundStyle(Theme.onInk)
            .padding(.horizontal, 18)
            .frame(maxWidth: fullWidth ? .infinity : nil, minHeight: 48)
            .background(Theme.ink, in: RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous))
            .opacity(enabled ? (configuration.isPressed ? 0.82 : 1) : 0.35)
            .contentShape(Rectangle())
    }
}

/// Quiet fill: secondary actions.
struct QuietButtonStyle: ButtonStyle {
    var fullWidth = true
    @Environment(\.isEnabled) private var enabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.body.weight(.medium))
            .foregroundStyle(enabled ? Theme.ink : Theme.ink3)
            .padding(.horizontal, 18)
            .frame(maxWidth: fullWidth ? .infinity : nil, minHeight: 48)
            .background(configuration.isPressed ? Theme.fillPressed : Theme.fill,
                        in: RoundedRectangle(cornerRadius: Theme.controlRadius, style: .continuous))
            .contentShape(Rectangle())
    }
}

// MARK: Mark

/// The Pulse mark: the icon pack's dark-tile artwork (brand/icon-pack, #514), the same mark as the app icon.
struct Mark: View {
    var size: CGFloat = 56

    var body: some View {
        Image("PulseMark")
            .resizable()
            .interpolation(.high)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

// MARK: Formatting

enum TimeLabel {
    /// "11:52" today, "Yesterday", "Mon" this week, else "Oct 12". Nil for an unknown time.
    static func short(_ date: Date) -> String? {
        guard date.timeIntervalSince1970 > 946_684_800 else { return nil }
        let calendar = Calendar.current
        if calendar.isDateInToday(date) { return date.formatted(date: .omitted, time: .shortened) }
        if calendar.isDateInYesterday(date) { return "Yesterday" }
        if let days = calendar.dateComponents([.day], from: date, to: .now).day, days < 7 {
            return date.formatted(.dateTime.weekday(.abbreviated))
        }
        return date.formatted(.dateTime.month(.abbreviated).day())
    }
}

extension String {
    /// First non-empty line, whitespace collapsed: for one-line previews.
    var firstLine: String {
        let line = split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty } ?? ""
        return line.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }
}
#endif
