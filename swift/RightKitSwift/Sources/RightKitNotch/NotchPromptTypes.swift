import Foundation
import SwiftUI

/// **An update, as the notch offers it**: which version, a line of what is in
/// it, and how far along taking it up is.
struct UpdatePrompt: Equatable {
    enum Phase: Equatable {
        case available
        case downloading(Double?)
        case extracting(Double)
        case installing
        /// The copy in /Applications is a newer build than the running one.
        case restart
    }

    var version: String
    var notes: String
    var phase: Phase

    static func summary(of html: String) -> String {
        var text = html.replacingOccurrences(of: "<[^>]+>", with: " ", options: .regularExpression)
        for (entity, character) in [("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"),
                                    ("&quot;", "\""), ("&#39;", "'"), ("&nbsp;", " ")] {
            text = text.replacingOccurrences(of: entity, with: character)
        }
        return text.replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// What the notch said to the update it offered.
enum UpdateChoice: Equatable {
    case install, later, close, restart
}

/// What a card's buttons answer with. The root view routes each to the callback
/// its source always had.
enum NoticeChoice: Equatable {
    case update(UpdateChoice)
    case disk(DiskImageChoice)
    /// A notice, closed.
    case dismissNotice
    /// The hub, opened on this section, and the notice closed.
    case openHub(String)
}

extension NoticePrompt {
    /// The card's own size, the tail aside: 196pt wide, as tall as `NoticeLayout` measures.
    static func size(for prompt: NoticePrompt) -> CGSize {
        CGSize(width: NoticeMetrics.width, height: NoticeLayout(prompt).height)
    }
}

/// **What one toast says**: a glyph, a title, a detail, a warning line,
/// actions, a progress bar, device rows, and how long it lives.
struct NoticePrompt: Equatable {
    enum Glyph: Equatable {
        /// Pulse's own icon.
        case app
        /// A file's or an app's Finder icon.
        case file(String)
        case provider(ProviderGlyph)
        case symbol(String)
        /// A check in green: done.
        case done
        /// A "!" in amber: a problem.
        case problem
        /// Working: the system's small spinner.
        case working
    }

    struct Action: Equatable {
        var choice: NoticeChoice
        var label: String
        var symbol: String
        /// What it answers with when the checkbox is ticked.
        var choiceWhenChecked: NoticeChoice?
        /// A spinner in place of the symbol, and no press: looking again.
        var busy = false
    }

    struct Checkbox: Equatable {
        var label: String
        /// Ticked when the card appears; only the view pictures ask for it.
        var startsTicked = false
    }

    /// A device list: the rows, and what to say when there are none yet.
    struct Rows: Equatable {
        var items: [SendCardContent.Row]
        var emptyText: String
    }

    /// How long a toast lasts before it goes by itself, from when it came up.
    struct Life: Equatable {
        var seconds: TimeInterval
        var expiresAt: Date

        init(seconds: TimeInterval, from start: Date = Date()) {
            self.seconds = seconds
            self.expiresAt = start.addingTimeInterval(seconds)
        }
    }

    var glyph: Glyph
    var title: String
    var detail = ""
    /// A line of consequence, in the warning colour.
    var warning: String?
    /// A problem: the detail is in the warning colour too.
    var isProblem = false
    /// A limit reached: the warning line is red.
    var isAlert = false
    var actions: [Action] = []
    var checkbox: Checkbox?
    /// A bar: its share, or nil for none. (A wait with no share draws none.)
    var progress: Double?
    var rows: Rows?
    /// The received-files shelf (`sendReceivedShelf`), drawn under the detail.
    var shelf: [SendShelfItem]?
    var life: Life?
    /// The × at the end, or nil for a card that cannot be closed now.
    var closeChoice: NoticeChoice?
    var closeHelp = ""
}

/// What the notch's disk image card can be asked to do.
enum DiskImageChoice: Equatable, Sendable {
    case install, replace, quitAndUpdate, undo, openInstalled, openInstaller, showImage, cancel, dismiss
    /// Nearby sharing's device list: send to this device (its fingerprint), or look again.
    case sendTo(String), refresh
    /// A received message: copy it, or open it when it is a link.
    case copyText, openLink
    /// Accept an incoming request and set that device to Allow (the card's ticked checkbox).
    case installAlways
    /// A note's one action ("Open Settings" on the clipboard's off toast).
    case noteAction
    /// A device pairing request on the host's notch: approve or reject the joiner (`DevicePairingCard`).
    case approvePairing, rejectPairing
    /// The one-time question before Pulse's Chat plugin goes into an agent app that asks first
    /// (Muse): Allow, or Not now (`ChatPluginInstaller`).
    case allowChatPlugin, declineChatPlugin
    /// The new-Claude-account offer (`ClaudeAccountOffer`): bring this computer's chats in, or keep them apart.
    case bringChatsIn, keepChatsApart
    /// A browser sign-in request on a paired computer (`BrowserLoginAlert`, bogusyogi/pulse#460): approve
    /// it with the owner check, deny it, or open the hub's live view of a session that needs the owner.
    case browserApprove(String), browserDeny(String), browserOpen(String)
}

/// **A disk image, as the notch shows it**: an app installed on its own (with
/// Undo), a question about one that could not be, or how an install went.
struct DiskImagePrompt: Equatable {
    enum Style: Equatable {
        /// A question: the buttons answer it.
        case ask
        /// Copying and checking.
        case working
        /// Done.
        case done
        /// Done with a caveat, or failed.
        case problem
        /// Stopped by the user: neutral, not a problem.
        case stopped
    }

    struct Checkbox: Equatable {
        var label: String
        var primaryWhenChecked: DiskImageChoice
        /// Ticked when the card appears; only the view pictures ask for it.
        var startsTicked = false
    }

    struct Button: Equatable {
        var choice: DiskImageChoice
        var label: String
    }

    /// The path whose icon is drawn: the app, or the volume.
    var iconPath: String
    var title: String
    var detail: String
    /// A line of consequence, in the warning colour.
    var warning: String?
    var style: Style
    var primary: Button?
    var secondary: Button?
    /// An optional checkbox beside the buttons (incoming request card only). When it is ticked,
    /// pressing the primary button reports `primaryWhenChecked` instead of the primary's own choice.
    var checkbox: Checkbox?
    /// Nearby sharing's device list or transfer, drawn in place of the icon layout
    /// and anchored to the Send ring — see `SendCardContent`.
    var send: SendCardContent?
    /// Hangs from the Send ring, with its tail on it, whatever the card holds:
    /// every nearby sharing card (received, saved, note) sets this.
    var sendAnchored = false
    var hangsFromSend: Bool { send != nil || sendAnchored }
    /// How long the card lasts before its owner takes it down by itself, from
    /// when the prompt was made: shown as the ring round the ×. Nil for a card
    /// that stays until answered. The owner still schedules the deadline; this
    /// only lets the card show it.
    var life: NoticePrompt.Life?

    init(iconPath: String, title: String, detail: String, warning: String? = nil,
         style: Style, primary: Button? = nil, secondary: Button? = nil,
         send: SendCardContent? = nil, checkbox: Checkbox? = nil, life: TimeInterval? = nil) {
        self.life = life.map { NoticePrompt.Life(seconds: $0) }
        self.checkbox = checkbox
        self.send = send
        self.iconPath = iconPath
        self.title = title
        self.detail = detail
        self.warning = warning
        self.style = style
        self.primary = primary
        self.secondary = secondary
    }
}

/// **A notch being carried, and set down**: when it was taken hold of, let
/// go of, and landed — what the settings handle's end of it shows, worked out
/// from these times alone.
struct Carry: Equatable {
    var at: Date
    /// Taken hold of by its dots, out beside the settings button — rather
    /// than by ⌥-drag, with nothing out.
    var fromHover: Bool
    var releasedAt: Date?
    var landedAt: Date?

    /// Long enough after landing for everything it shows to have settled.
    static let settles: TimeInterval = 0.5
}

/// What nearby sharing's notch card shows, carried on a `DiskImagePrompt`:
/// the devices to pick from (a click sends), or the transfer in flight. It is
/// drawn by `NoticeCard` (rows, a bar and actions, like every other toast) and
/// hangs from the Send ring, not the notch's middle.
struct SendCardContent: Equatable {
    struct Row: Equatable, Identifiable {
        var id: String          // the device's fingerprint
        var alias: String
        var model: String
        var symbol: String      // SF Symbol for its device type
        var route: String = ""  // "Nearby" or "Relay" for a paired device, else empty (#469)
    }

    struct Transfer: Equatable {
        enum State: Equatable { case waiting, active, done, problem }
        var state: State
        var symbol: String
        /// 0...1 while bytes move; nil while waiting.
        var fraction: Double?
        var canCancel: Bool
    }

    var rows: [Row]
    var scanning: Bool
    var transfer: Transfer?
    /// The received-files shelf (`sendReceivedShelf`), newest first; empty hides it.
    var shelf: [SendShelfItem] = []
}

/// One received file on the Send card's shelf: where it is on disk. Its name is the file's.
struct SendShelfItem: Equatable, Identifiable {
    var id: String { path }
    let path: String
    /// The device it came from; empty when unknown (a shelf kept by an older build).
    var from: String = ""
    /// Seconds since 1970 when it arrived; nil when unknown.
    var receivedAt: Double?
    var name: String { URL(fileURLWithPath: path).lastPathComponent }

    /// "1.2 MB · from Dell · 3 min ago", leaving out whatever is unknown.
    var detail: String {
        var parts: [String] = []
        if let bytes = (try? URL(fileURLWithPath: path).resourceValues(forKeys: [.fileSizeKey]))?.fileSize {
            parts.append(ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file))
        }
        if !from.isEmpty { parts.append(L10n.t("from \(from)")) }
        if let receivedAt { parts.append(ElapsedCopy.ago(since: Date(timeIntervalSince1970: receivedAt))) }
        return parts.joined(separator: " · ")
    }
}
