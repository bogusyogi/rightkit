import Foundation

public struct WatchReceivedRecording: Codable, Equatable, Sendable, Identifiable {
    public let id: String
    public let fileName: String
    public let receivedAt: Date
    public let metadataFileName: String
    public let startedAt: Date?
}

public struct WatchRecordingStorageKeys: Sendable {
    public var manifest: String
    public var processed: String
    public var terminal: String
    public var terminalReasons: String
    public var retryCounts: String
    public var lastError: String
    public init(manifest: String, processed: String, terminal: String, terminalReasons: String,
                retryCounts: String, lastError: String) {
        self.manifest = manifest; self.processed = processed; self.terminal = terminal
        self.terminalReasons = terminalReasons; self.retryCounts = retryCounts; self.lastError = lastError
    }
}

public enum WatchRecordingStorageError: Error { case invalidIdentifier, invalidDuration, invalidMetadata, corruptManifest, persistence }

/// Strict receipt preserves sender identity. Assignment is opt-in compatibility for hosts that
/// already acknowledge canonical/generated UUIDs; metadata remains unchanged in the sidecar.
/// A generated ID cannot identify a malformed sender queue entry without sender cooperation.
public enum WatchRecordingReceivePolicy: Sendable {
    case requireValidIdentifier
    case assignCanonicalIdentifier
}

public struct WatchRecordingStatus: Sendable {
    public let pendingCount: Int
    public let processedCount: Int
    public let noSpeechCount: Int
    public let terminalFailureCount: Int
    public let lastError: String?
}

/// Serializes all instances in one process, including synchronous WCSession receipt callbacks.
/// Root & keys are host inputs so existing files remain readable after adoption.
public final class WatchRecordingStorage: @unchecked Sendable {
    private static let lock = NSLock()
    private let defaults: UserDefaults
    private let keys: WatchRecordingStorageKeys
    private let root: URL
    private let fileManager: FileManager
    private let maximumDuration: TimeInterval
    private let receivePolicy: WatchRecordingReceivePolicy
    private let maximumManifestRecords: Int?

    public init(root: URL, defaults: UserDefaults = .standard, keys: WatchRecordingStorageKeys,
                maximumDuration: TimeInterval = 600, fileManager: FileManager = .default,
                receivePolicy: WatchRecordingReceivePolicy = .requireValidIdentifier,
                maximumManifestRecords: Int? = nil) {
        self.root = root; self.defaults = defaults; self.keys = keys
        self.maximumDuration = maximumDuration; self.fileManager = fileManager
        self.receivePolicy = receivePolicy
        self.maximumManifestRecords = maximumManifestRecords.map { max(0, $0) }
    }

    /// Returns only after audio, JSON sidecar & manifest exist. Strict receipt rejects invalid
    /// IDs; compatibility receipt returns its assigned UUID for the host to acknowledge.
    public func receive(fileURL: URL, metadata: [String: Any]) throws -> String {
        try Self.lock.withLock {
            let rawID = metadata["id"] as? String
            let id: String
            switch receivePolicy {
            case .requireValidIdentifier:
                guard let rawID, UUID(uuidString: rawID) != nil else {
                    throw WatchRecordingStorageError.invalidIdentifier
                }
                id = rawID
            case .assignCanonicalIdentifier:
                id = rawID.flatMap { UUID(uuidString: $0) }?.uuidString ?? UUID().uuidString
            }
            if let number = metadata["duration"] as? NSNumber {
                guard number.doubleValue.isFinite, number.doubleValue >= 0,
                      number.doubleValue <= maximumDuration else { throw WatchRecordingStorageError.invalidDuration }
            }
            guard JSONSerialization.isValidJSONObject(metadata) else { throw WatchRecordingStorageError.invalidMetadata }
            try fileManager.createDirectory(at: root, withIntermediateDirectories: true)
            var records = try readManifest()
            let audio = root.appendingPathComponent(id).appendingPathExtension("m4a")
            let sidecar = root.appendingPathComponent(id).appendingPathExtension("json")
            // Do not replace metadata for an already durable recording.
            if !fileManager.fileExists(atPath: sidecar.path) {
                try JSONSerialization.data(withJSONObject: metadata, options: [.sortedKeys]).write(to: sidecar, options: .atomic)
            }
            if !fileManager.fileExists(atPath: audio.path) {
                let incoming = root.appendingPathComponent(".\(id).incoming")
                if fileManager.fileExists(atPath: incoming.path) { try fileManager.removeItem(at: incoming) }
                try fileManager.copyItem(at: fileURL, to: incoming)
                try fileManager.moveItem(at: incoming, to: audio)
            }
            if !records.contains(where: { $0.id == id }) {
                let start = metadata["startedAt"] as? TimeInterval
                records.insert(WatchReceivedRecording(id: id, fileName: audio.lastPathComponent,
                    receivedAt: Date(), metadataFileName: sidecar.lastPathComponent,
                    startedAt: start.flatMap { $0.isFinite ? Date(timeIntervalSince1970: $0) : nil }), at: 0)
            }
            try writeManifest(records)
            return id
        }
    }

