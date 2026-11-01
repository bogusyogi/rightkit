#if os(macOS)
import AppKit
import SwiftUI

// What the clipboard strip draws (docs/clipboard-spec.md section 7), as plain values. The
// live strip (`ClipboardStripController`) builds one from the store on every change; the
// picture renderer (`ClipboardStripPicture`) builds one from a fixture. One view draws both.

/// Section 7.2: sizes, in points at Medium, all multiplied by `k`, the notch's size scale
/// clamped to 0.8 to 1.25.
enum ClipStripMetrics {
    static func clamp(_ scale: CGFloat) -> CGFloat { min(max(scale, 0.8), 1.25) }

    /// The square card, before `k`: Small, Medium, Large (0.8, 1 and 1.25 of the toast width).
    static func cardSide(_ size: ClipCardSize) -> CGFloat {
        switch size {
        case .small: return 156
        case .medium: return 196
        case .large: return 244
        }
    }

    static func textLines(_ size: ClipCardSize) -> Int {
        switch size {
        case .small: return 7
        case .medium: return 9
        case .large: return 12
        }
    }

    static let gap: CGFloat = 10
    static let padding: CGFloat = 11
    static let corner: CGFloat = 18
    static let cardCorner: CGFloat = 12
    static let header: CGFloat = 30
    static let headerGap: CGFloat = 8
    static let footer: CGFloat = 18
    static let pill: CGFloat = 22
    /// The strip stays this far in from the ends of the display (7.1).
    static let screenMargin: CGFloat = 24
    static let searchWidth: CGFloat = 220

    /// The strip's depth away from the notch (its height on a horizontal edge), neck aside.
    static func across(vertical: Bool, size: ClipCardSize, k: CGFloat, footer hasFooter: Bool) -> CGFloat {
        let card = cardSide(size)
        if vertical { return (padding * 2 + card) * k }
        return (padding * 2 + header + headerGap + card + (hasFooter ? footer : 0)) * k
    }

    /// Its length along the notch: what the cards need, at least `minimum`, at most `limit`.
    static func length(count: Int, size: ClipCardSize, k: CGFloat, minimum: CGFloat, limit: CGFloat) -> CGFloat {
        let natural = (padding * 2 + CGFloat(count) * cardSide(size) + CGFloat(max(count - 1, 0)) * gap) * k
        return min(max(natural, minimum * k), limit)
    }
}

/// One board tab. `History` and `Pinned` are views, not boards (6).
struct ClipTab: Identifiable, Equatable {
    var id: String
    var title: String
    /// `#RRGGBB` for a board's dot; nil for the two views.
    var colour: String?
    var selected: Bool
    /// Being renamed or just created: the tab shows this text and the caret.
    var editing: String?
}

/// One card. A screenshot stack is one card with `stackCount` above 1.
struct ClipCardData: Identifiable {
    var item: ClipItem
    var stackCount = 1
    /// The ids of the items in a stack, newest first (one id when it is not a stack).
    var stackMembers: [String] = []
    /// A drawn stand-in for an image (fixtures): `gradient-blue`, `gradient-orange`,
    /// `screenshot-light`, `screenshot-dark`, `photo-green`.
    var art: String?
    var thumb: NSImage?
    /// `#RRGGBB` for the colour band and the icon stand-in.
    var appColour: String
    /// Up to three dots, one per board the item is on.
    var boardDots: [String] = []
    /// The first file's path, for its system icon.
    var iconPath: String?
    var id: String { item.id }
}

/// The filter popover's state (7.6).
struct ClipFilterPanel: Equatable {
    var kinds: Set<ClipKind> = []
    var apps: [ClipSourceApp] = []
    var selectedApps: Set<String> = []
    /// "any", "today", "yesterday", "last7", "last30" or "range".
    var date = "any"
}

/// The board picker (Cmd+B).
struct ClipBoardPicker: Equatable {
    var query = ""
    var rows: [Row] = []
    var selected = 0

    struct Row: Equatable, Identifiable {
        var id: String
        var title: String
        var colour: String?
        var ticked: Bool
        var isNew = false
    }
}

enum ClipPopover: Equatable {
    case filters(ClipFilterPanel)
    /// The context menu of the selected card, drawn by the picture renderer (live uses a native menu).
    case context
    /// A board's colour chooser: the board's id, its colour, and the hex field's text.
    case boardColour(board: String, colour: String, hex: String)
    case boardPicker(ClipBoardPicker)
}

