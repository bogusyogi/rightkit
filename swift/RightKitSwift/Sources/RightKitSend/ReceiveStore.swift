#if os(iOS)
import Foundation

/// A scratchpad, not a history: an item leaves as soon as it is used or
/// dismissed, nothing is kept across launches, nothing is synced. In memory
/// only; file payloads live in `receivedDirectory`, which is emptied on launch
/// and per item on removal.
@MainActor
final class ReceiveStore: ObservableObject {
    /// Where engines place received file payloads.
    nonisolated static var receivedDirectory: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("ReceivedItems", isDirectory: true)
    }

    @Published private(set) var items: [ReceivedItem] = []
    @Published private(set) var offers: [IncomingOffer] = []
    /// Dismissable "Saved to Photos" confirmations. Not a history.
    @Published private(set) var notices: [SavedNotice] = []

    struct SavedNotice: Identifiable, Equatable {
        let id = UUID()
        enum Place { case photos, files }
        let place: Place
        let name: String
        let senderAlias: String
        /// Plain explanation when a chosen folder was not used.
        var message: String? = nil
    }

    init() {
        try? FileManager.default.removeItem(at: Self.receivedDirectory)
    }

    func add(_ item: ReceivedItem) {
        items.append(item)
        items.sort { $0.receivedAt > $1.receivedAt }
    }

    func addOffer(_ offer: IncomingOffer) {
        guard !offers.contains(where: { $0.id == offer.id }) else { return }
        offers.append(offer)
    }

    func removeOffer(_ id: UUID) {
        offers.removeAll { $0.id == id }
    }

    func addNotice(_ notice: SavedNotice) { notices.append(notice) }
    func removeNotice(_ id: UUID) { notices.removeAll { $0.id == id } }

    /// Used or dismissed: drop the item and its temp file.
    func remove(_ id: UUID) {
        guard let item = items.first(where: { $0.id == id }) else { return }
        switch item.kind {
        case .file(let url, _, _, _), .picture(let url, _): try? FileManager.default.removeItem(at: url)
        case .text, .link: break
        }
        items.removeAll { $0.id == id }
    }
}
#endif
