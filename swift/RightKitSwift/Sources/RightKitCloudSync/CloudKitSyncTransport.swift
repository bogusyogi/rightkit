#if canImport(CloudKit)
import CloudKit
import Foundation

/// Production transport: the user's PRIVATE CloudKit database, one custom zone, no server.
///
/// Change tags are handled here so the engine never sees them: records returned by
/// `fetchChanges` are cached by name and reused for later saves; anything else is fetched or
/// created first. That is what stops "record to insert already exists" collisions without a
/// fetch+save round trip per record (HeardRight measured 4.5 minutes for 88 serial records).
public actor CloudKitSyncTransport: SyncTransport {
    public let containerIdentifier: String
    public let zoneName: String
    public let subscriptionID: String

    private let container: CKContainer
    private var cache: [String: CKRecord] = [:]

    public init(containerIdentifier: String, zoneName: String, subscriptionID: String, container: CKContainer? = nil) {
        self.containerIdentifier = containerIdentifier
        self.zoneName = zoneName
        self.subscriptionID = subscriptionID
        self.container = container ?? CKContainer(identifier: containerIdentifier)
    }

    private var database: CKDatabase { container.privateCloudDatabase }
    private var zoneID: CKRecordZone.ID { CKRecordZone.ID(zoneName: zoneName, ownerName: CKCurrentUserDefaultName) }

    public func accountState() async throws -> SyncAccountState {
        do {
            switch try await container.accountStatus() {
            case .available: return .available
            case .noAccount: return .noAccount
            case .restricted: return .restricted
            case .couldNotDetermine: return .couldNotDetermine
            case .temporarilyUnavailable: return .temporarilyUnavailable
            @unknown default: return .couldNotDetermine
            }
        } catch { throw SyncTransportError(error) }
    }

    public func prepareZone() async throws {
        do { _ = try await database.modifyRecordZones(saving: [CKRecordZone(zoneID: zoneID)], deleting: []) }
        catch { throw SyncTransportError(error) }
    }

    public func ensureSubscription() async throws {
        do {
            do {
                _ = try await database.subscription(for: subscriptionID)
            } catch let error as CKError where error.code == .unknownItem {
                let subscription = CKRecordZoneSubscription(zoneID: zoneID, subscriptionID: subscriptionID)
                let info = CKSubscription.NotificationInfo()
                info.shouldSendContentAvailable = true
                info.shouldBadge = false
                subscription.notificationInfo = info
                _ = try await database.save(subscription)
            }
        } catch { throw SyncTransportError(error) }
    }

    public func fetchChanges(since token: SyncChangeToken?) async throws -> SyncChangePage {
        let serverToken: CKServerChangeToken?
        if let token {
            // Secure-coded, class-constrained decode: corrupt or tampered bytes expire the cursor.
            guard let decoded = try? NSKeyedUnarchiver.unarchivedObject(ofClass: CKServerChangeToken.self, from: token.data) else {
                throw SyncTransportError.changeTokenExpired
            }
            serverToken = decoded
        } else {
            serverToken = nil
            cache.removeAll()
        }
        do {
            // Zone changes need no queryable-index schema, unlike a CKQuery, so they work on
            // development containers and return every record.
            let changes = try await database.recordZoneChanges(inZoneWith: zoneID, since: serverToken)
            var records: [SyncRecord] = []
            for (_, result) in changes.modificationResultsByID {
                guard let ck = try? result.get().record else { continue }
                cache[ck.recordID.recordName] = ck
                records.append(CloudKitRecordCoding.decode(ck))
            }
            let deleted = changes.deletions.map(\.recordID.recordName)
            for name in deleted { cache[name] = nil }
            let archived = try NSKeyedArchiver.archivedData(withRootObject: changes.changeToken, requiringSecureCoding: true)
            return SyncChangePage(
                records: records, deletedRecordNames: deleted,
                token: SyncChangeToken(data: archived), moreComing: changes.moreComing
            )
        } catch { throw SyncTransportError(error) }
    }

    public func save(_ writes: [SyncRecordWrite]) async throws -> [String: SyncSaveOutcome] {
        guard !writes.isEmpty else { return [:] }
        var outcomes: [String: SyncSaveOutcome] = [:]
        var records: [CKRecord] = []
        for write in writes {
            do {
                let base = try await baseRecord(name: write.recordName, type: write.recordType)
                CloudKitRecordCoding.apply(write.fields, to: base)
                records.append(base)
            } catch {
                outcomes[write.recordName] = .failed(SyncTransportError(error))
            }
        }
        guard !records.isEmpty else { return outcomes }
        do {
            let result = try await database.modifyRecords(
                saving: records, deleting: [], savePolicy: .allKeys, atomically: false
            )
            for record in records {
                let name = record.recordID.recordName
                switch result.saveResults[record.recordID] {
                case let .success(saved)?:
                    cache[name] = saved
                    outcomes[name] = .saved
                case let .failure(error)?:
                    cache[name] = nil
                    outcomes[name] = .failed(SyncTransportError(error))
                case nil:
                    outcomes[name] = .failed(.other(code: -1, message: "no result for record"))
                }
            }
        } catch { throw SyncTransportError(error) }
        return outcomes
    }

    private func baseRecord(name: String, type: String) async throws -> CKRecord {
        if let cached = cache[name] { return cached }
        let id = CKRecord.ID(recordName: name, zoneID: zoneID)
        do {
            return try await database.record(for: id)
        } catch let error as CKError where error.code == .unknownItem {
            return CKRecord(recordType: type, recordID: id)
        }
    }
}

