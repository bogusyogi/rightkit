import Foundation
import RightKitHub
import SwiftUI

// Controls and wording that match the Tauri Apps page (`hub/src/views/Apps.tsx`, `apps.css`) where
// the shared `AC*` controls (also used by Cleanup) are drawn differently. Built from the same allowed
// controls; the `Apps` prefix keeps the names clear of the other sections' helpers.

/// The page's number wording (`bytes` in `api.ts`): decimal units, one decimal under 100, "0 B".
enum AppsFormat {
    static func bytes(_ value: Int64) -> String {
        let units = ["B", "KB", "MB", "GB", "TB"]
        var number = Double(value)
        var index = 0
        while number >= 1000 && index < units.count - 1 {
            number /= 1000
            index += 1
        }
        let text = (number >= 100 || index == 0) ? String(Int(number.rounded())) : String(format: "%.1f", number)
        return "\(text) \(units[index])"
    }
}

/// A small status label (`.ck-apps-badge`): the plane colour behind ink-2 text, a soft tint for a tone.
/// The text wraps when it is longer than the room it is given.
struct AppsBadge: View {
    private let text: String
    private let tone: HubTone
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(_ text: String, tone: HubTone = .neutral) {
        self.text = text
        self.tone = tone
    }

    private var palette: HubPalette { theme.palette(scheme) }

    private var fill: Color {
        switch tone {
        case .neutral, .accent: return palette.plane.color
        case .ok: return palette.okSoft.color
        case .warn: return palette.warnSoft.color
        case .bad: return palette.badSoft.color
        }
    }

    private var ink: Color {
        switch tone {
        case .neutral: return palette.ink2.color
        case .accent: return palette.accent.color
        case .ok: return palette.ok.color
        case .warn: return palette.warn.color
        case .bad: return palette.bad.color
        }
    }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: 6).fill(fill)
            Text(verbatim: text)
                .font(Font.system(size: 12))
                .foregroundStyle(ink)
                .padding(.horizontal, 7)
                .padding(.vertical, 1)
        }
        // A badge keeps its width on Windows' wider font; the name beside it gives way first.
        .layoutPriority(1)
    }
}

/// A small button for use on a card: the page's secondary button, a grey fill with no outline.
struct AppsButton: View {
    private let title: String
    private let enabled: Bool
    private let accessibility: String?
    private let action: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(
        _ title: String,
        enabled: Bool = true,
        accessibility: String? = nil,
        action: @escaping @MainActor () -> Void
    ) {
        self.title = title
        self.enabled = enabled
        self.accessibility = accessibility
        self.action = action
    }

    var body: some View {
        let palette = theme.palette(scheme)
        Button {
            action()
        } label: {
            HubLayered {
                RoundedRectangle(cornerRadius: theme.radii.control).fill(palette.fill3.color)
                Text(verbatim: title)
                    .font(theme.type.label.font)
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
            }
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.45)
        .accessibilityLabel(accessibility ?? title)
        // A button keeps its width on Windows' wider font; text and bars beside it give way first.
        .layoutPriority(1)
    }
}

/// The search box (`.ck-apps-search`): a magnifier and a text field in a grey, outlined field. `label`
/// is the name a screen reader says when it differs from the placeholder.
struct AppsSearchField: View {
    private let placeholder: String
    private let label: String
    private let text: Binding<String>
    /// The box shows the accent ring while the field has the keyboard, as Windows does (#336).
    @FocusState private var focused: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(_ placeholder: String, label: String, text: Binding<String>) {
        self.placeholder = placeholder
        self.label = label
        self.text = text
    }

    var body: some View {
        let palette = theme.palette(scheme)
        HubLayered {
            RoundedRectangle(cornerRadius: 7).fill(palette.fill2.color)
            RoundedRectangle(cornerRadius: 7)
                .stroke(focused ? palette.accent.color : palette.hairline.color, lineWidth: focused ? 2 : 1)
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(palette.ink3.color)
                TextField(placeholder, text: text)
                    .textFieldStyle(.plain)
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink.color)
                    .accessibilityLabel(label)
                    .focused($focused)
            }
            .padding(.horizontal, 10)
            .frame(height: 32)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A closed-by-default section (`<details>`): a heading that names the section and opens it, with no
/// chevron. The owner holds the open state.
struct AppsDisclosure<Content: View>: View {
    private let title: String
    private let bold: Bool
    private let isOpen: Bool
    private let toggle: @MainActor () -> Void
    private let content: Content
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    /// - Parameter bold: The heading in the page's strong ink ("Starts by itself") rather than the
    ///   quiet one ("About these numbers").
    init(
        _ title: String,
        bold: Bool = false,
        isOpen: Bool,
        toggle: @escaping @MainActor () -> Void,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.bold = bold
        self.isOpen = isOpen
        self.toggle = toggle
        self.content = content()
    }

    var body: some View {
        let palette = theme.palette(scheme)
        VStack(alignment: .leading, spacing: 4) {
            Button {
                toggle()
            } label: {
                Text(verbatim: title)
                    .font(Font.system(size: 12, weight: bold ? .semibold : .regular))
                    .foregroundStyle(bold ? palette.ink.color : palette.ink2.color)
                    .lineLimit(1)
                    .padding(.vertical, 3)
                    .frame(minHeight: 24, alignment: .leading)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(title)
            .accessibilityValue(isOpen ? "expanded" : "collapsed")
            if isOpen {
                content
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
