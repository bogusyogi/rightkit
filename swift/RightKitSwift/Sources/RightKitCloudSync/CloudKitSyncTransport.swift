#if canImport(CloudKit)
import CloudKit
import CryptoKit
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

    private let container: CKContainer?
    private var cache: [String: CKRecord] = [:]

    public init(containerIdentifier: String, zoneName: String, subscriptionID: String, container: CKContainer? = nil) {
        self.containerIdentifier = containerIdentifier
        self.zoneName = zoneName
        self.subscriptionID = subscriptionID
        // macOS CloudKit may trap before an operation can throw when the process lacks its
        // container entitlement. Preserve the initializer, but defer the typed failure to I/O.
        // iOS/watchOS cannot inspect their own provisioning; those hosts still need native evidence.
        self.container = CloudKitEntitlement.current(containerIdentifier: containerIdentifier).allowsCloudKit
            ? (container ?? CKContainer(identifier: containerIdentifier)) : nil
    }

    private var availableContainer: CKContainer {
        get throws {
            guard let container else { throw SyncTransportError.permissionDenied }
            return container
        }
    }
    private var database: CKDatabase {
        get throws { try availableContainer.privateCloudDatabase }
    }
    private var zoneID: CKRecordZone.ID { CKRecordZone.ID(zoneName: zoneName, ownerName: CKCurrentUserDefaultName) }

    public func accountState() async throws -> SyncAccountState {
        do {
            switch try await availableContainer.accountStatus() {
            case .available: return .available
            case .noAccount: return .noAccount
            case .restricted: return .restricted
            case .couldNotDetermine: return .couldNotDetermine
            case .temporarilyUnavailable: return .temporarilyUnavailable
            @unknown default: return .couldNotDetermine
            }
        } catch { throw SyncTransportError(error) }
    }

    /// Sync-state scope: sha256(container, zone, userRecordID). The ledger (token, versions,
    /// aliases, tombstones, deletion queue) belongs to exactly this triple.
    public func accountIdentity() async throws -> String? {
        do {
            let user = try await availableContainer.userRecordID().recordName
            return CloudKitRecordCoding.syncScope(container: containerIdentifier, zone: zoneName, userRecordName: user)
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
            var failed: [String] = []
            var undecodable: [String] = []
            var retryAfter: TimeInterval?
            for (id, result) in changes.modificationResultsByID {
                // A per-record fetch failure must never vanish behind the advanced token: report
                // it as transient so the engine aborts the pull and keeps its previous token.
                let ck: CKRecord
                do { ck = try result.get().record } catch {
                    failed.append(id.recordName)
                    if let delay = SyncTransportError(error).retryDelay { retryAfter = max(retryAfter ?? 0, delay) }
                    continue
                }
                // A fetched record that cannot be decoded is permanent: quarantine, never retry-loop.
                do {
                    let decoded = try CloudKitRecordCoding.decode(ck)
                    cache[ck.recordID.recordName] = ck
                    records.append(decoded)
                } catch {
                    undecodable.append(id.recordName)
                }
            }
            let deleted = changes.deletions.map(\.recordID.recordName)
            var deletedTypes: [String: String] = [:]
            for deletion in changes.deletions { deletedTypes[deletion.recordID.recordName] = deletion.recordType }
            for name in deleted { cache[name] = nil }
            let archived = try NSKeyedArchiver.archivedData(withRootObject: changes.changeToken, requiringSecureCoding: true)
            return SyncChangePage(
                records: records, deletedRecordNames: deleted,
                token: SyncChangeToken(data: archived), moreComing: changes.moreComing,
                failedRecordNames: failed.sorted(), failedRecordRetryAfter: retryAfter,
                undecodableRecordNames: undecodable.sorted(), deletedRecordTypes: deletedTypes
            )
        } catch { throw SyncTransportError(error) }
    }

    /// Live server copy of one record (`CKDatabase.record(for:)`), or nil for `.unknownItem`.
    /// The fetched record also refreshes the change-tag cache used by later saves.
    public func fetchRecord(named recordName: String) async throws -> SyncRecord? {
        let id = CKRecord.ID(recordName: recordName, zoneID: zoneID)
        let record: CKRecord
        do {
            record = try await database.record(for: id)
        } catch let error as CKError where error.code == .unknownItem {
            cache[recordName] = nil
            return nil
        } catch { throw SyncTransportError(error) }
        let decoded = try CloudKitRecordCoding.decode(record)
        cache[recordName] = record
        return decoded
    }

    public func save(_ writes: [SyncRecordWrite]) async throws -> [String: SyncSaveOutcome] {
        guard !writes.isEmpty else { return [:] }
        var outcomes: [String: SyncSaveOutcome] = [:]
        var records: [CKRecord] = []
        var staged: [URL] = []
        defer { for url in staged { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) } }
        for write in writes {
            do {
                let base = try await baseRecord(name: write.recordName, type: write.recordType)
                staged += try CloudKitRecordCoding.apply(write.fields, to: base)
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
    /// Hex sha256 of "container|zone|userRecordName", the same scope HeardRight's macOS sync uses.
    public static func syncScope(container: String, zone: String, userRecordName: String) -> String {
        SHA256.hash(data: Data("\(container)|\(zone)|\(userRecordName)".utf8))
            .map { String(format: "%02x", $0) }.joined()
    }

    /// Applies `fields` to `record`. `.asset` values are staged as files; the returned URLs are
    /// the staged files, which the caller removes once the save finished (CloudKit uploads them
    /// during the operation, so they must outlive it).
    @discardableResult
    public static func apply(_ fields: [String: SyncValue], to record: CKRecord) throws -> [URL] {
        var staged: [URL] = []
        for (key, value) in fields {
            switch value {
            case let .string(v): record[key] = v as NSString
            case let .int(v): record[key] = NSNumber(value: v)
            case let .double(v): record[key] = NSNumber(value: v)
            case let .date(v): record[key] = v as NSDate
            case let .data(v): record[key] = v as NSData
            case let .strings(v): record[key] = v.isEmpty ? nil : v as NSArray
            case let .asset(v):
                let directory = FileManager.default.temporaryDirectory
                    .appendingPathComponent("rightkit-assets-\(UUID().uuidString)", isDirectory: true)
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                let url = directory.appendingPathComponent("asset.bin")
                try v.write(to: url, options: .atomic)
                staged.append(url)
                record[key] = CKAsset(fileURL: url)
            case .null: record[key] = nil
            }
        }
        return staged
    }

    /// Throws when an asset-backed field cannot be read back: dropping it would present a record
    /// whose content silently vanished.
    public static func decode(_ record: CKRecord) throws -> SyncRecord {
        var fields: [String: SyncValue] = [:]
        for key in record.allKeys() {
            switch record[key] {
            case let v as String: fields[key] = .string(v)
            case let v as Date: fields[key] = .date(v)
            case let v as Data: fields[key] = .data(v)
            case let v as [String]: fields[key] = .strings(v)
            case let v as CKAsset:
                guard let url = v.fileURL, let bytes = try? Data(contentsOf: url) else {
                    throw SyncTransportError.other(code: -2, message: "asset unreadable for field \(key)")
                }
                fields[key] = .asset(bytes)
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
