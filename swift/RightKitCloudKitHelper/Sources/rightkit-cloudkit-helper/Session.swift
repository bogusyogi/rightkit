// Protocol handler for `rightkit-sync-helper/1`: one JSON request per line in, one JSON response
// per line out. Stateless across restarts except the change-tag cache inside the transport and the
// per-record field-name memory below; the change cursor lives in the Rust ledger (the token travels
// in every fetch_changes request/response as base64), so nothing here needs migrating.
//
// Commands (all except `status` carry `expected_scope`, the account/container/zone scope the Rust
// ledger belongs to; a mismatch refuses the request before any I/O and again after it):
//   status                          -> {status:"ready"|"unavailable", detail?, scope?}
//   prepare                         -> {subscription_error?}
//   fetch_changes {since}           -> {records, deleted, token, more_coming, failed_records,
//                                       failed_retry_after_secs?, undecodable_records}
//   fetch_record {record_name}      -> {record|null}
//   save_records {records, policy}  -> {outcomes:[{name, ok, error?}]}
//   shutdown                        -> {}

import Foundation
import RightKitCloudSync
#if canImport(CloudKit)
import CloudKit
#endif

struct WireFailure: Error {
    var kind: String
    var message: String
    var retryAfter: Double?

    init(_ kind: String, _ message: String, retryAfter: Double? = nil) {
        self.kind = kind
        self.message = message
        self.retryAfter = retryAfter
    }

    init(_ error: SyncTransportError) {
        switch error {
        case let .networkUnavailable(hint):
            self.init("network", "network unavailable", retryAfter: error.retryDelay ?? hint)
        case .busy:
            self.init("busy", "CloudKit is busy or rate limiting", retryAfter: error.retryDelay)
        case .notAuthenticated: self.init("not_authenticated", "Sign in to iCloud on this Mac to sync.")
        case .quotaExceeded: self.init("quota_exceeded", "iCloud storage is full.")
        case .permissionDenied: self.init("permission_denied", "CloudKit permission denied.")
        case .changeTokenExpired: self.init("token_expired", "change token expired")
        case .zoneNotFound: self.init("zone_missing", "sync zone missing")
        case .unknownItem: self.init("transient", "unknown item")
        case let .other(code, message): self.init("transient", "CloudKit error \(code): \(message)")
        }
    }

    var json: [String: Any] {
        var out: [String: Any] = ["kind": kind, "message": message]
        if let retryAfter { out["retry_after_secs"] = retryAfter }
        return out
    }
}

