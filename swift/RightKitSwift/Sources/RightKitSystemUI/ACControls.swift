import Foundation
import RightKitHub
import SwiftUI

// Small controls shared by the Apps and Cleanup sections. They are built only from the controls the
// hub may use (docs/swiftui-hub.md): Text, Image, shapes, stacks, Button, TextField, HubLayered. The
// `AC` prefix keeps the names clear of the other sections' helpers in this module.

/// Words that depend on the platform: the Mac moves files to the Trash, Windows to the Recycle Bin.
enum ACPlatform {
    #if os(Windows)
    static let isWindows = true
    #else
    static let isWindows = false
    #endif

    /// "Trash" on the Mac, "Recycle Bin" on Windows.
    static var trash: String { isWindows ? "Recycle Bin" : "Trash" }
}

/// Shared wording and formatting.
enum ACFormat {
    /// "3 items", "1 item".
    static func items(_ count: Int) -> String {
        "\(count) item\(count == 1 ? "" : "s")"
    }

    /// The home folder as "~": `/Users/demo/Library` becomes `~/Library`.
    static func shortPath(_ path: String) -> String {
        path.replacingOccurrences(of: "^/Users/[^/]+", with: "~", options: .regularExpression)
    }

    /// "just now", "5 min ago", "2 h ago", or the date for anything older (the page's `ago`).
    static func ago(_ seconds: Int64, now: Date = Date()) -> String {
        let elapsed = max(0, now.timeIntervalSince1970 - Double(seconds))
        if elapsed < 90 { return "just now" }
        if elapsed < 3600 { return "\(Int((elapsed / 60).rounded())) min ago" }
        if elapsed < 86400 { return "\(Int((elapsed / 3600).rounded())) h ago" }
        return monthDay(seconds)
    }

    /// "Oct 15": a day without its year.
    static func monthDay(_ seconds: Int64) -> String {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("MMMd")
        return formatter.string(from: Date(timeIntervalSince1970: Double(seconds)))
    }

    /// "Oct 15, 2026 at 3:04 PM": a history entry's time.
    static func dateTime(_ seconds: Int64) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: Date(timeIntervalSince1970: Double(seconds)))
    }

    /// The message of an engine failure, or the error's description.
    static func describe(_ error: any Error) -> String {
        if let engine = error as? EngineError { return engine.message }
        return String(describing: error)
    }
}

/// Which colour a piece of text takes.
enum ACInk {
    case primary
    case secondary
    case tertiary
    case accent
    case ok
    case warn
    case bad
}

/// Which size and weight a piece of text takes (the hub's type scale).
enum ACStyle {
    case page
    case title
    case body
    case label
    case caption
    case small
    case micro
    case stat
}

/// Text in the hub's type scale and palette. `lines` limits the line count; nil wraps freely.
struct ACText: View {
    private let text: String
    private let style: ACStyle
    private let ink: ACInk
    private let lines: Int?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(_ text: String, _ style: ACStyle = .body, ink: ACInk = .primary, lines: Int? = nil) {
        self.text = text
        self.style = style
        self.ink = ink
        self.lines = lines
    }

    private var spec: HubFontSpec {
        switch style {
        case .page: return theme.type.pageTitle
        case .title: return theme.type.title
        case .body: return theme.type.body
        case .label: return theme.type.label
        case .caption: return theme.type.caption
        case .small: return theme.type.small
        case .micro: return theme.type.micro
        case .stat: return theme.type.stat
        }
    }

    private var color: Color {
        let palette = theme.palette(scheme)
        switch ink {
        case .primary: return palette.ink.color
        case .secondary: return palette.ink2.color
        case .tertiary: return palette.ink3.color
        case .accent: return palette.accent.color
        case .ok: return palette.ok.color
        case .warn: return palette.warn.color
        case .bad: return palette.bad.color
        }
    }

    var body: some View {
        Text(verbatim: text)
            .font(spec.font)
            .foregroundStyle(color)
            .lineLimit(lines)
    }
}

/// The ink that goes with a tone.
func acInk(for tone: HubTone) -> ACInk {
    switch tone {
    case .neutral: return .primary
    case .accent: return .accent
    case .ok: return .ok
    case .warn: return .warn
    case .bad: return .bad
    }
}

/// The soft fill that goes with a tone.
func acSoftFill(for tone: HubTone, in palette: HubPalette) -> Color {
    switch tone {
    case .neutral: return palette.fill2.color
    case .accent: return palette.accentSoft.color
    case .ok: return palette.okSoft.color
    case .warn: return palette.warnSoft.color
    case .bad: return palette.badSoft.color
    }
}

