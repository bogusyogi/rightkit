import Foundation
import RightKitPresentation

/// What the launcher's answer panel draws for one answer: the structural document, once it has
/// been built off the main thread. `document` is nil while it is being built (the view shows the
/// plain text meanwhile), and the provider label is shown only when the engine supplied one.
struct LauncherAnswerDisplay: Equatable {
    let panelID: String
    let revision: UInt64
    var document: ResponseDocument?
    let provider: String?
}

/// Answer text to `ResponseDocument`, parsed once per panel identity and revision and kept only in
/// memory. Parsing runs off the main thread, and a result is published only if the caller says the
/// panel and answer are still the ones it was built for. Nothing is written to the saved history.
@MainActor
final class LauncherResponseAdapter {
    private struct Key: Hashable {
        let panelID: String
        let revision: UInt64
    }

    /// A few documents are kept so reopening the same saved answer does not parse it again.
    private static let capacity = 8
    private var cache: [Key: ResponseDocument] = [:]
    private var order: [Key] = []
    private var inflight: [Key: Task<Void, Never>] = [:]

    /// The revision of an answer: a stable digest of its text (FNV-1a over UTF-8), so the same
    /// saved answer reopens to the same revision and an edited one does not.
    static func revision(of answer: String) -> UInt64 {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in answer.utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 0x0000_0100_0000_01b3
        }
        return hash
    }

    func cached(panelID: String, revision: UInt64) -> ResponseDocument? {
        cache[Key(panelID: panelID, revision: revision)]
    }

    /// Builds the document off the main thread and hands it to `publish` on the main actor.
    /// `isCurrent` is asked just before publishing; false drops the result.
    func prepare(panelID: String, answer: String, revision: UInt64,
                 isCurrent: @escaping @MainActor () -> Bool,
                 publish: @escaping @MainActor (ResponseDocument) -> Void) {
        let key = Key(panelID: panelID, revision: revision)
        guard inflight[key] == nil else { return }
        let task = Task.detached(priority: .userInitiated) { [weak self] in
            let document = ResponsePlan.compose(
                id: panelID, revision: revision, canonicalText: answer,
                projections: [.text(TextDocument.parse(answer))])
            guard !Task.isCancelled else { return }
            await MainActor.run {
                guard let self else { return }
                self.inflight[key] = nil
                self.store(document, for: key)
                if isCurrent() { publish(document) }
            }
        }
        inflight[key] = task
    }

    /// Drops work in flight (the panel closed or moved on). Cached documents stay for a reopen.
    func cancelPending() {
        inflight.values.forEach { $0.cancel() }
        inflight.removeAll()
    }

    private func store(_ document: ResponseDocument, for key: Key) {
        cache[key] = document
        order.removeAll { $0 == key }
        order.append(key)
        while order.count > Self.capacity {
            cache[order.removeFirst()] = nil
        }
    }
}
