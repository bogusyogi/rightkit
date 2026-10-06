import Foundation

/// The zone I/O the engine needs. `CloudKitSyncTransport` is the production implementation;
/// `InMemorySyncTransport` implements real change-feed semantics for tests.
public protocol SyncTransport: Sendable {
    func accountState() async throws -> SyncAccountState
    /// Create the custom zone (idempotent).
    func prepareZone() async throws
    /// Create the zone subscription for silent pushes if absent (idempotent).
    func ensureSubscription() async throws
    /// One page of zone changes since `token` (nil = from the beginning).
    func fetchChanges(since token: SyncChangeToken?) async throws -> SyncChangePage
    /// Save full-field writes, non-atomically: every record gets its own outcome so one malformed
    /// row never blocks the rest. Implementations resolve server change tags themselves.
    /// Throw only when the whole request failed.
    func save(_ writes: [SyncRecordWrite]) async throws -> [String: SyncSaveOutcome]
    /// Stable opaque identity of the signed-in account, or nil when the transport has none. The
    /// engine binds its ledger to this value and resets it when it changes.
    func accountIdentity() async throws -> String?
}

public extension SyncTransport {
    func accountIdentity() async throws -> String? { nil }
}

/// In-process zone with change-token, pagination, expiry and zone-deletion semantics.
public actor InMemorySyncTransport: SyncTransport {
    private struct Token: Codable { var epoch: Int; var seq: Int }

    private var records: [String: SyncRecord] = [:]
    private var log: [(seq: Int, name: String, deleted: Bool)] = []
    private var seq = 0
    private var epoch = 0
    private var zoneExists = false
    public private(set) var subscriptionCount = 0
    public private(set) var fetchCount = 0
    public private(set) var saveCallCount = 0
    public var account: SyncAccountState = .available
    public var pageSize = 100
    /// Record names whose next save fails with the given error (cleared when consumed).
    public var failingSaves: [String: SyncTransportError] = [:]
    /// One-shot failure for the next call of that kind.
    public var failNextFetch: SyncTransportError?
    public var failNextSave: SyncTransportError?
    public var failNextSubscription: SyncTransportError?
    /// Record name -> number of fetch passes in which that record is reported as a TRANSIENT
    /// failure (the page still succeeds and its token advances, like a CloudKit per-record error).
    public var failingFetches: [String: Int] = [:]
    /// Records reported as PERMANENTLY undecodable on every fetch.
    public var undecodableRecords: Set<String> = []
    public var failingFetchRetryAfter: TimeInterval?
    public var accountID: String? = "in-memory-account"
    /// Runs inside `fetchChanges` before it reads the zone (to inject mid-sync events).
    public var onFetch: (@Sendable () async -> Void)?
    public var now: @Sendable () -> Date = { Date() }

    public init() {}

    public func setAccount(_ state: SyncAccountState) { account = state }
    public func setPageSize(_ size: Int) { pageSize = max(1, size) }
    public func setOnFetch(_ hook: (@Sendable () async -> Void)?) { onFetch = hook }

    public func setFailingSaves(_ failures: [String: SyncTransportError]) { failingSaves = failures }
    public func setFailNextFetch(_ error: SyncTransportError?) { failNextFetch = error }
    public func setFailNextSave(_ error: SyncTransportError?) { failNextSave = error }
    public func setFailingFetches(_ failures: [String: Int]) { failingFetches = failures }
    public func setUndecodableRecords(_ names: Set<String>) { undecodableRecords = names }
    public func setFailingFetchRetryAfter(_ delay: TimeInterval?) { failingFetchRetryAfter = delay }
    public func setAccountID(_ id: String?) { accountID = id }
    public func accountIdentity() async throws -> String? { accountID }
    public func setFailNextSubscription(_ error: SyncTransportError?) { failNextSubscription = error }

    // MARK: Test controls
    public func allRecords() -> [String: SyncRecord] { records }
    public func record(_ name: String) -> SyncRecord? { records[name] }

    /// Seed a record as if another device wrote it.
    public func seed(_ record: SyncRecord) {
        zoneExists = true
        put(record)
    }

    public func hardDelete(_ name: String) {
        records[name] = nil
        seq += 1
        log.append((seq, name, true))
    }

    /// Server invalidates every outstanding token.
    public func expireTokens() { epoch += 1 }

    public func deleteZone() {
        records = [:]
        log = []
        zoneExists = false
        epoch += 1
    }

    // MARK: SyncTransport
    public func accountState() async throws -> SyncAccountState { account }

    public func prepareZone() async throws { zoneExists = true }

    public func ensureSubscription() async throws {
        if let e = failNextSubscription { failNextSubscription = nil; throw e }
        subscriptionCount += 1
    }

    public func fetchChanges(since token: SyncChangeToken?) async throws -> SyncChangePage {
        fetchCount += 1
        if let onFetch { await onFetch() }
        if let e = failNextFetch { failNextFetch = nil; throw e }
        guard zoneExists else { throw SyncTransportError.zoneNotFound }
        var after = 0
        if let token {
            guard let decoded = try? JSONDecoder().decode(Token.self, from: token.data), decoded.epoch == epoch else {
                throw SyncTransportError.changeTokenExpired
            }
            after = decoded.seq
        }
        let pending = log.filter { $0.seq > after }
        // Latest event per record only, like a real change feed.
        var latest: [String: (seq: Int, deleted: Bool)] = [:]
        for e in pending { latest[e.name] = (e.seq, e.deleted) }
        let ordered = latest.sorted { $0.value.seq < $1.value.seq }
        let page = Array(ordered.prefix(pageSize))
        let last = page.last?.value.seq ?? seq
        let more = ordered.count > page.count
        let cursor = more ? last : seq
        var out: [SyncRecord] = []
        var deleted: [String] = []
        var failed: [String] = []
        var undecodable: [String] = []
        for (name, event) in page {
            if event.deleted {
                deleted.append(name)
            } else if undecodableRecords.contains(name) {
                undecodable.append(name)
            } else if let remaining = failingFetches[name], remaining > 0 {
                failingFetches[name] = remaining - 1
                failed.append(name)
            } else if let r = records[name] {
                out.append(r)
            }
        }
        let data = try JSONEncoder().encode(Token(epoch: epoch, seq: cursor))
        return SyncChangePage(records: out, deletedRecordNames: deleted, token: SyncChangeToken(data: data), moreComing: more,
                              failedRecordNames: failed, failedRecordRetryAfter: failingFetchRetryAfter,
                              undecodableRecordNames: undecodable)
    }

    public func save(_ writes: [SyncRecordWrite]) async throws -> [String: SyncSaveOutcome] {
        saveCallCount += 1
        if let e = failNextSave { failNextSave = nil; throw e }
        guard zoneExists else { throw SyncTransportError.zoneNotFound }
        var outcomes: [String: SyncSaveOutcome] = [:]
        for write in writes {
            if let e = failingSaves.removeValue(forKey: write.recordName) {
                outcomes[write.recordName] = .failed(e)
                continue
            }
            var record = records[write.recordName]
                ?? SyncRecord(recordName: write.recordName, recordType: write.recordType, createdAt: now())
            for (key, value) in write.fields {
                switch value {
                case .null, .strings([]): record.fields[key] = nil
                default: record.fields[key] = value
                }
            }
            record.modifiedAt = now()
            put(record)
            outcomes[write.recordName] = .saved
        }
        return outcomes
    }

    private func put(_ record: SyncRecord) {
        records[record.recordName] = record
        seq += 1
        log.append((seq, record.recordName, false))
    }
}