/// The preview panel (Space).
struct ClipPreviewData: Equatable {
    var kind: ClipKind
    var text: String
    var title: String
    var imageSize: CGSize?
    var art: String?
    var colour: String?
    static func == (a: ClipPreviewData, b: ClipPreviewData) -> Bool {
        a.kind == b.kind && a.text == b.text && a.title == b.title && a.imageSize == b.imageSize
            && a.art == b.art && a.colour == b.colour
    }
}

/// Everything one picture of the strip needs.
struct ClipStripContent {
    var size: ClipCardSize = .medium
    /// The notch's size scale, already clamped.
    var k: CGFloat = 1
    /// A column (notch on the left or right edge) rather than a row.
    var vertical = false
    var tabs: [ClipTab] = []
    var query = ""
    var chips: [String] = []
    var searchOpen = false
    var querySelected = false
    var filtersActive = false
    /// The text an input method is composing, shown underlined after the query (#377).
    var composition = ""
    var cards: [ClipCardData] = []
    /// Index in `cards`; -1 for none.
    var selected = 0
    /// How far the cards are scrolled along the strip, in points at scale 1 (pictures only).
    var scrollOffset: CGFloat = 0
    var showNumbers = true
    var now = Date()
    var footer: String?
    /// The footer is the alert colour (capture blocked by the lock, a refused board name).
    var footerAlert = false
    var message: String?
    var detail: String?
    var stripAction: String?
    var popover: ClipPopover?
    var preview: ClipPreviewData?
    var highContrast = false
    /// Where keyboard focus is: `cards`, `tab:<id>`, `search` or `filter` (7.5, Tab and Shift+Tab).
    var focus = "cards"
    /// Total in the current result, for "N items" and the spoken position.
    var total = 0
    var calendar = Calendar.current

    var hasFooter: Bool { footer != nil }
}

/// What the strip's buttons and gestures call. All optional: pictures set none.
struct ClipStripActions {
    var select: (Int) -> Void = { _ in }
    var paste: (Int, Bool) -> Void = { _, _ in }
    var tab: (String) -> Void = { _ in }
    var openSearch: () -> Void = {}
    var openFilters: () -> Void = {}
    var newBoard: () -> Void = {}
    var filterKind: (ClipKind?, Bool) -> Void = { _, _ in }
    var filterApp: (String) -> Void = { _ in }
    var filterDate: (String) -> Void = { _ in }
    var colour: (String) -> Void = { _ in }
    var hexChanged: (String) -> Void = { _ in }
    var pickBoard: (Int) -> Void = { _ in }
    var cardMenu: (Int) -> [ClipMenuItem] = { _ in [] }
    var boardMenu: (String) -> [ClipMenuItem] = { _ in [] }
    var beginRename: (String) -> Void = { _ in }
    var openStack: (Int) -> Void = { _ in }
    var beginDrag: (Int) -> Void = { _ in }
    var stripAction: () -> Void = {}
    var moveBoard: (String, Int) -> Void = { _, _ in }
    /// An item dragged from the strip onto a board tab (item id, tab id).
    var dropOnTab: (String, String) -> Void = { _, _ in }
    /// A board tab dragged onto another tab (dragged board id, target tab id).
    var dropBoard: (String, String) -> Void = { _, _ in }
    /// An item dragged onto another card of a board (dragged id, target index).
    var dropOnCard: (String, Int) -> Void = { _, _ in }
}

/// One entry of a context menu, drawn natively live and as a facsimile in pictures.
struct ClipMenuItem: Identifiable {
    var id = UUID()
    var title = ""
    /// The shortcut shown beside the title.
    var key: Character?
    var modifiers: EventModifiers = []
    /// Spoken-and-drawn shortcut text for the facsimile ("⌘P").
    var shortcut = ""
    var checked = false
    var children: [ClipMenuItem]?
    var isDivider = false
    var action: () -> Void = {}

    static var divider: ClipMenuItem { ClipMenuItem(isDivider: true) }
}

// MARK: - Relative time (7.13)

