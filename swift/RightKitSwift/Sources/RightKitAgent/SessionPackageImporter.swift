import Foundation

/// Swift mirror of `coderight_history::session_package::SessionPackage`
/// (ENG-101). Decodes a cross-host session package produced by the engine
/// (or another CodeRight host) and refuses anything whose `schemaVersion` is
/// newer than [`SessionPackageImporter.maxSupportedSchemaVersion`],
/// exactly like the Rust decoder's `SessionPackageError::UnsupportedSchemaVersion`.
///
/// This type never carries a credential field — see the Rust module's doc
/// comment for the enforcement rationale this mirrors.
public struct PackagedJournalEvent: Codable, Equatable, Sendable {
    public let sequence: UInt64
    public let nonce: String
    /// Opaque journal-event JSON, preserved verbatim. Decoded lazily by
    /// transcript-rendering call sites that already know the redacted
    /// `JournalEventKind` shape; this importer does not interpret it.
    public let kind: CodeRightJSONValue

    public init(sequence: UInt64, nonce: String, kind: CodeRightJSONValue) {
        self.sequence = sequence
        self.nonce = nonce
        self.kind = kind
    }
}

public struct WorkspaceIdentity: Codable, Equatable, Sendable {
    public let name: String
    public let identityKey: String?

    public init(name: String, identityKey: String? = nil) {
        self.name = name
        self.identityKey = identityKey
    }
}

public struct TargetMetadata: Codable, Equatable, Sendable {
    public let target: String
    public let hostLabel: String?

    public init(target: String, hostLabel: String? = nil) {
        self.target = target
        self.hostLabel = hostLabel
    }
}

public struct SessionPackage: Codable, Equatable, Sendable {
    public let schemaVersion: UInt32
    public let sessionId: String
    public let workspace: WorkspaceIdentity
    public let sourceTarget: TargetMetadata
    public let events: [PackagedJournalEvent]
    public let cursor: UInt64
    public let exportedAtMs: UInt64
}

public enum SessionPackageImportError: Error, LocalizedError, Sendable, Equatable {
    /// Mirrors `SessionPackageError::UnsupportedSchemaVersion`.
    case unsupportedSchemaVersion(found: UInt32, maxSupported: UInt32)
    /// Mirrors `SessionPackageError::InconsistentCursor`.
    case inconsistentCursor
    case malformed(String)

    public var errorDescription: String? {
        switch self {
        case .unsupportedSchemaVersion(let found, let maxSupported):
            return "This session package uses schema version \(found), newer than the "
                + "highest version this app supports (\(maxSupported)). Update the app "
                + "before importing it."
        case .inconsistentCursor:
            return "This session package's transcript does not match its saved position."
        case .malformed(let reason):
            return "This session package could not be read: \(reason)"
        }
    }
}

/// Decodes and validates a session package before anything in the app trusts
/// its contents, mirroring `coderight_history::session_package::SessionPackage::decode`.
public enum SessionPackageImporter {
    /// Highest `schemaVersion` this build understands. Kept in lockstep with
    /// the Rust crate's `SESSION_PACKAGE_FORMAT_VERSION`; bump both together.
    public static let maxSupportedSchemaVersion: UInt32 = 1

    /// Peek just the schema version without decoding the full package, so an
    /// unsupported version can be reported even if a future, wider schema
    /// would otherwise fail generic `Decodable` decoding.
    private struct VersionProbe: Decodable {
        let schemaVersion: UInt32
    }

    public static func decode(_ data: Data) throws -> SessionPackage {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase

        let probe: VersionProbe
        do {
            probe = try decoder.decode(VersionProbe.self, from: data)
        } catch {
            throw SessionPackageImportError.malformed(String(describing: error))
        }
        guard probe.schemaVersion <= maxSupportedSchemaVersion else {
            throw SessionPackageImportError.unsupportedSchemaVersion(
                found: probe.schemaVersion,
                maxSupported: maxSupportedSchemaVersion
            )
        }

        let package: SessionPackage
        do {
            package = try decoder.decode(SessionPackage.self, from: data)
        } catch {
            throw SessionPackageImportError.malformed(String(describing: error))
        }

        var lastSequence: UInt64 = 0
        for event in package.events {
            if event.sequence == 0 {
                throw SessionPackageImportError.inconsistentCursor
            }
            if lastSequence != 0 && event.sequence <= lastSequence {
                throw SessionPackageImportError.inconsistentCursor
            }
            lastSequence = event.sequence
        }
        if let last = package.events.last, last.sequence > package.cursor {
            throw SessionPackageImportError.inconsistentCursor
        }

        return package
    }
}

/// Minimal JSON value so `PackagedJournalEvent.kind` round-trips
/// through `Codable` without pulling in a third-party JSON library.
public enum CodeRightJSONValue: Codable, Equatable, Sendable {
    case string(String)
    case number(Double)
    case bool(Bool)
    case object([String: CodeRightJSONValue])
    case array([CodeRightJSONValue])
    case null

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([String: CodeRightJSONValue].self) {
            self = .object(value)
        } else if let value = try? container.decode([CodeRightJSONValue].self) {
            self = .array(value)
        } else {
            throw DecodingError.dataCorruptedError(
                in: container,
                debugDescription: "Unsupported JSON value"
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .bool(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .null: try container.encodeNil()
        }
    }
}
