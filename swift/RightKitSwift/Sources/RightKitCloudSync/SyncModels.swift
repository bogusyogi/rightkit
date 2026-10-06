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
    /// Large body stored as a CloudKit asset (`CKAsset`) instead of an inline field, so it is not
    /// bound by the per-record size limit. A field keeps one kind for its lifetime: CloudKit
    /// schemas cannot change a field between bytes and asset. Reads of an asset-backed record
    /// return `.asset`; `dataValue` yields the bytes for either kind.
    case asset(Data)
    /// Clears the field on the server. An empty `.strings([])` is treated the same way, because
    /// CloudKit refuses to create a new schema field from an empty list (CKError 12) — the failure
    /// that blocked every first sync in HeardRight before the fix.
    case null

    public var stringValue: String? { if case let .string(v) = self { v } else { nil } }
    public var dateValue: Date? { if case let .date(v) = self { v } else { nil } }
    public var dataValue: Data? {
        switch self {
        case let .data(v), let .asset(v): v
        default: nil
        }
    }
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
    /// TRANSIENT per-record failures: the server listed these records but the fetch of them failed
    /// (network, rate limit, busy zone) inside an otherwise successful page. The engine aborts the
    /// pull, keeps its previous token and honours `failedRecordRetryAfter`. Never quarantined.
    public var failedRecordNames: [String]
    /// Server guidance for retrying `failedRecordNames`; nil lets the engine apply its floor.
    public var failedRecordRetryAfter: TimeInterval?
    /// PERMANENT per-record failures: the record was fetched but can never be decoded (corrupt
    /// payload, unreadable asset). Quarantined at once; the cursor moves on.
    public var undecodableRecordNames: [String]

    public init(
        records: [SyncRecord], deletedRecordNames: [String], token: SyncChangeToken, moreComing: Bool,
        failedRecordNames: [String] = [], failedRecordRetryAfter: TimeInterval? = nil,
        undecodableRecordNames: [String] = []
    ) {
        self.records = records
        self.deletedRecordNames = deletedRecordNames
        self.token = token
        self.moreComing = moreComing
        self.failedRecordNames = failedRecordNames
        self.failedRecordRetryAfter = failedRecordRetryAfter
        self.undecodableRecordNames = undecodableRecordNames
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
