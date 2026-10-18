import SwiftUI

/// The bar above a section's content: a title, an optional subtitle and a slot for trailing actions.
/// It draws no background of its own: the shell puts it on the window's chrome colour. The title is
/// drawn in the theme's `headerTitle` style.
public struct HubHeader<Trailing: View>: View {
    private let title: String
    private let subtitle: String?
    private let trailing: Trailing
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    public init(title: String, subtitle: String? = nil, @ViewBuilder trailing: () -> Trailing) {
        self.title = title
        self.subtitle = subtitle
        self.trailing = trailing()
    }

    private var palette: HubPalette { theme.palette(scheme) }

    public var body: some View {
        HStack(spacing: theme.spacing.md) {
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: theme.headerTitle.text(title))
                    .font(theme.headerTitle.font)
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                if let subtitle {
                    Text(verbatim: subtitle)
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                        .lineLimit(1)
                }
            }
            Spacer()
            trailing
        }
        .padding(.leading, theme.layout.titleInset)
        .padding(.trailing, theme.spacing.lg)
        .frame(height: theme.layout.headerHeight)
    }
}

public extension HubHeader where Trailing == HubNoTrailing {
    /// A header with no trailing actions.
    init(title: String, subtitle: String? = nil) {
        self.init(title: title, subtitle: subtitle) { HubNoTrailing() }
    }
}
