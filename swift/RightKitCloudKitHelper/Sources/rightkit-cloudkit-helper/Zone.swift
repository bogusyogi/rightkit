// Zone I/O the helper needs: the shared transport's account/zone/subscription calls plus a change-tag
// aware fetch and a conditional save (`.ifServerRecordUnchanged`), which RightKitCloudSync's
// transport does not expose. `CloudKitZone` talks to CloudKit; `MemoryZone` is the deterministic
// in-process zone used by the protocol E2E tests (it can also simulate a concurrent writer).

import Foundation
import RightKitCloudSync
#if canImport(CloudKit)
import CloudKit
#endif

struct ZoneRecord: Sendable {
    var record: SyncRecord
    /// Opaque server change tag: `CKRecord` system fields (base64) on CloudKit, a sequence number in
    /// memory. Handed to Rust with every fetched record and returned with the next conditional save.
    var changeTag: String?
}

struct ZoneWrite: Sendable {
    var recordName: String
    var recordType: String
    var fields: [String: SyncValue]
    var changeTag: String?
}

enum ZoneSave: Sendable {
    case saved
    /// The server record differs from the write's change tag (nil = could not be read).
    case conflict(ZoneRecord?)
    case failed(SyncTransportError)
}

struct ZonePage: Sendable {
    var records: [ZoneRecord]
    var deleted: [String]
    var token: Data
    var moreComing: Bool
    var failed: [String]
    var failedRetryAfter: Double?
    var undecodable: [String]
}

protocol ZoneIO: Sendable {
    func accountState() async throws -> SyncAccountState
    func accountIdentity() async throws -> String?
    func prepareZone() async throws
    func ensureSubscription() async throws
    func fetchChanges(since: Data?) async throws -> ZonePage
    func record(_ name: String) async throws -> ZoneRecord?
    /// `conditional`: each write must match the server's change tag (no tag = must not exist yet).
    func save(_ writes: [ZoneWrite], conditional: Bool) async throws -> [String: ZoneSave]
}

// MARK: - Memory

actor MemoryZone: ZoneIO {
    private struct Cursor: Codable { var epoch: Int; var seq: Int }
    private var records: [String: (seq: Int, record: SyncRecord)] = [:]
    private var log: [(seq: Int, name: String, deleted: Bool)] = []
    private var seq = 0
    private var epoch = 0
    private var zoneExists = false
    private var account: String
    private var pageSize: Int

    init(account: String, pageSize: Int) {
        self.account = account
        self.pageSize = max(1, pageSize)
    }

    func accountState() async throws -> SyncAccountState { .available }
    func accountIdentity() async throws -> String? { account }
    func prepareZone() async throws { zoneExists = true }
    func ensureSubscription() async throws {}

    func fetchChanges(since: Data?) async throws -> ZonePage {
        guard zoneExists else { throw SyncTransportError.zoneNotFound }
        var after = 0
        if let since {
            guard let c = try? JSONDecoder().decode(Cursor.self, from: since), c.epoch == epoch else {
                throw SyncTransportError.changeTokenExpired
            }
            after = c.seq
        }
        var latest: [String: (seq: Int, deleted: Bool)] = [:]
        for e in log where e.seq > after { latest[e.name] = (e.seq, e.deleted) }
        let ordered = latest.sorted { $0.value.seq < $1.value.seq }
        let page = Array(ordered.prefix(pageSize))
        let more = ordered.count > page.count
        let cursor = more ? (page.last?.value.seq ?? seq) : seq
        var out: [ZoneRecord] = []
        var deleted: [String] = []
        for (name, event) in page {
            if event.deleted { deleted.append(name) } else if let r = records[name] {
                out.append(ZoneRecord(record: r.record, changeTag: String(r.seq)))
            }
        }
        let token = try JSONEncoder().encode(Cursor(epoch: epoch, seq: cursor))
        return ZonePage(records: out, deleted: deleted, token: token, moreComing: more,
                        failed: [], failedRetryAfter: nil, undecodable: [])
    }

    func record(_ name: String) async throws -> ZoneRecord? {
        records[name].map { ZoneRecord(record: $0.record, changeTag: String($0.seq)) }
    }

    func save(_ writes: [ZoneWrite], conditional: Bool) async throws -> [String: ZoneSave] {
        guard zoneExists else { throw SyncTransportError.zoneNotFound }
        var out: [String: ZoneSave] = [:]
        for w in writes {
            let current = records[w.recordName]
            if conditional {
                let serverTag = current.map { String($0.seq) }
                if serverTag != w.changeTag {
                    if serverTag == nil { out[w.recordName] = .failed(.unknownItem) } else {
                        out[w.recordName] = .conflict(current.map { ZoneRecord(record: $0.record, changeTag: String($0.seq)) })
                    }
                    continue
                }
            }
            put(name: w.recordName, type: w.recordType, fields: w.fields)
            out[w.recordName] = .saved
        }
        return out
    }

    /// Simulates another device writing: replaces/creates the record, bumping its change tag.
    func debugWrite(name: String, type: String, fields: [String: SyncValue]) {
        zoneExists = true
        put(name: name, type: type, fields: fields)
    }

    private func put(name: String, type: String, fields: [String: SyncValue]) {
        var record = records[name]?.record ?? SyncRecord(recordName: name, recordType: type, createdAt: Date())
        for (key, value) in fields {
            switch value {
            case .null, .strings([]): record.fields[key] = nil
            default: record.fields[key] = value
            }
        }
        record.modifiedAt = Date()
        seq += 1
        records[name] = (seq, record)
        log.append((seq, name, false))
    }
}