/// How a button looks.
enum ACButtonTone {
    case secondary
    case primary
    case danger
}

/// A small push button: a rounded fill with a title. Disabled buttons dim.
struct ACButton: View {
    private let title: String
    private let tone: ACButtonTone
    private let enabled: Bool
    private let accessibility: String?
    private let action: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    /// - Parameters:
    ///   - accessibility: What a screen reader says when the title alone is not enough.
    init(
        _ title: String,
        tone: ACButtonTone = .secondary,
        enabled: Bool = true,
        accessibility: String? = nil,
        action: @escaping @MainActor () -> Void
    ) {
        self.title = title
        self.tone = tone
        self.enabled = enabled
        self.accessibility = accessibility
        self.action = action
    }

    private var palette: HubPalette { theme.palette(scheme) }

    private var fill: Color {
        switch tone {
        case .secondary: return palette.fill2.color
        case .primary: return palette.accent.color
        case .danger: return palette.danger.color
        }
    }

    private var textColor: Color {
        switch tone {
        case .secondary: return palette.ink.color
        case .primary: return palette.accentInk.color
        case .danger: return palette.accentInk.color
        }
    }

    var body: some View {
        Button {
            action()
        } label: {
            HubLayered {
                RoundedRectangle(cornerRadius: theme.radii.control).fill(fill)
                RoundedRectangle(cornerRadius: theme.radii.control).stroke(palette.line.color, lineWidth: 1)
                Text(verbatim: title)
                    .font(theme.type.label.font)
                    .foregroundStyle(textColor)
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

/// A small status pill: "Running", "Safe", "Update available 1.2".
struct ACBadge: View {
    private let text: String
    private let tone: HubTone
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(_ text: String, tone: HubTone = .neutral) {
        self.text = text
        self.tone = tone
    }

    var body: some View {
        let palette = theme.palette(scheme)
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.pill).fill(acSoftFill(for: tone, in: palette))
            ACText(text, .micro, ink: tone == .neutral ? .secondary : acInk(for: tone), lines: 1)
                .padding(.horizontal, 7)
                .padding(.vertical, 2)
        }
        // A badge keeps its width on Windows' wider font; the name beside it gives way first.
        .layoutPriority(1)
    }
}

/// The three states of a checkbox.
enum ACCheckState {
    case off
    case on
    case mixed
}

/// A checkbox drawn as an icon button. Its label names what it selects.
struct ACCheckbox: View {
    private let state: ACCheckState
    private let label: String
    private let enabled: Bool
    private let action: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(state: ACCheckState, label: String, enabled: Bool = true, action: @escaping @MainActor () -> Void) {
        self.state = state
        self.label = label
        self.enabled = enabled
        self.action = action
    }

    /// The mark inside a filled box, or nil for an empty one. Both names are in SwiftRight's symbol map.
    private var mark: String? {
        switch state {
        case .off: return nil
        case .on: return "checkmark"
        case .mixed: return "minus"
        }
    }

    /// The box, drawn rather than named: `checkmark.square.fill` reaches Windows as a plain filled
    /// square without its check, while `checkmark` and `minus` draw. The empty box is an outline.
    @ViewBuilder
    private func box(_ palette: HubPalette) -> some View {
        if let mark {
            HubLayered {
                RoundedRectangle(cornerRadius: 4).fill(palette.accent.color)
                Image(systemName: mark)
                    .font(Font.system(size: 10, weight: .semibold))
                    .foregroundStyle(palette.accentInk.color)
                    .frame(width: 15, height: 15)
            }
            .frame(width: 15, height: 15)
        } else {
            RoundedRectangle(cornerRadius: 4)
                .stroke(palette.ink3.color, lineWidth: 1.25)
                .frame(width: 15, height: 15)
        }
    }

    private var valueText: String {
        switch state {
        case .off: return "not selected"
        case .on: return "selected"
        case .mixed: return "partly selected"
        }
    }

    var body: some View {
        let palette = theme.palette(scheme)
        Button {
            action()
        } label: {
            box(palette)
                .frame(width: 28, height: 28)
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.45)
        .accessibilityLabel(label)
        .accessibilityValue(valueText)
    }
}

/// One option of an `ACSegmented`.
struct ACSegmentOption<Value: Hashable & Sendable>: Identifiable, Sendable {
    let value: Value
    let label: String

    var id: Value { value }
}

/// A row of buttons that picks one value, in place of a segmented Picker.
struct ACSegmented<Value: Hashable & Sendable>: View {
    private let options: [ACSegmentOption<Value>]
    private let selection: Binding<Value>
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(options: [ACSegmentOption<Value>], selection: Binding<Value>) {
        self.options = options
        self.selection = selection
    }

    var body: some View {
        let palette = theme.palette(scheme)
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.fill2.color)
            HStack(spacing: 2) {
                ForEach(options) { option in
                    let chosen = option.value == selection.wrappedValue
                    Button {
                        selection.wrappedValue = option.value
                    } label: {
                        HubLayered {
                            RoundedRectangle(cornerRadius: theme.radii.control)
                                .fill(chosen ? palette.panel.color : Color.clear)
                            Text(verbatim: option.label)
                                .font(theme.type.label.font)
                                .foregroundStyle(chosen ? palette.ink.color : palette.ink2.color)
                                .lineLimit(1)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 5)
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(chosen ? .isSelected : [])
                    // Longest label first, so none is cut when the row only just fits (Fit.swift).
                    .layoutPriority(Fit.segment(option.label))
                }
            }
            .padding(2)
        }
        // The whole control keeps its width ("Unused 90+ days" never wraps); the summary beside it gives way.
        .layoutPriority(2)
    }
}

/// A notice box above or inside a section's content: a soft fill with a hairline border.
struct ACNotice<Content: View>: View {
    private let tone: HubTone
    private let content: Content
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(tone: HubTone = .neutral, @ViewBuilder content: () -> Content) {
        self.tone = tone
        self.content = content()
    }