/// Pure CKRecord <-> neutral mapping (testable offline).
public enum CloudKitRecordCoding {
    public static func apply(_ fields: [String: SyncValue], to record: CKRecord) {
        for (key, value) in fields {
            switch value {
            case let .string(v): record[key] = v as NSString
            case let .int(v): record[key] = NSNumber(value: v)
            case let .double(v): record[key] = NSNumber(value: v)
            case let .date(v): record[key] = v as NSDate
            case let .data(v): record[key] = v as NSData
            case let .strings(v): record[key] = v.isEmpty ? nil : v as NSArray
            case .null: record[key] = nil
            }
        }
    }

    public static func decode(_ record: CKRecord) -> SyncRecord {
        var fields: [String: SyncValue] = [:]
        for key in record.allKeys() {
            switch record[key] {
            case let v as String: fields[key] = .string(v)
            case let v as Date: fields[key] = .date(v)
            case let v as Data: fields[key] = .data(v)
            case let v as [String]: fields[key] = .strings(v)
            case let v as NSNumber:
                fields[key] = CFNumberIsFloatType(v) ? .double(v.doubleValue) : .int(v.int64Value)
            default: break
            }
        }
        return SyncRecord(
            recordName: record.recordID.recordName, recordType: record.recordType, fields: fields,
            modifiedAt: record.modificationDate, createdAt: record.creationDate
        )
    }
}

public extension SyncTransportError {
    init(_ error: Error) {
        if let existing = error as? SyncTransportError { self = existing; return }
        guard let ck = error as? CKError else {
            self = .other(code: -1, message: error.localizedDescription)
            return
        }
        // Partial failures carry the real retry guidance on the nested errors.
        var hint = (ck.userInfo[CKErrorRetryAfterKey] as? NSNumber)?.doubleValue
        if let nested = ck.userInfo[CKPartialErrorsByItemIDKey] as? [AnyHashable: Error] {
            for value in nested.values {
                if let n = ((value as NSError).userInfo[CKErrorRetryAfterKey] as? NSNumber)?.doubleValue {
                    hint = max(hint ?? 0, n)
                }
            }
        }
        switch ck.code {
        case .networkUnavailable, .networkFailure: self = .networkUnavailable(retryAfter: hint)
        case .requestRateLimited, .zoneBusy, .serviceUnavailable: self = .busy(retryAfter: hint)
        case .notAuthenticated: self = .notAuthenticated
        case .quotaExceeded: self = .quotaExceeded
        case .permissionFailure: self = .permissionDenied
        case .changeTokenExpired: self = .changeTokenExpired
        case .zoneNotFound, .userDeletedZone: self = .zoneNotFound
        case .unknownItem: self = .unknownItem
        default: self = .other(code: ck.code.rawValue, message: ck.localizedDescription)
        }
    }
}
#endif
