#if os(iOS)
import RightKitUI
import SwiftUI

/// Response-specific roles for the phone (#505), mapped onto the existing Theme. A local adapter, not a second
/// design system: colours come from Theme, text uses native Dynamic Type styles.
enum ResponseStyle {
    // Spacing (points).
    static let gutter: CGFloat = 20
    static let sectionGap: CGFloat = 24
    static let paragraphGap: CGFloat = 12
    static let headingGap: CGFloat = 8
    static let rowGap: CGFloat = 8
    static let groupPadding: CGFloat = 16
    static let panelPadding: CGFloat = 20
    static let groupRadius: CGFloat = 20
    static let minTarget: CGFloat = 44
    /// Loaded row groups collapse after this many rows.
    static let collapsedRows = 5

    // Colours: page, group and inset surfaces and the ink roles.
    static let background = Theme.page
    static let group = Theme.card
    static let inset = Theme.raised
    static let text = Theme.ink
    static let secondary = Theme.ink2
    static let metadata = Theme.ink3
    static let separator = Theme.hair

    // Type: native text styles, so everything scales with Dynamic Type.
    static let body = Font.body
    static let metadataFont = Font.footnote
    /// Optional lead paragraph: a short first paragraph of an answer that has more after it.
    static let lead = Font.title3
    static let maxLeadCharacters = 140
    static let code = Font.system(.callout, design: .monospaced)

    static func heading(level: Int) -> Font {
        switch level {
        case ...1: .title3.weight(.semibold)
        case 2: .headline.weight(.semibold)
        default: .body.weight(.semibold)
        }
    }

    /// Tone for a host-supplied status word. The word itself is always shown beside the colour.
    static func tone(forStatus status: String) -> StatusTone {
        let word = status.lowercased()
        if word.contains("offline") || word.contains("unavailable") { return .idle }
        if word.contains("refused") || word.contains("failed") { return .bad }
        if word.contains("online") || word.contains("nearby") || word.contains("direct") { return .ok }
        if word.contains("relay") { return .info }
        if word.contains("busy") || word.contains("connecting") || word.contains("checking") { return .warn }
        return .idle
    }

    /// Tone for a send receipt state. Only a confirmed "delivered" is green; everything uncertain says so.
    static func tone(forReceipt state: String) -> StatusTone {
        switch state.lowercased() {
        case "delivered": .ok
        case "sent": .info
        case "queued", "held", "unknown", "uncertain": .warn
        case "refused", "unsupported", "failed": .bad
        default: .idle
        }
    }
}

extension View {
    /// A structured group (several values forming one object): surface contrast, no border, no shadow.
    func responseGroup(padding: CGFloat = ResponseStyle.groupPadding) -> some View {
        self.padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(ResponseStyle.group,
                        in: RoundedRectangle(cornerRadius: ResponseStyle.groupRadius, style: .continuous))
    }
}
#endif
