import Foundation

/// Cross-process payload shared by a share extension and its app. Legacy payloads that omitted
/// `id` derive one from immutable content so migration is repeatable.
public struct PendingSharePayload: Codable, Sendable, Equatable {
    public let id: UUID
    public let text: String?
    public let fileURL: String?
    public let typeIdentifier: String?
    /// Name the user's item had before staging renamed it. Optional; older payloads omit it.
    public let originalName: String?
    public let createdAt: Date
    public var lastError: String?
    public var lastAttemptAt: Date?

    public init(
        id: UUID = UUID(),
        text: String?,
        fileURL: String?,
        typeIdentifier: String?,
        originalName: String? = nil,
        createdAt: Date = Date(),
        lastError: String? = nil,
        lastAttemptAt: Date? = nil
    ) {
        self.id = id
        self.text = text
        self.fileURL = fileURL
        self.typeIdentifier = typeIdentifier
        self.originalName = originalName
        self.createdAt = createdAt
        self.lastError = lastError
        self.lastAttemptAt = lastAttemptAt
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        text = try values.decodeIfPresent(String.self, forKey: .text)
        fileURL = try values.decodeIfPresent(String.self, forKey: .fileURL)
        typeIdentifier = try values.decodeIfPresent(String.self, forKey: .typeIdentifier)
        originalName = try values.decodeIfPresent(String.self, forKey: .originalName)
        // Legacy payloads omitted this field; epoch keeps derived IDs stable across launches
        // instead of hashing a fresh wall-clock value.
        createdAt = try values.decodeIfPresent(Date.self, forKey: .createdAt)
            ?? Date(timeIntervalSinceReferenceDate: 0)
        id = try values.decodeIfPresent(UUID.self, forKey: .id)
            ?? Self.stableLegacyID(text: text, fileURL: fileURL, typeIdentifier: typeIdentifier, createdAt: createdAt)
        lastError = try values.decodeIfPresent(String.self, forKey: .lastError)
        lastAttemptAt = try values.decodeIfPresent(Date.self, forKey: .lastAttemptAt)
    }

    private static func stableLegacyID(text: String?, fileURL: String?, typeIdentifier: String?, createdAt: Date) -> UUID {
        let seed = "\(text ?? "")\u{1f}\(fileURL ?? "")\u{1f}\(typeIdentifier ?? "")\u{1f}\(createdAt.timeIntervalSinceReferenceDate)"
        var first: UInt64 = 1469598103934665603
        for byte in seed.utf8 {
            first ^= UInt64(byte)
            first &*= 1099511628211
        }
        var second = first ^ 0x9e3779b97f4a7c15
        for byte in seed.utf8.reversed() {
            second ^= UInt64(byte)
            second &*= 1099511628211
        }
        var bytes = [UInt8](repeating: 0, count: 16)
        withUnsafeBytes(of: first) { bytes.replaceSubrange(0..<8, with: $0) }
        withUnsafeBytes(of: second) { bytes.replaceSubrange(8..<16, with: $0) }
        return UUID(uuid: (
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ))
    }
}
