import Foundation

public struct CloudSyncConfiguration: Sendable {
    public enum Publication: Sendable {
        /// Existing and new items are saved in `uploadBatchSize` batches.
        case batched
        /// New (never-published) items are saved one at a time, oldest first, and a failure halts
        /// further new publication for the pass. CloudKit gives no ordering inside one operation;
        /// use this when consumers render history by creation order.
        case sequentialOldestFirst
    }

    public var uploadBatchSize = 200
    public var deletionBatchSize = 50
    public var publication: Publication = .batched
    /// Floor for the retry gate after failed uploads or still-pending deletions.
    public var failureRetryFloor: TimeInterval = 30
    /// Subscription setup retry delays (bounded; setup never blocks data transport).
    public var subscriptionRetryDelays: [TimeInterval] = [15, 30]
    public var messages = SyncMessages()
    public var clock: @Sendable () -> Date = { Date() }
    public var sleep: @Sendable (TimeInterval) async throws -> Void = { seconds in
        try await Task.sleep(nanoseconds: UInt64(min(max(seconds, 0), 1e9) * 1_000_000_000))
    }

    public init() {}
}

/// What the coordinator drives. `CloudSyncEngine` is the implementation.
public protocol CloudSyncRunning: Sendable {
    func sync() async -> SyncResult
    func retryDelayRemaining() async -> TimeInterval?
    func accountDidChange() async
    func setEnabled(_ enabled: Bool) async
}