// MARK: - CloudKit

#if canImport(CloudKit)
struct CloudKitZone: ZoneIO {
    private let base: CloudKitSyncTransport
    private let container: CKContainer
    private let zoneID: CKRecordZone.ID

    init(containerIdentifier: String, zoneName: String, subscriptionID: String) {
        base = CloudKitSyncTransport(containerIdentifier: containerIdentifier, zoneName: zoneName, subscriptionID: subscriptionID)
        container = CKContainer(identifier: containerIdentifier)
        zoneID = CKRecordZone.ID(zoneName: zoneName, ownerName: CKCurrentUserDefaultName)
    }

    private var database: CKDatabase { container.privateCloudDatabase }

    func accountState() async throws -> SyncAccountState { try await base.accountState() }
    func accountIdentity() async throws -> String? { try await base.accountIdentity() }
    func prepareZone() async throws { try await base.prepareZone() }
    func ensureSubscription() async throws { try await base.ensureSubscription() }

    func fetchChanges(since: Data?) async throws -> ZonePage {
        var serverToken: CKServerChangeToken?
        if let since {
            // Class-constrained secure decode: tampered bytes expire the cursor.
            guard let decoded = try? NSKeyedUnarchiver.unarchivedObject(ofClass: CKServerChangeToken.self, from: since) else {
                throw SyncTransportError.changeTokenExpired
            }
            serverToken = decoded
        }
        do {
            let changes = try await database.recordZoneChanges(inZoneWith: zoneID, since: serverToken)
            var records: [ZoneRecord] = []
            var failed: [String] = []
            var undecodable: [String] = []
            var retryAfter: Double?
            for (id, result) in changes.modificationResultsByID {
                // A per-record failure must never hide behind the advanced cursor.
                let ck: CKRecord
                do { ck = try result.get().record } catch {
                    failed.append(id.recordName)
                    if let d = SyncTransportError(error).retryDelay { retryAfter = max(retryAfter ?? 0, d) }
                    continue
                }
                do { records.append(ZoneRecord(record: try CloudKitRecordCoding.decode(ck), changeTag: Self.tag(ck))) } catch {
                    undecodable.append(id.recordName)
                }
            }
            let archived = try NSKeyedArchiver.archivedData(withRootObject: changes.changeToken, requiringSecureCoding: true)
            return ZonePage(records: records, deleted: changes.deletions.map(\.recordID.recordName), token: archived,
                            moreComing: changes.moreComing, failed: failed.sorted(), failedRetryAfter: retryAfter,
                            undecodable: undecodable.sorted())
        } catch { throw SyncTransportError(error) }
    }