    var body: some View {
        let palette = theme.palette(scheme)
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill(acSoftFill(for: tone, in: palette))
            RoundedRectangle(cornerRadius: theme.radii.item).stroke(palette.hairline.color, lineWidth: 1)
            VStack(alignment: .leading, spacing: 6) {
                content
            }
            .padding(.horizontal, theme.spacing.md)
            .padding(.vertical, theme.spacing.sm)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A notice that is only a sentence.
struct ACNoticeText: View {
    private let text: String
    private let tone: HubTone

    init(_ text: String, tone: HubTone = .neutral) {
        self.text = text
        self.tone = tone
    }

    var body: some View {
        ACNotice(tone: tone) {
            ACText(text, .caption, ink: acInk(for: tone))
        }
    }
}

/// A header you can open: a chevron beside the header, and content under it while open. The owner
/// holds the open state, so a list of these keeps it in its model.
struct ACDisclosure<Header: View, Content: View>: View {
    private let isOpen: Bool
    private let toggle: @MainActor () -> Void
    private let header: Header
    private let content: Content
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(
        isOpen: Bool,
        toggle: @escaping @MainActor () -> Void,
        @ViewBuilder header: () -> Header,
        @ViewBuilder content: () -> Content
    ) {
        self.isOpen = isOpen
        self.toggle = toggle
        self.header = header()
        self.content = content()
    }

    var body: some View {
        let palette = theme.palette(scheme)
        VStack(alignment: .leading, spacing: 0) {
            Button {
                toggle()
            } label: {
                HubLayered {
                    HubHitArea()
                    HStack(alignment: .top, spacing: 6) {
                        Image(systemName: isOpen ? "chevron.down" : "chevron.right")
                            .foregroundStyle(palette.ink2.color)
                            .frame(width: 14, height: 18)
                        header
                        Spacer(minLength: 0)
                    }
                    .padding(.vertical, 5)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .buttonStyle(.plain)
            .accessibilityValue(isOpen ? "expanded" : "collapsed")
            if isOpen {
                content
                    .padding(.leading, 20)
                    .padding(.bottom, 6)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        // Take the row's spare width explicitly, so a size beside it stays at the trailing edge.
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A search box: a magnifier and a text field in a rounded outline.
struct ACSearchField: View {
    private let prompt: String
    private let text: Binding<String>
    /// The box shows the accent ring while the field has the keyboard (#336).
    @FocusState private var focused: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(_ prompt: String, text: Binding<String>) {
        self.prompt = prompt
        self.text = text
    }

    var body: some View {
        let palette = theme.palette(scheme)
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.field).fill(palette.fill.color)
            RoundedRectangle(cornerRadius: theme.radii.field)
                .stroke(focused ? palette.accent.color : palette.line.color, lineWidth: focused ? 2 : 1)
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(palette.ink3.color)
                TextField(prompt, text: text)
                    .textFieldStyle(.plain)
                    .focused($focused)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
