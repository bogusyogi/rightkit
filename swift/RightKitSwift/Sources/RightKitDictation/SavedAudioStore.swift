import Foundation

/// Keeps completed captures outside temporary storage so a failed decode can
/// be retried after app relaunch. Retention is bounded to avoid unbounded disk
/// growth; transcript history remains the source of truth for text.
/// All mutable state is confined to `let`s set at init, so instances are safe
/// to call from any actor — the WAV encode is dispatched off-main by callers.
final class SavedAudioStore: @unchecked Sendable {
    static let shared = SavedAudioStore()
    static let maximumSavedCaptures = 50

    private let fileManager = FileManager.default
    private let root: URL

    init(root: URL? = nil) {
        self.root = root ?? fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("HeardRight/SavedAudio", isDirectory: true)
        try? fileManager.createDirectory(at: self.root, withIntermediateDirectories: true)
    }

    @discardableResult
    func save(samples: [Float], sessionID: String) -> URL? {
        guard !samples.isEmpty else { return nil }
        let url = root.appendingPathComponent("\(sessionID).wav")
        do {
            try PCMWavWriter.write(
                samples: samples,
                sampleRate: Int(WarmAudioCapture.targetSampleRate),
                to: url
            )
            prune()
            return url
        } catch {
            DogfoodTraceStore.record("saved_audio_write_failed", fields: [
                "session_id": sessionID,
                "error": error.localizedDescription
            ])
            return nil
        }
    }

    func url(for sessionID: String) -> URL {
        root.appendingPathComponent("\(sessionID).wav")
    }

    func delete(sessionID: String) {
        try? fileManager.removeItem(at: url(for: sessionID))
    }

    func prune() {
        guard let urls = try? fileManager.contentsOfDirectory(
            at: root,
            includingPropertiesForKeys: [.contentModificationDateKey],
            options: [.skipsHiddenFiles]
        ) else { return }
        let sorted = urls.filter { $0.pathExtension == "wav" }.sorted {
            let lhs = modificationDate(for: $0)
            let rhs = modificationDate(for: $1)
            return lhs > rhs
        }
        for url in sorted.dropFirst(Self.maximumSavedCaptures) {
            try? fileManager.removeItem(at: url)
        }
    }

    private func modificationDate(for url: URL) -> Date {
        guard let values = try? url.resourceValues(forKeys: [.contentModificationDateKey]),
              let date = values.contentModificationDate else { return .distantPast }
        return date
    }
}