    func record(_ name: String) async throws -> ZoneRecord? {
        do {
            let ck = try await database.record(for: CKRecord.ID(recordName: name, zoneID: zoneID))
            return ZoneRecord(record: try CloudKitRecordCoding.decode(ck), changeTag: Self.tag(ck))
        } catch let error as CKError where error.code == .unknownItem {
            return nil
        } catch { throw SyncTransportError(error) }
    }

    func save(_ writes: [ZoneWrite], conditional: Bool) async throws -> [String: ZoneSave] {
        var out: [String: ZoneSave] = [:]
        var records: [CKRecord] = []
        var staged: [URL] = []
        defer { for url in staged { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) } }
        for w in writes {
            do {
                let ck: CKRecord
                if let tag = w.changeTag {
                    ck = try Self.record(fromTag: tag, type: w.recordType, name: w.recordName, zoneID: zoneID)
                } else if conditional {
                    // No tag: this must be an insert; an existing record answers serverRecordChanged.
                    ck = CKRecord(recordType: w.recordType, recordID: CKRecord.ID(recordName: w.recordName, zoneID: zoneID))
                } else {
                    // Authoritative overwrite (tombstones): attach to the existing record, if any.
                    ck = try await database.record(for: CKRecord.ID(recordName: w.recordName, zoneID: zoneID))
                }
                staged += try CloudKitRecordCoding.apply(w.fields, to: ck)
                records.append(ck)
            } catch let error as CKError where error.code == .unknownItem && !conditional {
                let ck = CKRecord(recordType: w.recordType, recordID: CKRecord.ID(recordName: w.recordName, zoneID: zoneID))
                do {
                    staged += try CloudKitRecordCoding.apply(w.fields, to: ck)
                    records.append(ck)
                } catch { out[w.recordName] = .failed(SyncTransportError(error)) }
            } catch { out[w.recordName] = .failed(SyncTransportError(error)) }
        }
        guard !records.isEmpty else { return out }
        do {
            let result = try await database.modifyRecords(
                saving: records, deleting: [],
                savePolicy: conditional ? .ifServerRecordUnchanged : .allKeys, atomically: false
            )
            for ck in records {
                let name = ck.recordID.recordName
                switch result.saveResults[ck.recordID] {
                case .success?: out[name] = .saved
                case let .failure(error)?:
                    if let ckError = error as? CKError, ckError.code == .serverRecordChanged {
                        let server = ckError.serverRecord.flatMap { s in
                            (try? CloudKitRecordCoding.decode(s)).map { ZoneRecord(record: $0, changeTag: Self.tag(s)) }
                        }
                        out[name] = .conflict(server)
                    } else {
                        out[name] = .failed(SyncTransportError(error))
                    }
                case nil: out[name] = .failed(.other(code: -1, message: "no result for record"))
                }
            }
        } catch { throw SyncTransportError(error) }
        return out
    }

    /// Archived `CKRecord` system fields (record ID, change tag, zone): everything a later
    /// conditional save needs, and nothing that is user content.
    static func tag(_ record: CKRecord) -> String {
        let coder = NSKeyedArchiver(requiringSecureCoding: true)
        record.encodeSystemFields(with: coder)
        coder.finishEncoding()
        return coder.encodedData.base64EncodedString()
    }

    static func record(fromTag tag: String, type: String, name: String, zoneID: CKRecordZone.ID) throws -> CKRecord {
        guard let data = Data(base64Encoded: tag),
              let coder = try? NSKeyedUnarchiver(forReadingFrom: data)
        else { throw SyncTransportError.other(code: -3, message: "unreadable change tag") }
        coder.requiresSecureCoding = true
        guard let record = CKRecord(coder: coder),
              record.recordType == type, record.recordID.recordName == name, record.recordID.zoneID == zoneID
        else { throw SyncTransportError.other(code: -3, message: "change tag does not match record") }
        return record
    }
}
#endif
