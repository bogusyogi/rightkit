import SwiftUI

/// The tone of a value: neutral text, the accent, or a status colour.
public enum HubTone: Sendable, Equatable {
    case neutral
    case accent
    case ok
    case warn
    case bad

    /// The colour for this tone.
    public func color(in palette: HubPalette) -> HubColor {
        switch self {
        case .neutral: return palette.ink
        case .accent: return palette.accent
        case .ok: return palette.ok
        case .warn: return palette.warn
        case .bad: return palette.bad
        }
    }
}

/// A rounded panel with a hairline border. Content is laid out top to bottom, leading aligned.
public struct HubCard<Content: View>: View {
    private let padding: CGFloat?
    private let content: Content
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    /// - Parameter padding: Inner padding, or nil for the theme's `lg`. Pass 0 for a card of rows
    ///   that bring their own padding.
    public init(padding: CGFloat? = nil, @ViewBuilder content: () -> Content) {
        self.padding = padding
        self.content = content()
    }

    private var palette: HubPalette { theme.palette(scheme) }

    public var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.card).fill(palette.panel.color)
            RoundedRectangle(cornerRadius: theme.radii.card).stroke(palette.line.color, lineWidth: 1)
            VStack(alignment: .leading, spacing: 0) {
                content
            }
            .padding(padding ?? theme.spacing.lg)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A card showing one number: a title, a large value and an optional detail line.
public struct HubStatTile: View {
    private let title: String
    private let value: String
    private let detail: String?
    private let tone: HubTone
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    public init(title: String, value: String, detail: String? = nil, tone: HubTone = .neutral) {
        self.title = title
        self.value = value
        self.detail = detail
        self.tone = tone
    }

    private var palette: HubPalette { theme.palette(scheme) }

    public var body: some View {
        HubCard {
            VStack(alignment: .leading, spacing: 4) {
                Text(verbatim: title)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
                    .lineLimit(1)
                Text(verbatim: value)
                    .font(theme.type.stat.font)
                    .foregroundStyle(tone.color(in: palette).color)
                    .lineLimit(1)
                if let detail {
                    Text(verbatim: detail)
                        .font(theme.type.small.font)
                        .foregroundStyle(palette.ink3.color)
                        .lineLimit(1)
                }
            }
        }
    }
}

/// One row: an optional icon, a title with an optional detail line, and a trailing slot.
/// Put rows in a `HubCard(padding: 0)` and separate them with `Divider()`.
public struct HubRow<Trailing: View>: View {
    private let symbol: String?
    private let title: String
    private let detail: String?
    private let trailing: Trailing
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    public init(
        symbol: String? = nil,
        title: String,
        detail: String? = nil,
        @ViewBuilder trailing: () -> Trailing
    ) {
        self.symbol = symbol
        self.title = title
        self.detail = detail
        self.trailing = trailing()
    }

    private var palette: HubPalette { theme.palette(scheme) }

    public var body: some View {
        HStack(spacing: theme.spacing.row) {
            if let symbol {
                Image(systemName: symbol)
                    .foregroundStyle(palette.ink2.color)
                    .frame(width: 20)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: title)
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                if let detail {
                    Text(verbatim: detail)
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink3.color)
                        .lineLimit(2)
                }
            }
            Spacer()
            trailing
        }
        .padding(.horizontal, theme.spacing.md)
        .padding(.vertical, theme.spacing.row)
    }
}

/// A row with a plain text value on the trailing side.
public struct HubValueTrailing: View {
    private let value: String
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    public init(_ value: String) {
        self.value = value
    }

    public var body: some View {
        Text(verbatim: value)
            .font(theme.type.body.font)
            .foregroundStyle(theme.palette(scheme).ink2.color)
            .lineLimit(1)
    }
}

/// Nothing: the default trailing slot of `HubHeader` and the shell.
public struct HubNoTrailing: View {
    public init() {}

    public var body: some View {
        Color.clear.frame(width: 0, height: 0)
    }
}

public extension HubRow where Trailing == HubNoTrailing {
    /// A row with no trailing content.
    init(symbol: String? = nil, title: String, detail: String? = nil) {
        self.init(symbol: symbol, title: title, detail: detail) { HubNoTrailing() }
    }
}
