#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation

/// Durable hand-off for a model download that finished while nobody was waiting for it.
///
/// iOS deletes the delegate's `location` file as soon as `didFinishDownloadingTo` returns, so a download
/// that completes while the app is not running must be moved synchronously into a staging folder, with a
/// marker saying which model it is. The next launch (or the background-session event handler) adopts it
/// into the normal archive path, where the installer verifies the digest.
public struct ModelDownloadStaging: Sendable, Equatable {
    public let root: URL

    private struct Marker: Codable {
        let model: String
        let bytes: Int64
        let stagedAt: Date
    }

    public init(root: URL) {
        self.root = root
    }

    /// `<app group container>/ModelDownloadStaging` when the app group is available, else
    /// `<fallback>/ModelDownloadStaging`. A staging folder in the app group is readable by the app after a
    /// background relaunch; the fallback is for builds without the group.
    public static func defaultRoot(appGroupIdentifier: String?, fallback: URL) -> URL {
        if let appGroupIdentifier,
           let container = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroupIdentifier) {
            return container.appendingPathComponent("ModelDownloadStaging", isDirectory: true)
        }
        return fallback.appendingPathComponent("ModelDownloadStaging", isDirectory: true)
    }

    private func archive(_ model: String) -> URL { root.appendingPathComponent("\(model).tar.gz") }
    private func marker(_ model: String) -> URL { root.appendingPathComponent("\(model).json") }

    /// Synchronous: call it inside `didFinishDownloadingTo`. Returns false, staging nothing, for a
    /// non-2xx response or a failed move.
    @discardableResult
    public func stageCompleted(location: URL, model: String, response: URLResponse?) -> Bool {
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else { return false }
        let manager = FileManager.default
        do {
            try manager.createDirectory(at: root, withIntermediateDirectories: true)
            ModelStoreLayout.excludeFromBackup(root)
            try? manager.removeItem(at: marker(model))
            try? manager.removeItem(at: archive(model))
            try manager.moveItem(at: location, to: archive(model))
            let size = (try manager.attributesOfItem(atPath: archive(model).path)[.size] as? NSNumber)?.int64Value ?? 0
            let data = try JSONEncoder().encode(Marker(model: model, bytes: size, stagedAt: Date()))
            try data.write(to: marker(model), options: .atomic)
            return true
        } catch {
            try? manager.removeItem(at: archive(model))
            try? manager.removeItem(at: marker(model))
            return false
        }
    }

    /// The staged archive for `model`, only when its marker agrees on size.
    public func stagedArchive(for model: String) -> URL? {
        guard let data = try? Data(contentsOf: marker(model)),
              let info = try? JSONDecoder().decode(Marker.self, from: data),
              info.model == model, info.bytes > 0 else { return nil }
        let url = archive(model)
        let size = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? NSNumber)?.int64Value
        return size == info.bytes ? url : nil
    }

    public func discard(model: String) {
        try? FileManager.default.removeItem(at: archive(model))
        try? FileManager.default.removeItem(at: marker(model))
    }

    /// Moves the staged archive to `destination` (replacing any file there) and clears its marker.
    /// Returns false when nothing valid is staged.
    public func adopt(model: String, to destination: URL) throws -> Bool {
        guard let staged = stagedArchive(for: model) else { return false }
        let manager = FileManager.default
        try manager.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
        if manager.fileExists(atPath: destination.path) { try manager.removeItem(at: destination) }
        try manager.moveItem(at: staged, to: destination)
        try? manager.removeItem(at: marker(model))
        return true
    }
}
#endif
