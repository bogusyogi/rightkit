#if os(iOS)
import UIKit

/// What the phone may look at on the pasteboard without triggering the system
/// "Allow Paste" prompt (`has*` and `changeCount` are prompt-free; the content
/// accessors are not). Injected so tests never touch the real pasteboard.
protocol PasteboardReading {
    var changeCount: Int { get }
    var hasURLs: Bool { get }
    var hasStrings: Bool { get }
    var hasImages: Bool { get }
    var url: URL? { get }
    var string: String? { get }
    var image: UIImage? { get }
}

extension UIPasteboard: PasteboardReading {}

enum ClipboardContentKind: Equatable {
    case link, text, image
}

struct ClipboardOffer: Equatable {
    let changeCount: Int
    let kind: ClipboardContentKind
}

/// Decides whether the clipboard holds something new worth offering. The
/// pasteboard is never "empty of history", so the stored change count is the
/// only thing that stops the same content being offered on every foreground.
struct ClipboardOfferGate {
    static let defaultsKey = "send.lastOfferedChangeCount"

    let pasteboard: PasteboardReading
    let defaults: UserDefaults

    func pendingOffer() -> ClipboardOffer? {
        let count = pasteboard.changeCount
        if defaults.object(forKey: Self.defaultsKey) as? Int == count { return nil }
        if pasteboard.hasURLs { return ClipboardOffer(changeCount: count, kind: .link) }
        if pasteboard.hasStrings { return ClipboardOffer(changeCount: count, kind: .text) }
        if pasteboard.hasImages { return ClipboardOffer(changeCount: count, kind: .image) }
        return nil
    }

    /// Record an offer (or its dismissal) so it is not made again.
    func markHandled(_ offer: ClipboardOffer) {
        defaults.set(offer.changeCount, forKey: Self.defaultsKey)
    }
}

/// The buttons shown for an offer: Process always, then one Send per peer.
enum SendChoice: Hashable, Identifiable {
    case process
    case send(SendPeer)

    var id: String {
        switch self {
        case .process: return "process"
        case .send(let peer): return "send-\(peer.fingerprint)"
        }
    }

    var title: String {
        switch self {
        case .process: return "Process"
        case .send(let peer): return "Send to \(peer.alias)"
        }
    }

    static func choices(peers: [SendPeer]) -> [SendChoice] {
        [.process] + peers.sorted { $0.alias.localizedCaseInsensitiveCompare($1.alias) == .orderedAscending }.map(SendChoice.send)
    }
}
#endif