actor HelperSession {
    private let transport: any ZoneIO
    /// Field names last seen per record name. CloudKit saves with `.allKeys` overwrite the fields
    /// they carry only; a tombstone must also clear the content fields the record already holds,
    /// so absent-but-known fields are sent as `.null`.
    private var knownFields: [String: Set<String>] = [:]
    /// Memory backend only: lets a test play another device writing to the zone.
    private let debugWriter: MemoryZone?

    init(transport: any ZoneIO, debugWriter: MemoryZone? = nil) {
        self.transport = transport
        self.debugWriter = debugWriter
    }

    /// Returns the response line, plus whether to stop after sending it.
    func handle(line: String) async -> (response: String, stop: Bool) {
        guard let data = line.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = (object["id"] as? NSNumber)?.uint64Value,
              let cmd = object["cmd"] as? String
        else {
            return (Self.failureLine(id: 0, WireFailure("transient", "malformed request")), false)
        }
        do {
            var body: [String: Any]
            switch cmd {
            case "status": body = try await status()
            case "shutdown": return (Self.okLine(id: id, [:]), true)
            default:
                guard let expected = object["expected_scope"] as? String, !expected.isEmpty else {
                    throw WireFailure("transient", "missing expected scope")
                }
                try await checkScope(expected)
                switch cmd {
                case "prepare": body = try await prepare()
                case "fetch_changes": body = try await fetchChanges(object)
                case "fetch_record": body = try await fetchRecord(object)
                case "debug_write": body = try await debugWrite(object)
                case "save_records": body = try await saveRecords(object)
                default: throw WireFailure("transient", "unknown command \(cmd)")
                }
                // The account may have moved while the request ran: never hand back results that
                // belong to a different scope.
                try await checkScope(expected)
                body["scope"] = expected
            }
            return (Self.okLine(id: id, body), false)
        } catch let failure as WireFailure {
            return (Self.failureLine(id: id, failure), false)
        } catch let error as SyncTransportError {
            return (Self.failureLine(id: id, WireFailure(error)), false)
        } catch {
            return (Self.failureLine(id: id, WireFailure("transient", error.localizedDescription)), false)
        }
    }

    // MARK: Commands

    private func status() async throws -> [String: Any] {
        let state: SyncAccountState
        do { state = try await transport.accountState() } catch {
            return ["status": "unavailable", "detail": "iCloud is unavailable right now: \(error.localizedDescription)"]
        }
        let detail: String?
        switch state {
        case .available: detail = nil
        case .noAccount: detail = "Sign in to iCloud on this Mac to sync."
        case .restricted: detail = "iCloud is restricted on this Mac."
        case .couldNotDetermine: detail = "iCloud status could not be determined yet."
        case .temporarilyUnavailable: detail = "iCloud is temporarily unavailable."
        }
        if let detail { return ["status": "unavailable", "detail": detail] }
        var body: [String: Any] = ["status": "ready"]
        // Fail closed: an unreadable identity means no scope, so no request can pass the check.
        if let scope = try await transport.accountIdentity() { body["scope"] = scope }
        return body
    }

    private func checkScope(_ expected: String) async throws {
        guard let live = try await transport.accountIdentity(), !live.isEmpty, live == expected else {
            throw WireFailure("scope_changed", "iCloud account changed during the sync request")
        }
    }

    private func prepare() async throws -> [String: Any] {
        try await transport.prepareZone()
        do { try await transport.ensureSubscription() } catch {
            // Silent pushes are an optimisation; the zone is usable without them.
            return ["subscription_error": "\(error)"]
        }
        return [:]
    }

    private func fetchChanges(_ request: [String: Any]) async throws -> [String: Any] {
        var token: Data?
        if let since = request["since"] as? String {
            guard let bytes = Data(base64Encoded: since) else { throw WireFailure("token_expired", "unreadable change token") }
            token = bytes
        }
        let page = try await transport.fetchChanges(since: token)
        for r in page.records { knownFields[r.record.recordName] = Set(r.record.fields.keys) }
        for name in page.deleted { knownFields[name] = nil }
        var body: [String: Any] = [
            "records": page.records.map(Self.wire),
            "deleted": page.deleted,
            "token": page.token.base64EncodedString(),
            "more_coming": page.moreComing,
            "failed_records": page.failed,
            "undecodable_records": page.undecodable,
        ]
        if let hint = page.failedRetryAfter, hint > 0 { body["failed_retry_after_secs"] = hint }
        return body
    }

    private func fetchRecord(_ request: [String: Any]) async throws -> [String: Any] {
        guard let name = request["record_name"] as? String else { throw WireFailure("transient", "record_name missing") }
        guard let record = try await transport.record(name) else { return ["record": NSNull()] }
        knownFields[name] = Set(record.record.fields.keys)
        return ["record": Self.wire(record)]
    }

    private func debugWrite(_ request: [String: Any]) async throws -> [String: Any] {
        guard let debugWriter, let record = request["record"] as? [String: Any],
              let name = record["name"] as? String, let type = record["type"] as? String
        else { throw WireFailure("transient", "debug_write unavailable") }
        await debugWriter.debugWrite(name: name, type: type, fields: try Self.decodeFields(record["fields"] as? [String: Any] ?? [:]))
        return [:]
    }

    private func saveRecords(_ request: [String: Any]) async throws -> [String: Any] {
        guard let raw = request["records"] as? [[String: Any]] else { throw WireFailure("transient", "records missing") }
        var writes: [ZoneWrite] = []
        for record in raw {
            guard let name = record["name"] as? String, let type = record["type"] as? String else {
                throw WireFailure("transient", "record without name/type")
            }
            var fields = try Self.decodeFields(record["fields"] as? [String: Any] ?? [:])
            for key in knownFields[name] ?? [] where fields[key] == nil { fields[key] = SyncValue.null }
            writes.append(ZoneWrite(recordName: name, recordType: type, fields: fields, changeTag: record["change_tag"] as? String))
        }
        // `if_server_unchanged`: never overwrite a record that changed since the tag the write is based
        // on; the conflict is reported with the server's record and Rust decides.
        let conditional = (request["policy"] as? String) == "if_server_unchanged"
        let outcomes = try await transport.save(writes, conditional: conditional)
        var results: [[String: Any]] = []
        for write in writes {
            switch outcomes[write.recordName] {
            case .saved?:
                knownFields[write.recordName] = Set(write.fields.filter { $0.value != .null }.keys)
                results.append(["name": write.recordName, "ok": true])
            case let .conflict(server)?:
                var entry: [String: Any] = ["name": write.recordName, "ok": false,
                                            "error": WireFailure("conflict", "server record changed").json]
                if let server {
                    entry["server_record"] = Self.wire(server)
                    knownFields[write.recordName] = Set(server.record.fields.keys)
                }
                results.append(entry)
            case let .failed(error)?:
                results.append(["name": write.recordName, "ok": false, "error": WireFailure(error).json])
            case nil:
                results.append(["name": write.recordName, "ok": false, "error": WireFailure("transient", "no result for record").json])
            }
        }
        return ["outcomes": results]
    }

    // MARK: Wire mapping

    private static func ms(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1000).rounded()) }

    static func wire(_ zoneRecord: ZoneRecord) -> [String: Any] {
        var out = wire(zoneRecord.record)
        if let tag = zoneRecord.changeTag { out["change_tag"] = tag }
        return out
    }

    static func wire(_ record: SyncRecord) -> [String: Any] {
        var fields: [String: Any] = [:]
        for (key, value) in record.fields {
            switch value {
            case let .string(v): fields[key] = ["t": "text", "v": v]
            case let .int(v): fields[key] = ["t": "i64", "v": v]
            case let .double(v): fields[key] = ["t": "f64", "v": v]
            case let .date(v): fields[key] = ["t": "date", "v": ms(v)]
            case let .data(v), let .asset(v): fields[key] = ["t": "bytes", "v": v.base64EncodedString()]
            case let .strings(v): fields[key] = ["t": "strings", "v": v]
            case .null: break
            }
        }
        var out: [String: Any] = ["type": record.recordType, "name": record.recordName, "fields": fields]
        if let m = record.modifiedAt { out["modified_ms"] = ms(m) }
        if let c = record.createdAt { out["created_ms"] = ms(c) }
        return out
    }

    static func decodeFields(_ raw: [String: Any]) throws -> [String: SyncValue] {
        var out: [String: SyncValue] = [:]
        for (key, value) in raw {
            guard let box = value as? [String: Any], let tag = box["t"] as? String else {
                throw WireFailure("transient", "field \(key) malformed")
            }
            let v = box["v"]
            switch tag {
            case "text": guard let s = v as? String else { throw bad(key) }; out[key] = .string(s)
            case "i64": guard let n = v as? NSNumber else { throw bad(key) }; out[key] = .int(n.int64Value)
            case "f64": guard let n = v as? NSNumber else { throw bad(key) }; out[key] = .double(n.doubleValue)
            case "date":
                guard let n = v as? NSNumber else { throw bad(key) }
                out[key] = .date(Date(timeIntervalSince1970: n.doubleValue / 1000))
            // Bytes are CloudKit assets: no per-record size limit, the shared contract's choice.
            case "bytes":
                guard let s = v as? String, let d = Data(base64Encoded: s) else { throw bad(key) }
                out[key] = .asset(d)
            case "strings": guard let a = v as? [String] else { throw bad(key) }; out[key] = .strings(a)
            default: throw bad(key)
            }
        }
        return out
    }

    private static func bad(_ key: String) -> WireFailure { WireFailure("transient", "field \(key) has a bad value") }

    // MARK: Output

    static func okLine(id: UInt64, _ body: [String: Any]) -> String {
        var out = body
        out["id"] = id
        out["ok"] = true
        return encode(out)
    }

    static func failureLine(id: UInt64, _ failure: WireFailure) -> String {
        encode(["id": id, "ok": false, "error": failure.json])
    }

    private static func encode(_ object: [String: Any]) -> String {
        guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.withoutEscapingSlashes]),
              let text = String(data: data, encoding: .utf8)
        else { return #"{"id":0,"ok":false,"error":{"kind":"transient","message":"failed to encode response"}}"# }
        return text
    }
}
