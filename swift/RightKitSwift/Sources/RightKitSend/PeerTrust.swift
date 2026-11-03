import Foundation

/// A peer found on the local network, with the certificate fingerprint it advertises in its Bonjour TXT record.
public struct ResolvedPeer: Codable, Equatable, Sendable {
    /// The Bonjour service name, the peer's stable label on the network.
    public let serviceName: String
    /// Lowercase hex SHA-256 of the peer's DER certificate, as advertised in TXT key `fp`.
    public let certificateSHA256: String

    public init(serviceName: String, certificateSHA256: String) {
        self.serviceName = serviceName
        self.certificateSHA256 = certificateSHA256
    }
}

/// A peer the user has trusted. Trust is the certificate fingerprint, never the advertised name.
public struct PeerRecord: Codable, Equatable, Sendable {
    public let certificateSHA256: String
    public var name: String
    public let addedAt: Date

    public init(certificateSHA256: String, name: String, addedAt: Date) {
        self.certificateSHA256 = certificateSHA256
        self.name = name
        self.addedAt = addedAt
    }
}

public enum PeerTrustError: Error, Equatable, Sendable {
    /// The fingerprint is not 64 lowercase hex characters.
    case invalidFingerprint(String)
    /// The trust file exists but cannot be decoded. Nothing is rewritten.
    case corruptStore(String)
}

/// The set of trusted peers, optionally persisted as one atomically written JSON file.
///
/// Admission is by fingerprint only. `addResolved` records a peer discovered through Bonjour; it does
/// not make the peer trusted for a connection unless the caller adds it here, which is the consent step.
public final class PeerTrustStore: @unchecked Sendable {
    private let lock = NSLock()
    private let fileURL: URL?
    private var records: [String: PeerRecord]

    /// `fileURL` nil keeps the store in memory only.
    public init(fileURL: URL? = nil) throws {
        self.fileURL = fileURL
        var loaded: [String: PeerRecord] = [:]
        if let fileURL, FileManager.default.fileExists(atPath: fileURL.path) {
            do {
                let data = try Data(contentsOf: fileURL)
                let list = try JSONDecoder().decode([PeerRecord].self, from: data)
                for record in list { loaded[record.certificateSHA256] = record }
            } catch {
                throw PeerTrustError.corruptStore(fileURL.lastPathComponent)
            }
        }
        records = loaded
    }

    public var peers: [PeerRecord] {
        lock.lock(); defer { lock.unlock() }
        return records.values.sorted { $0.addedAt < $1.addedAt }
    }

    public func isTrusted(certificateSHA256: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return records[certificateSHA256] != nil
    }

    /// Trust a peer by fingerprint. Re-adding a known fingerprint keeps its first `addedAt` and updates its name.
    @discardableResult
    public func add(certificateSHA256: String, name: String, now: Date = Date()) throws -> PeerRecord {
        try Self.validate(certificateSHA256)
        lock.lock(); defer { lock.unlock() }
        let existing = records[certificateSHA256]
        let record = PeerRecord(certificateSHA256: certificateSHA256, name: name, addedAt: existing?.addedAt ?? now)
        records[certificateSHA256] = record
        try persistLocked()
        return record
    }

    /// Record a peer resolved from Bonjour. Returns the stored record, keeping the first `addedAt` for a known fingerprint.
    @discardableResult
    public func addResolved(_ peer: ResolvedPeer, now: Date = Date()) throws -> PeerRecord {
        try add(certificateSHA256: peer.certificateSHA256, name: peer.serviceName, now: now)
    }

    public func remove(certificateSHA256: String) throws {
        lock.lock(); defer { lock.unlock() }
        records[certificateSHA256] = nil
        try persistLocked()
    }

    static func validate(_ fingerprint: String) throws {
        let hex = Set("0123456789abcdef")
        guard fingerprint.count == 64, fingerprint.allSatisfy({ hex.contains($0) }) else {
            throw PeerTrustError.invalidFingerprint(fingerprint)
        }
    }

    private func persistLocked() throws {
        guard let fileURL else { return }
        let list = records.values.sorted { $0.certificateSHA256 < $1.certificateSHA256 }
        let data = try JSONEncoder().encode(list)
        try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: fileURL, options: .atomic)
    }
}
