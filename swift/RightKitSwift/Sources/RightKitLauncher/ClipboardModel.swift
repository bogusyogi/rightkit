import Foundation

// The clipboard manager's data model (docs/clipboard-spec.md section 2) and the settings the
// store reads. Types only: the store is `LauncherClipboard` (LauncherClipboard.swift and the
// `Clipboard*.swift` files beside it); the strip is `ClipboardStrip*.swift`.

/// A JSON value, so a row rewritten by this the app keeps the keys a newer one added (2.5).
enum ClipJSON: Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([ClipJSON])
    case object([String: ClipJSON])
}

/// 2.1 `kind`.
enum ClipKind: String, CaseIterable, Sendable {
    case text, link, colour, code, file, image, rich
}

/// 2.1 `lane`.
enum ClipLane: String, Sendable {
    case main, screenshot
}

/// 2.1 `sourceApp`: the bundle identifier and the display name.
struct ClipSourceApp: Equatable, Hashable, Sendable {
    var id: String
    var name: String
}

/// 2.4 `meta`: every key optional. `extra` keeps unknown keys.
struct ClipMeta: Equatable, Sendable {
    var chars: Int?
    var lines: Int?
    var language: String?
    var host: String?
    var scheme: String?
    var hex: String?
    var original: String?
    var count: Int?
    var names: [String]?
    var anyMissing: Bool?
    var width: Int?
    var height: Int?
    var format: String?
    var screenshot: Bool?
    var stackId: String?
    var laneLocked: Bool?
    var hasHtml: Bool?
    var hasRtf: Bool?
    var extra: [String: ClipJSON] = [:]
}

/// 2.1: one record per clipboard copy. Dates are Unix seconds. `Codable` is added in
/// ClipboardCoding.swift, by hand, so keys keep the spec's order and unknown keys survive.
struct ClipItem: Identifiable, Equatable, Sendable {
    var id: String
    var kind: ClipKind
    var preview: String
    var title: String?
    var sourceApp: ClipSourceApp?
    var createdAt: Double
    var lastUsedAt: Double
    var useCount: Int = 0
    var pinned: Bool = false
    var boards: [String] = []
    var size: Int = 0
    var digest: String = ""
    var lane: ClipLane = .main
    var meta = ClipMeta()
    var concealed: Bool = false
    /// Unknown item-level keys, kept when the row is rewritten.
    var extra: [String: ClipJSON] = [:]

    /// 2.2: the first line, trimmed, at most 120 characters: the launcher row's title.
    var firstLine: String {
        let line = preview.split(whereSeparator: { $0.isNewline }).first.map(String.init) ?? ""
        return String(line.trimmingCharacters(in: .whitespaces).prefix(120))
    }

    /// An item on a board counts as pinned for retention (2.1, section 4).
    var isProtected: Bool { pinned || !boards.isEmpty }
}

/// Section 6: one pinboard. `colour` is `#RRGGBB`.
struct ClipBoard: Identifiable, Equatable, Sendable {
    var id: String
    var name: String
    var colour: String
    var order: Int
    var itemOrder: [String] = []
    var createdAt: Double
    var extra: [String: ClipJSON] = [:]

    /// 6: the eight palette colours, in order.
    static let palette: [(name: String, hex: String)] = [
        ("Blue", "#5E9BFF"), ("Green", "#34C759"), ("Orange", "#FF9F0A"), ("Red", "#FF453A"),
        ("Purple", "#BF5AF2"), ("Pink", "#FF6482"), ("Teal", "#40C8E0"), ("Grey", "#8E8E93"),
    ]
    static let maxBoards = 20
    static let maxItemsPerBoard = 1000
}

/// 5.3: the date filter.
enum ClipDateFilter: Equatable, Sendable {
    case any, today, yesterday, last7, last30
    /// Inclusive local days.
    case range(from: Date, to: Date)
}

/// 5.1 and 5.3: everything a query narrows by, besides its terms. The query prefixes
/// (`type:`, `app:`, `board:`, `is:pinned`, `before:`, `after:`) set the same fields.
struct ClipFilters: Equatable, Sendable {
    /// Empty means every kind.
    var kinds: Set<ClipKind> = []
    /// Source app ids (from the app picker). Empty means every app.
    var apps: Set<String> = []
    /// From an `app:` prefix: a fragment matched against the app's id or name, case-insensitive.
    var appTerms: [String] = []
    var date: ClipDateFilter = .any
    /// nil is History (everything); `"pinned"` is the Pinned view; otherwise a board id.
    var board: String?
    /// From a `board:` prefix: a fragment of a board's name.
    var boardTerms: [String] = []
    /// From `is:pinned`.
    var pinnedOnly = false
    /// From `before:` and `after:` (compared on `createdAt`, local day boundaries).
    var before: Date?
    var after: Date?
    /// Show only the members of one screenshot stack (opening a stack card).
    var stackId: String?

