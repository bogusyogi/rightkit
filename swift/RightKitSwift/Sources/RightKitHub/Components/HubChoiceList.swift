import SwiftUI

/// One option in a `HubChoiceList`.
public struct HubChoiceOption<Value: Hashable & Sendable>: Identifiable, Sendable {
    public let value: Value
    public let title: String
    public let detail: String?

    public var id: Value { value }

    public init(value: Value, title: String, detail: String? = nil) {
        self.value = value
        self.title = title
        self.detail = detail
    }
}

/// A single choice from a few options: a column of rows, the chosen one marked with a checkmark.
/// Use it where you would reach for a Picker (SwiftRight does not cover Picker).
public struct HubChoiceList<Value: Hashable & Sendable>: View {
    private let options: [HubChoiceOption<Value>]
    private let selection: Binding<Value>
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    public init(options: [HubChoiceOption<Value>], selection: Binding<Value>) {
        self.options = options
        self.selection = selection
    }

    private var palette: HubPalette { theme.palette(scheme) }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(options) { option in
                Button {
                    selection.wrappedValue = option.value
                } label: {
                    HubLayered {
                        HubHitArea()
                        HStack(spacing: theme.spacing.sm) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(verbatim: option.title)
                                    .font(theme.type.body.font)
                                    .foregroundStyle(palette.ink.color)
                                    .lineLimit(1)
                                if let detail = option.detail {
                                    Text(verbatim: detail)
                                        .font(theme.type.caption.font)
                                        .foregroundStyle(palette.ink3.color)
                                        .lineLimit(2)
                                }
                            }
                            Spacer()
                            Image(systemName: "checkmark")
                                .foregroundStyle(palette.accent.color)
                                .opacity(option.value == selection.wrappedValue ? 1 : 0)
                        }
                        .padding(.vertical, theme.spacing.xs + 2)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(option.value == selection.wrappedValue ? .isSelected : [])
            }
        }
    }
}