/// Generic CloudKit record sync: push/pull with change tokens, tombstones, conflict policy,
/// account handling and retry bookkeeping. The app supplies a `CloudSyncAdapter`.
///
/// Durable-readback contract: the change token advances only after remote tombstones, uploads and
/// the local merge all succeeded. A failed sync replays its page.
public actor CloudSyncEngine<Adapter: CloudSyncAdapter>: CloudSyncRunning {
    private struct RemoteEntry {
        let itemID: String
        let recordName: String
        let version: Date
        let item: Adapter.Item?
        var isTombstone: Bool { item == nil }
    }

    private struct Candidate { let item: Adapter.Item; let id: String; let recordName: String; let isNew: Bool }

    private struct Fetch {
        var entries: [RemoteEntry]
        var token: SyncChangeToken?
        var unresolved: Int
        /// True when the zone was read from the beginning, so absence means absence.
        var isFullReplay: Bool
    }

    private let adapter: Adapter
    private let transport: any SyncTransport
    private let stateStore: any CloudSyncStateStoring
    private let enabledStore: any SyncEnabledStoring
    private let events: any SyncEventSink
    private let config: CloudSyncConfiguration

    private var state: CloudSyncState
    private var stateLoadError: Error?
    private var retryGate = SyncRetryGate()
    private var generation: UInt64 = 0
    private var zonePrepared = false
    private var subscriptionPrepared = false
    private var subscriptionTerminal = false
    private var subscriptionTask: Task<Void, Never>?
    private var subscriptionJob: UUID?
    private var subscriptionReady: (@Sendable () -> Void)?

    public init(
        adapter: Adapter,
        transport: any SyncTransport,
        stateStore: any CloudSyncStateStoring,
        enabledStore: any SyncEnabledStoring,
        events: any SyncEventSink = NullSyncEventSink(),
        configuration: CloudSyncConfiguration = CloudSyncConfiguration()
    ) {
        self.adapter = adapter
        self.transport = transport
        self.stateStore = stateStore
        self.enabledStore = enabledStore
        self.events = events
        self.config = configuration
        do { state = try stateStore.load() } catch {
            state = CloudSyncState()
            stateLoadError = error
        }
    }

    public var isEnabled: Bool { enabledStore.isEnabled }
    public var snapshot: CloudSyncState { state }

    /// Called after the zone subscription is created, so the app can request a catch-up pass for
    /// changes committed between the first fetch and the subscription.
    public func onSubscriptionReady(_ handler: (@Sendable () -> Void)?) { subscriptionReady = handler }

    public func setEnabled(_ enabled: Bool) {
        if enabled != isEnabled { generation &+= 1 }
        enabledStore.setEnabled(enabled)
        if !enabled { retryGate.clear(); zonePrepared = false; resetSubscription() }
    }

    public func accountDidChange() {
        generation &+= 1
        zonePrepared = false
        retryGate.clear()
        resetSubscription()
    }

    public func retryDelayRemaining() -> TimeInterval? { retryGate.remaining(now: config.clock()) }

    /// Record a local delete. The tombstone blocks re-upload and is written to the server on the
    /// next pass. The app removes its own local copy.
    public func markDeleted(_ itemID: String) throws {
        let now = config.clock()
        state.tombstones[itemID] = state.tombstones[itemID] ?? now
        state.pendingDeletions.insert(itemID)
        state.remoteVersions[itemID] = nil
        try persist()
    }

    public func status() async -> SyncStatus {
        guard isEnabled else { return .disabled }
        do {
            switch try await transport.accountState() {
            case .available: return .ready
            case .noAccount: return .unavailable(config.messages.noAccount)
            case .restricted: return .unavailable(config.messages.restricted)
            case .couldNotDetermine:
                retryGate.raise(toAtLeast: 30, now: config.clock())
                return .unavailable(config.messages.undetermined)
            case .temporarilyUnavailable:
                retryGate.raise(toAtLeast: 30, now: config.clock())
                return .unavailable(config.messages.temporarilyUnavailable)
            }
        } catch {
            noteRetry(error)
            return .unavailable(config.messages.unavailable)
        }
    }

    // MARK: - Sync pass

    public func sync() async -> SyncResult {
        guard isEnabled else { return .disabled }
        let gen = generation
        let account = await status()
        guard case .ready = account else { return SyncResult(status: account) }
        do {
            if let stateLoadError {
                self.stateLoadError = nil
                throw stateLoadError
            }
            try fence(gen)
            try await prepareZoneIfNeeded()
            try fence(gen)

            let fetched = try await fetchRemote(gen: gen)
            try fence(gen)
            let remote = canonical(fetched.entries)
            for entry in fetched.entries { state.aliases[entry.itemID, default: []].insert(entry.recordName) }

            // 1. Remote tombstones win and block the item forever.
            var removedIDs = Set<String>()
            for entry in remote where entry.isTombstone {
                state.tombstones[entry.itemID] = state.tombstones[entry.itemID] ?? entry.version
                state.pendingDeletions.remove(entry.itemID)
                state.remoteVersions[entry.itemID] = nil
                removedIDs.insert(entry.itemID)
            }
            try persist()
            events.record("sync_remote_tombstones", fields: ["count": String(removedIDs.count)])

            // 2. Outbound deletions.
            try await flushDeletions(gen: gen)
            try fence(gen)
            let deletionsPending = !state.pendingDeletions.isEmpty
            if deletionsPending { retryGate.raise(toAtLeast: config.failureRetryFloor, now: config.clock()) }

            // 3. Upload. Re-read local after the suspension so a mid-sync edit is never lost.
            let local = try await adapter.loadLocal()
            let localByID = Dictionary(local.map { (adapter.itemID(of: $0), $0) }, uniquingKeysWith: { a, _ in a })
            let remoteByID = Dictionary(remote.map { ($0.itemID, $0) }, uniquingKeysWith: { a, _ in a })
            let upload = try await uploadChanges(local: local, remoteByID: remoteByID, isFullReplay: fetched.isFullReplay, gen: gen)

            // 4. Download / merge.
            try fence(gen)
            var upserts: [Adapter.Item] = []
            var deletedLocally = Set<String>()
            for entry in remote {
                if entry.isTombstone {
                    if localByID[entry.itemID] != nil { deletedLocally.insert(entry.itemID) }
                    continue
                }
                guard let item = entry.item, state.tombstones[entry.itemID] == nil else { continue }
                if let current = localByID[entry.itemID] {
                    if case .applyRemote = adapter.resolve(local: current, remote: item) { upserts.append(item) }
                } else {
                    upserts.append(item)
                }
            }
            // Blocked ids still present locally (older build, race) are removed too.
            for item in local where state.tombstones[adapter.itemID(of: item)] != nil {
                deletedLocally.insert(adapter.itemID(of: item))
            }
            if !upserts.isEmpty || !deletedLocally.isEmpty {
                do {
                    try await adapter.apply(upserts: upserts, deletedIDs: deletedLocally)
                } catch {
                    // The token and version cache must not advance past a page that was never saved.
                    events.record("sync_local_apply_failed", fields: ["error": String(error.localizedDescription.prefix(200))])
                    retryGate.raise(toAtLeast: config.failureRetryFloor, now: config.clock())
                    return SyncResult(uploaded: upload.uploaded, status: .unavailable(config.messages.localSaveFailed))
                }
            }

            // 5. Commit versions, then the token last.
            for entry in remote where !entry.isTombstone {
                state.remoteVersions[entry.itemID] = .init(recordName: entry.recordName, version: entry.version)
            }
            for (id, version) in upload.versions { state.remoteVersions[id] = version }
            if let token = fetched.token { state.changeToken = token }
            try persist()
            if fetched.unresolved > 0 {
                events.record("sync_records_quarantined", fields: ["count": String(fetched.unresolved)])
            }

            let result = SyncResult(
                uploaded: upload.uploaded, downloaded: upserts.count, deleted: deletedLocally.count, status: .ready
            )
            if upload.failed > 0 || deletionsPending {
                retryGate.raise(toAtLeast: config.failureRetryFloor, now: config.clock())
                var pending = result
                pending.status = .unavailable(config.messages.pendingChanges)
                return pending
            }
            retryGate.clear()
            return result
        } catch {
            zonePrepared = false
            if error is CancellationError { return .disabled }
            noteRetry(error)
            events.record("sync_failed", fields: [
                "error": String(error.localizedDescription.prefix(200)),
            ])
            return SyncResult(status: .unavailable(config.messages.failure(for: error)))
        }
    }

    // MARK: - Pieces

    private struct UploadOutcome {
        var uploaded = 0
        var failed = 0
        var versions: [String: CloudSyncState.RemoteVersion] = [:]
    }

    private func uploadChanges(
        local: [Adapter.Item], remoteByID: [String: RemoteEntry], isFullReplay: Bool, gen: UInt64
    ) async throws -> UploadOutcome {
        var candidates: [Candidate] = []
        for item in local {
            let id = adapter.itemID(of: item)
            guard state.tombstones[id] == nil else { continue }
            if let counterpart = remoteByID[id] {
                guard let remoteItem = counterpart.item else { continue }
                guard case .uploadLocal = adapter.resolve(local: item, remote: remoteItem) else { continue }
                candidates.append(Candidate(item: item, id: id, recordName: counterpart.recordName, isNew: false))
            } else if !isFullReplay, let known = state.remoteVersions[id] {
                // With a saved cursor, absence from this page means unchanged, not absent.
                guard known.version < adapter.version(of: item) else { continue }
                candidates.append(Candidate(item: item, id: id, recordName: known.recordName, isNew: false))
            } else {
                candidates.append(Candidate(item: item, id: id, recordName: adapter.recordName(forItemID: id), isNew: true))
            }
        }
        candidates.sort {
            let l = adapter.createdAt(of: $0.item), r = adapter.createdAt(of: $1.item)
            return l != r ? l < r : $0.id < $1.id
        }
        var batches: [[Candidate]] = []
        switch config.publication {
        case .batched:
            for start in stride(from: 0, to: candidates.count, by: config.uploadBatchSize) {
                batches.append(Array(candidates[start..<min(start + config.uploadBatchSize, candidates.count)]))
            }
        case .sequentialOldestFirst:
            batches = candidates.filter(\.isNew).map { [$0] }
            let existing = candidates.filter { !$0.isNew }
            for start in stride(from: 0, to: existing.count, by: config.uploadBatchSize) {
                batches.append(Array(existing[start..<min(start + config.uploadBatchSize, existing.count)]))
            }
        }
        var out = UploadOutcome()
        var blockedNew = false
        for batch in batches {
            let singleNew = config.publication == .sequentialOldestFirst && batch.count == 1 && batch[0].isNew
            if singleNew && blockedNew { continue }
            try fence(gen)
            // A delete may land while earlier batches were in flight.
            let live = batch.filter { state.tombstones[$0.id] == nil }
            guard !live.isEmpty else { continue }
            let writes = live.map {
                SyncRecordWrite(recordName: $0.recordName, recordType: adapter.recordType, fields: adapter.encode($0.item))
            }
            let outcomes = try await transport.save(writes)
            try fence(gen)
            for candidate in live {
                switch outcomes[candidate.recordName] ?? .failed(.other(code: -1, message: "no result")) {
                case .saved:
                    out.uploaded += 1
                    out.versions[candidate.id] = .init(
                        recordName: candidate.recordName, version: adapter.version(of: candidate.item)
                    )
                case let .failed(error):
                    out.failed += 1
                    if singleNew { blockedNew = true }
                    retryGate.note(error, now: config.clock())
                    events.record("sync_record_save_failed", fields: [
                        "record_name": candidate.recordName, "error": String(describing: error).prefix(200).description,
                    ])
                }
            }
        }
        return out
    }

    private func flushDeletions(gen: UInt64) async throws {
        guard !state.pendingDeletions.isEmpty else { return }
        var writes: [(id: String, name: String, deletedAt: Date)] = []
        for id in state.pendingDeletions.sorted() {
            var names = state.aliases[id] ?? []
            names.insert(adapter.recordName(forItemID: id))
            for name in names.sorted() {
                writes.append((id, name, state.tombstones[id] ?? config.clock()))
            }
        }
        var failed = Set<String>()
        for start in stride(from: 0, to: writes.count, by: config.deletionBatchSize) {
            try fence(gen)
            let chunk = Array(writes[start..<min(start + config.deletionBatchSize, writes.count)])
            let outcomes = try await transport.save(chunk.map {
                SyncRecordWrite(
                    recordName: $0.name, recordType: adapter.recordType,
                    fields: adapter.tombstoneFields(itemID: $0.id, deletedAt: $0.deletedAt)
                )
            })
            try fence(gen)
            for write in chunk {
                if case let .failed(error) = outcomes[write.name] ?? .failed(.other(code: -1, message: "no result")) {
                    failed.insert(write.id)
                    retryGate.note(error, now: config.clock())
                }
            }
        }
        let acknowledged = Set(writes.map(\.id)).subtracting(failed)
        state.pendingDeletions.subtract(acknowledged)
        try persist()
        if !acknowledged.isEmpty { events.record("sync_tombstones_written", fields: ["count": String(acknowledged.count)]) }
    }

    private func fetchRemote(gen: UInt64, didResetToken: Bool = false, didRecreateZone: Bool = false) async throws -> Fetch {
        var token = state.changeToken
        let isFullReplay = token == nil
        var records: [SyncRecord] = []
        var deleted: [String] = []
        do {
            var more = true
            while more {
                let page = try await transport.fetchChanges(since: token)
                try fence(gen)
                records += page.records
                deleted += page.deletedRecordNames
                token = page.token
                more = page.moreComing
            }
        } catch SyncTransportError.changeTokenExpired where state.changeToken != nil && !didResetToken {
            // Drop only cursor state and replay the zone once; never discard user data.
            state.changeToken = nil
            try persist()
            return try await fetchRemote(gen: gen, didResetToken: true, didRecreateZone: didRecreateZone)
        } catch SyncTransportError.zoneNotFound where !didRecreateZone {
            // The zone is gone: every cached server version and alias is now false.
            state.changeToken = nil
            state.remoteVersions = [:]
            state.aliases = [:]
            try persist()
            zonePrepared = false
            resetSubscription()
            try await prepareZoneIfNeeded()
            return try await fetchRemote(gen: gen, didResetToken: didResetToken, didRecreateZone: true)
        }
        var entries: [RemoteEntry] = []
        var unresolved = 0
        for record in records {
            do {
                switch try adapter.decode(record) {
                case let .upsert(item):
                    entries.append(RemoteEntry(
                        itemID: adapter.itemID(of: item), recordName: record.recordName,
                        version: adapter.version(of: item), item: item
                    ))
                case let .tombstone(itemID, version):
                    entries.append(RemoteEntry(itemID: itemID, recordName: record.recordName, version: version, item: nil))
                }
            } catch {
                // Quarantine instead of poisoning every future sync; a later server edit retries it.
                unresolved += 1
                events.record("sync_record_decode_deferred", fields: ["record_name": record.recordName])
            }
        }
        for name in deleted {
            // Hard deletes carry no fields: resolve through aliases seen on earlier syncs.
            let id = state.aliases.first { $0.value.contains(name) }?.key ?? adapter.itemID(forDeletedRecordName: name)
            guard let id else {
                unresolved += 1
                continue
            }
            entries.append(RemoteEntry(itemID: id, recordName: name, version: config.clock(), item: nil))
        }
        return Fetch(entries: entries, token: token, unresolved: unresolved, isFullReplay: isFullReplay)
    }

    /// Legacy history can hold several records for one logical id. Tombstone beats live, newer beats
    /// older, then the larger record name wins, so the result is deterministic.
    private func canonical(_ entries: [RemoteEntry]) -> [RemoteEntry] {
        var byID: [String: RemoteEntry] = [:]
        for entry in entries {
            guard let current = byID[entry.itemID] else { byID[entry.itemID] = entry; continue }
            let replace: Bool
            if entry.isTombstone != current.isTombstone {
                replace = entry.isTombstone
            } else if entry.version != current.version {
                replace = entry.version > current.version
            } else {
                replace = entry.recordName > current.recordName
            }
            if replace { byID[entry.itemID] = entry }
        }
        return byID.values.sorted { $0.itemID < $1.itemID }
    }

    private func prepareZoneIfNeeded() async throws {
        if !zonePrepared {
            let gen = generation
            try fence(gen)
            try await transport.prepareZone()
            try fence(gen)
            zonePrepared = true
        }
        startSubscriptionIfNeeded()
    }

    private func fence(_ gen: UInt64) throws {
        guard isEnabled, generation == gen else { throw CancellationError() }
    }

    private func persist() throws { try stateStore.save(state) }

    private func noteRetry(_ error: Error) {
        if let e = error as? SyncTransportError { retryGate.note(e, now: config.clock()) }
    }

    // MARK: - Subscription (bounded, independent of data transport)

    private func resetSubscription() {
        subscriptionTask?.cancel()
        subscriptionTask = nil
        subscriptionJob = nil
        subscriptionPrepared = false
        subscriptionTerminal = false
    }

    private func startSubscriptionIfNeeded() {
        guard isEnabled, !subscriptionPrepared, !subscriptionTerminal, subscriptionTask == nil else { return }
        let job = UUID()
        let gen = generation
        subscriptionJob = job
        subscriptionTask = Task { [weak self] in await self?.runSubscription(job: job, gen: gen) }
    }

    private func runSubscription(job: UUID, gen: UInt64) async {
        let delays = config.subscriptionRetryDelays
        for attempt in 0...delays.count {
            guard isEnabled, generation == gen, subscriptionJob == job, !Task.isCancelled else { return }
            do {
                try await transport.ensureSubscription()
                guard generation == gen, subscriptionJob == job else { return }
                subscriptionPrepared = true
                subscriptionTask = nil
                subscriptionJob = nil
                events.record("sync_subscription_ready", fields: [:])
                subscriptionReady?()
                return
            } catch {
                guard isEnabled, generation == gen, subscriptionJob == job, !Task.isCancelled else { return }
                events.record("sync_subscription_failed", fields: ["attempt": String(attempt)])
                let retryable = (error as? SyncTransportError)?.isRetryable ?? false
                guard retryable, attempt < delays.count else {
                    subscriptionTerminal = !retryable
                    subscriptionTask = nil
                    subscriptionJob = nil
                    return
                }
                let hint = (error as? SyncTransportError)?.retryDelay ?? 0
                do { try await config.sleep(max(hint, delays[attempt])) } catch { return }
            }
        }
    }
}