    var isEmpty: Bool {
        kinds.isEmpty && apps.isEmpty && appTerms.isEmpty && date == .any && boardTerms.isEmpty
            && !pinnedOnly && before == nil && after == nil && stackId == nil
    }
}

/// 5.1: a query split into its plain terms and the prefixes it recognised. `chips` are the
/// labels the search field shows for the prefixes (`type:link`, `app:chrome`, ...).
struct ClipQuery: Equatable, Sendable {
    var terms: [String]
    var chips: [String]
    var filters: ClipFilters
}

/// 9: why the history cannot be shown.
enum ClipLoadState: Equatable, Sendable {
    case ok
    /// `index.json` has a `version` above 1: do not write, do not capture (7.9).
    case newer
    /// `index.json` exists and could not be read (7.9).
    case unreadable
    /// Another the app holds `clipboard/.lock`: capture is refused (7.9).
    case lockedElsewhere
}

enum ClipCardSize: String, Sendable { case small, medium, large }
enum ClipPlacement: String, Sendable { case notch, bottom }
enum ClipPasteMode: String, Sendable { case paste, copy }

/// Section 8: the switches the store and the strip read. The app delegate builds this from
/// `Preferences` and sets `LauncherClipboard.settings` on every change. Defaults are the
/// spec's (8.1).
struct ClipSettings: Equatable, Sendable {
    /// `launcherClipboard`: the master switch. Off reads nothing.
    var enabled = false
    var stripEnabled = true
    /// 7.12 grammar: `cmd+shift+v`.
    var hotkey = "cmd+shift+v"
    var cardSize: ClipCardSize = .medium
    var placement: ClipPlacement = .notch
    var showNumbers = true
    var pasteMode: ClipPasteMode = .paste
    var plainDefault = false
    var captureImages = true
    var captureFiles = true
    var captureRich = true
    var maxItems = 500
    var maxMB = 200
    var maxAgeDays = 30
    var screenshotHours = 24
    var skipAutoGenerated = true
    var skipRemote = true
    var secretFilter = true
    /// `clipboardEncryption`: the history is sealed at rest with a Keychain key (ClipSeal, #152). On.
    var encrypt = true
    var excludedApps: [String] = ClipSettings.defaultExcludedApps
    var ignoredTypes: [String] = []
    var rememberBoard = false
    /// `clipboardHideFromCapture`: the strip and its preview stay out of screenshots and shares. On.
    var hideFromCapture = true
    /// `clipboardRestoreAfterPaste`: after a paste the previous clipboard goes back 1.5 s later. Off.
    var restoreAfterPaste = false
    /// `clipboardCopyBurst` (#387): copies made under 60 s apart show as dots on the Send cell and
    /// plain Cmd+V pastes them in copied order. On.
    var copyBurst = true

    /// 8.2, macOS.
    static let defaultExcludedApps = [
        "com.1password.1password", "com.agilebits.onepassword7", "com.agilebits.onepassword-osx",
        "com.bitwarden.desktop", "org.keepassxc.keepassxc", "com.lastpass.lastpass",
        "com.dashlane.dashlanephonefinal", "com.apple.keychainaccess", "com.apple.Passwords",
        "in.sinew.Enpass-Desktop", "com.nordsec.nordpass",
    ]
}

/// What a clear removes (docs/clipboard-spec.md 9): everything, the items copied in the last hour,
/// or the items copied more than a day ago. The two windows keep pinned and board items.
enum ClipClearScope: Sendable {
    case all, lastHour, olderThanDay

    /// The launcher row's title: the English source text, which also names the row.
    var title: String {
        switch self {
        case .all: return "Clear clipboard history"
        case .lastHour: return "Clear clipboard from the last hour"
        case .olderThanDay: return "Clear clipboard older than a day"
        }
    }

    /// The row's title as shown in the active language.
    var label: String {
        switch self {
        case .all: return L10n.t("Clear clipboard history")
        case .lastHour: return L10n.t("Clear clipboard from the last hour")
        case .olderThanDay: return L10n.t("Clear clipboard older than a day")
        }
    }
}