enum ClipboardTime {
    /// "Just now", "N min", "N h", "Yesterday", a short weekday under seven days, else "d MMM"
    /// (with the year when it is not this one). `calendar` and `now` are injected so the
    /// pictures read the same on every run.
    static func relative(from then: Date, now: Date, calendar: Calendar = .current) -> String {
        let age = max(0, now.timeIntervalSince(then))
        if age < 60 { return L10n.t("Just now") }
        if age < 3600 { return L10n.t("\(Int(age / 60)) min") }
        if age < 86_400 { return L10n.t("\(Int(age / 3600)) h") }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now),
           calendar.isDate(then, inSameDayAs: yesterday) {
            return L10n.t("Yesterday")
        }
        let formatter = DateFormatter()
        formatter.locale = L10n.locale
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        if age < 7 * 86_400 {
            formatter.setLocalizedDateFormatFromTemplate("EEE")
        } else if calendar.component(.year, from: then) == calendar.component(.year, from: now) {
            formatter.setLocalizedDateFormatFromTemplate("d MMM")
        } else {
            formatter.setLocalizedDateFormatFromTemplate("d MMM y")
        }
        return formatter.string(from: then)
    }

    /// The spoken form: "5 minutes ago".
    static func spoken(from then: Date, now: Date) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.locale = L10n.locale
        formatter.unitsStyle = .full
        return formatter.localizedString(for: then, relativeTo: now)
    }
}

// MARK: - Card words

extension ClipKind {
    var title: String {
        switch self {
        case .text: return L10n.t("Text")
        case .link: return L10n.t("Link")
        case .colour: return L10n.t("Colour")
        case .code: return L10n.t("Code")
        case .file: return L10n.t("File")
        case .image: return L10n.t("Image")
        case .rich: return L10n.t("Rich text")
        }
    }

    var plural: String {
        switch self {
        case .text: return L10n.t("Text")
        case .link: return L10n.t("Links")
        case .colour: return L10n.t("Colours")
        case .code: return L10n.t("Code")
        case .file: return L10n.t("Files")
        case .image: return L10n.t("Images")
        case .rich: return L10n.t("Rich")
        }
    }

    /// The glyph in a card's header line; `rich` draws "Aa" instead.
    var symbol: String {
        switch self {
        case .text: return "text.alignleft"
        case .link: return "link"
        case .colour: return "paintpalette"
        case .code: return "chevron.left.forwardslash.chevron.right"
        case .file: return "doc"
        case .image: return "photo"
        case .rich: return "textformat"
        }
    }
}

extension ClipCardData {
    /// The footer's right-hand fact: a character count, or a file size.
    var sizeText: String {
        switch item.kind {
        case .text, .code, .rich:
            let chars = item.meta.chars ?? item.preview.count
            return L10n.t("\(chars) chars")
        case .link, .colour:
            return ""
        case .file, .image:
            return ByteCountFormatter.string(fromByteCount: Int64(item.size), countStyle: .file)
        }
    }

    /// "Link, example.com/roadmap, from Safari, 5 minutes ago, pinned, 3 of 120" (7.10).
    func spoken(position: Int, of total: Int, now: Date) -> String {
        var parts = [item.kind.title]
        switch item.kind {
        case .image:
            if let w = item.meta.width, let h = item.meta.height { parts.append("\(w) × \(h)") }
        case .link:
            parts.append(item.meta.host.map { $0 + ClipCardData.path(of: item.preview) } ?? item.preview)
        default:
            parts.append(item.firstLine)
        }
        if let app = item.sourceApp?.name, !app.isEmpty { parts.append(L10n.t("from \(app)")) }
        parts.append(ClipboardTime.spoken(from: Date(timeIntervalSince1970: item.lastUsedAt), now: now))
        if item.pinned { parts.append(L10n.t("pinned")) }
        if stackCount > 1 { parts.append(L10n.t("stack of \(stackCount)")) }
        parts.append(L10n.t("\(position) of \(total)"))
        return parts.joined(separator: ", ")
    }

    /// A link's path and query, without the host.
    static func path(of url: String) -> String {
        guard let parsed = URL(string: url), parsed.host != nil else { return "" }
        var path = parsed.path
        if path == "/" { path = "" }
        if let query = parsed.query { path += "?" + query }
        return path
    }
}

extension ClipSourceApp: Identifiable {}
#endif