    public func records() throws -> [WatchReceivedRecording] { try Self.lock.withLock { try readManifest() } }
    public func audioURL(for record: WatchReceivedRecording) -> URL { RelocatableFile.url(fileName: record.fileName, in: root) }
    public func metadata(for record: WatchReceivedRecording) throws -> [String: Any] {
        try JSONSerialization.jsonObject(with: metadataData(for: record)) as? [String: Any] ?? [:]
    }
    public func metadataData(for record: WatchReceivedRecording) throws -> Data {
        try Self.lock.withLock {
            let url = RelocatableFile.url(fileName: record.metadataFileName, in: root)
            // Legacy orphan audio may lack a sidecar. Host then chooses its configured mode.
            guard fileManager.fileExists(atPath: url.path) else { return Data("{}".utf8) }
            return try Data(contentsOf: url)
        }
    }
    /// Receipt queries must include the entire durable manifest, including processed files.
    public func receivedRecordingIDs() throws -> [String] {
        try Self.lock.withLock {
            try readManifest().filter { fileManager.fileExists(atPath: audioURL(for: $0).path) }.map(\.id)
        }
    }
    public func recoverOrphans() throws {
        try Self.lock.withLock {
            try fileManager.createDirectory(at: root, withIntermediateDirectories: true)
            let files = try fileManager.contentsOfDirectory(at: root,
                includingPropertiesForKeys: [.contentModificationDateKey], options: [.skipsHiddenFiles])
            var records = try readManifest()
            var known = Set(records.map(\.id))
            for file in files where file.pathExtension == "m4a" {
                let id = file.deletingPathExtension().lastPathComponent
                guard UUID(uuidString: id) != nil, known.insert(id).inserted else { continue }
                let sidecar = file.deletingPathExtension().appendingPathExtension("json")
                let metadata = (try? Data(contentsOf: sidecar)).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
                let start = metadata?["startedAt"] as? TimeInterval
                records.append(WatchReceivedRecording(id: id, fileName: file.lastPathComponent,
                    receivedAt: (try? file.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate ?? Date(),
                    metadataFileName: sidecar.lastPathComponent,
                    startedAt: start.flatMap { $0.isFinite ? Date(timeIntervalSince1970: $0) : nil }))
            }
            try writeManifest(records)
        }
    }
    public func isFinished(_ id: String) -> Bool {
        Self.lock.withLock { strings(keys.processed).contains(id) || strings(keys.terminal).contains(id) }
    }
    public func status() throws -> WatchRecordingStatus {
        try Self.lock.withLock {
            let records = try readManifest()
            let processed = strings(keys.processed)
            let terminal = strings(keys.terminal)
            let reasons = defaults.dictionary(forKey: keys.terminalReasons) as? [String: String] ?? [:]
            let noSpeech = records.filter { terminal.contains($0.id) && (reasons[$0.id] ?? "no_speech") == "no_speech" }.count
            let terminalCount = records.filter { terminal.contains($0.id) && !processed.contains($0.id) }.count
            return WatchRecordingStatus(
                pendingCount: records.filter { !processed.contains($0.id) && !terminal.contains($0.id) }.count,
                processedCount: records.filter { processed.contains($0.id) }.count,
                noSpeechCount: noSpeech, terminalFailureCount: max(0, terminalCount - noSpeech),
                lastError: defaults.string(forKey: keys.lastError))
        }
    }
    public func retryCount(_ id: String) -> Int {
        Self.lock.withLock { (defaults.dictionary(forKey: keys.retryCounts)?[id] as? NSNumber)?.intValue ?? 0 }
    }
    public func setRetryCount(_ count: Int?, for id: String) {
        Self.lock.withLock {
            var counts = defaults.dictionary(forKey: keys.retryCounts) ?? [:]
            counts[id] = count
            defaults.set(counts, forKey: keys.retryCounts)
        }
    }
    public func markProcessed(_ id: String) throws {
        try Self.lock.withLock { try append(id, to: keys.processed) }
    }
    public func markTerminal(_ id: String, reason: String) throws {
        try Self.lock.withLock {
            var reasons = defaults.dictionary(forKey: keys.terminalReasons) ?? [:]
            reasons[id] = reason; defaults.set(reasons, forKey: keys.terminalReasons)
            try append(id, to: keys.terminal)
        }
    }
    public var terminalReasons: [String: String] {
        Self.lock.withLock { defaults.dictionary(forKey: keys.terminalReasons) as? [String: String] ?? [:] }
    }
    public var lastError: String? {
        get { Self.lock.withLock { defaults.string(forKey: keys.lastError) } }
        set { Self.lock.withLock { defaults.set(newValue, forKey: keys.lastError) } }
    }
    private func strings(_ key: String) -> Set<String> { Set(defaults.stringArray(forKey: key) ?? []) }
    private func append(_ id: String, to key: String) throws {
        var values = strings(key); values.insert(id); defaults.set(values.sorted(), forKey: key)
        guard defaults.synchronize() else { throw WatchRecordingStorageError.persistence }
    }
    private func readManifest() throws -> [WatchReceivedRecording] {
        guard let data = defaults.data(forKey: keys.manifest) else { return [] }
        guard let decoded = try? JSONDecoder().decode([WatchReceivedRecording].self, from: data) else {
            throw WatchRecordingStorageError.corruptManifest
        }
        return decoded
    }
    private func writeManifest(_ records: [WatchReceivedRecording]) throws {
        // Optional soft cap matches bounded phone history without dropping unprocessed audio.
        // Pending rows may exceed the cap. Eviction removes only finished manifest rows;
        // audio, sidecars & processing ledgers remain available for recovery/deduplication.
        var retained = records
        if let maximumManifestRecords, records.count > maximumManifestRecords {
            let finished = strings(keys.processed).union(strings(keys.terminal))
            let pendingCount = records.filter { !finished.contains($0.id) }.count
            let finishedSlots = max(0, maximumManifestRecords - pendingCount)
            let keepFinished = Set(records.filter { finished.contains($0.id) }
                .sorted { $0.receivedAt > $1.receivedAt }.prefix(finishedSlots).map(\.id))
            retained = records.filter { !finished.contains($0.id) || keepFinished.contains($0.id) }
        }
        defaults.set(try JSONEncoder().encode(retained), forKey: keys.manifest)
        guard defaults.synchronize() else { throw WatchRecordingStorageError.persistence }
    }
}
