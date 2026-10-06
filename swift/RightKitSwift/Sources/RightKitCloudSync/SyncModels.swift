import Foundation

/// Transport-neutral field value. The engine and adapters never touch `CKRecord`, so the whole sync
/// state machine runs (and is tested) without an iCloud account.
public enum SyncValue: Equatable, Sendable {
    case string(String)
    case int(Int64)
    case double(Double)
    case date(Date)
    case data(Data)
    case strings([String])
    /// Clears the field on the server. An empty `.strings([])` is treated the same way, because
    /// CloudKit refuses to create a new schema field from an empty list (CKError 12) — the failure
    /// that blocked every first sync in HeardRight before the fix.
    case null

    public var stringValue: String? { if case let .string(v) = self { v } else { nil } }
    public var dateValue: Date? { if case let .date(v) = self { v } else { nil } }
    public var dataValue: Data? { if case let .data(v) = self { v } else { nil } }
    public var stringsValue: [String]? { if case let .strings(v) = self { v } else { nil } }
    public var intValue: Int64? { if case let .int(v) = self { v } else { nil } }
    public var doubleValue: Double? { if case let .double(v) = self { v } else { nil } }
}

/// A record as read from the zone.
public struct SyncRecord: Equatable, Sendable {
    public var recordName: String
    public var recordType: String
    public var fields: [String: SyncValue]
    /// Server modification/creation dates, for header recovery when an app field is missing.
    public var modifiedAt: Date?
    public var createdAt: Date?

    public init(
        recordName: String,
        recordType: String,
        fields: [String: SyncValue] = [:],
        modifiedAt: Date? = nil,
        createdAt: Date? = nil
    ) {
        self.recordName = recordName
        self.recordType = recordType
        self.fields = fields
        self.modifiedAt = modifiedAt
        self.createdAt = createdAt
    }
}

/// A full-field write. `.null` values clear the field. Writes are authoritative (the user's own
/// single-writer private zone): the transport saves with `.allKeys`, so a stale change tag can never
/// wedge a record.
public struct SyncRecordWrite: Equatable, Sendable {
    public var recordName: String
    public var recordType: String
    public var fields: [String: SyncValue]

    public init(recordName: String, recordType: String, fields: [String: SyncValue]) {
        self.recordName = recordName
        self.recordType = recordType
        self.fields = fields
    }
}

/// Opaque server continuation token. Persisted only after a whole pull has been applied.
public struct SyncChangeToken: Equatable, Sendable, Codable {
    public var data: Data
    public init(data: Data) { self.data = data }
}

public struct SyncChangePage: Sendable {
    public var records: [SyncRecord]
    /// Hard-deleted record names (no fields survive a hard delete).
    public var deletedRecordNames: [String]
    public var token: SyncChangeToken
    public var moreComing: Bool

    public init(records: [SyncRecord], deletedRecordNames: [String], token: SyncChangeToken, moreComing: Bool) {
        self.records = records
        self.deletedRecordNames = deletedRecordNames
        self.token = token
        self.moreComing = moreComing
    }
}

public enum SyncAccountState: Equatable, Sendable {
    case available
    case noAccount
    case restricted
    case couldNotDetermine
    case temporarilyUnavailable
}

/// Classified transport failure. The CloudKit transport maps `CKError` into this; the engine's
/// retry, replay and user-message decisions depend only on this type.
public enum SyncTransportError: Error, Equatable, Sendable {
    case networkUnavailable(retryAfter: TimeInterval?)
    /// Rate limited, zone busy, or service unavailable.
    case busy(retryAfter: TimeInterval?)
    case notAuthenticated
    case quotaExceeded
    case permissionDenied
    case changeTokenExpired
    /// Zone missing or deleted by the user (iCloud storage reset).
    case zoneNotFound
    case unknownItem
    case other(code: Int, message: String)

    /// Minimum delay before retrying, honouring server guidance (never earlier than it permits).
    public var retryDelay: TimeInterval? {
        switch self {
        case let .networkUnavailable(hint): Self.merge(hint, 15)
        case let .busy(hint): Self.merge(hint, 30)
        default: nil
        }
    }

    public var isRetryable: Bool { retryDelay != nil }

    private static func merge(_ hint: TimeInterval?, _ fallback: TimeInterval) -> TimeInterval {
        guard let hint, hint.isFinite else { return fallback }
        return max(hint, fallback)
    }
}

public enum SyncSaveOutcome: Equatable, Sendable {
    case saved
    case failed(SyncTransportError)
}

public enum SyncStatus: Equatable, Sendable {
    case disabled
    case unavailable(String)
    case ready
}

public struct SyncResult: Equatable, Sendable {
    public var uploaded: Int
    public var downloaded: Int
    /// Local items removed because of remote tombstones.
    public var deleted: Int
    public var status: SyncStatus

    public init(uploaded: Int = 0, downloaded: Int = 0, deleted: Int = 0, status: SyncStatus) {
        self.uploaded = uploaded
        self.downloaded = downloaded
        self.deleted = deleted
        self.status = status
    }

    public static let disabled = SyncResult(status: .disabled)

    /// Whether a silent-push background fetch brought anything new.
    public var hasNewData: Bool { downloaded + deleted > 0 }
}

public enum SyncTrigger: String, Sendable {
    case launch, foreground, localMutation, remotePush, networkRestored, accountChanged, enabled, retry
}

/// Diagnostics seam. Bridge to `RightKitDiagnostics` (or `os.Logger`) in the app; never put user
/// content in `fields`.
public protocol SyncEventSink: Sendable {
    func record(_ event: String, fields: [String: String])
}

public struct NullSyncEventSink: SyncEventSink {
    public init() {}
    public func record(_ event: String, fields: [String: String]) {}
}
