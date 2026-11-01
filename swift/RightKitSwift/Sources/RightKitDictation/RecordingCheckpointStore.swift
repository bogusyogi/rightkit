import Foundation

/// Durable manifest for an in-progress recording. A manifest is written before
/// each chunk so an interrupted long capture can be found on next launch.
struct RecordingCheckpointManifest: Codable, Equatable {
    struct Chunk: Codable, Equatable {
        let index: Int
        let fileName: String
        let startSample: Int
        let sampleCount: Int
        let createdAt: Date
    }

    let sessionID: String
    let startedAt: Date
    let sampleRate: Int
    var chunks: [Chunk]
    var isComplete: Bool
    var lastUpdatedAt: Date
}

final class RecordingCheckpointStore {
    static let shared = RecordingCheckpointStore()
    private let fileManager = FileManager.default
    private let root: URL

    init(root: URL? = nil) {
        let applicationSupport = root ?? fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("HeardRight/RecordingCheckpoints", isDirectory: true)
        self.root = applicationSupport
        try? fileManager.createDirectory(at: applicationSupport, withIntermediateDirectories: true)
    }

    func begin(sessionID: String, startedAt: Date = Date(), sampleRate: Int) throws {
        let directory = directory(for: sessionID)
        try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        let manifest = RecordingCheckpointManifest(
            sessionID: sessionID,
            startedAt: startedAt,
            sampleRate: sampleRate,
            chunks: [],
            isComplete: false,
            lastUpdatedAt: Date()
        )
        try write(manifest, for: sessionID)
    }

    func appendChunk(
        sessionID: String,
        index: Int,
        startSample: Int,
        sampleCount: Int,
        fileName: String
    ) throws {
        guard sampleCount > 0 else { return }
        var manifest = try load(sessionID: sessionID)
        guard !manifest.isComplete,
              !manifest.chunks.contains(where: { $0.index == index }) else { return }
        manifest.chunks.append(.init(
            index: index,
            fileName: fileName,
            startSample: startSample,
            sampleCount: sampleCount,
            createdAt: Date()
        ))
        manifest.chunks.sort { $0.index < $1.index }
        manifest.lastUpdatedAt = Date()
        try write(manifest, for: sessionID)
    }

    func complete(sessionID: String) throws {
        var manifest = try load(sessionID: sessionID)
        manifest.isComplete = true
        manifest.lastUpdatedAt = Date()
        try write(manifest, for: sessionID)
    }

    func load(sessionID: String) throws -> RecordingCheckpointManifest {
        let data = try Data(contentsOf: manifestURL(for: sessionID))
        return try JSONDecoder().decode(RecordingCheckpointManifest.self, from: data)
    }

    func chunkURL(sessionID: String, index: Int) -> URL {
        directory(for: sessionID).appendingPathComponent(
            "chunk-\(String(format: "%04d", index)).wav"
        )
    }

    func recoverable() -> [RecordingCheckpointManifest] {
        guard let sessions = try? fileManager.contentsOfDirectory(
            at: root,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        ) else { return [] }
        return sessions.compactMap { url in
            guard let id = url.lastPathComponent.removingPercentEncoding,
                  let manifest = try? load(sessionID: id),
                  !manifest.isComplete,
                  !manifest.chunks.isEmpty else { return nil }
            return manifest
        }.sorted { $0.lastUpdatedAt > $1.lastUpdatedAt }
    }

    func remove(sessionID: String) {
        try? fileManager.removeItem(at: directory(for: sessionID))
    }

    private func directory(for sessionID: String) -> URL {
        root.appendingPathComponent(sessionID, isDirectory: true)
    }

    private func manifestURL(for sessionID: String) -> URL {
        directory(for: sessionID).appendingPathComponent("manifest.json")
    }

    private func write(_ manifest: RecordingCheckpointManifest, for sessionID: String) throws {
        let data = try JSONEncoder().encode(manifest)
        try data.write(to: manifestURL(for: sessionID), options: .atomic)
    }
}
