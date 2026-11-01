import Foundation
import RightKitCloudSync

/// User-owned iCloud sync for transcript text and organization metadata.
/// Audio never leaves the device. RightKit owns CloudKit transport, cursors,
/// tombstones, retries, account fences, and subscription setup.
enum TranscriptCloudSyncStatus: Equatable, Sendable {
    case disabled
    case unavailable(String)
    case ready
}

struct TranscriptCloudSyncResult: Equatable, Sendable {
    let uploaded: Int
    let downloaded: Int
    let status: TranscriptCloudSyncStatus
}

enum TranscriptCloudSyncError: LocalizedError {
    case persistence(String)
    case malformedRecord(String)

    var errorDescription: String? {
        switch self {
        case let .persistence(message), let .malformedRecord(message): message
        }
    }
}

/// HeardRight's schema and merge policy; RightKit owns all generic sync state.
private struct TranscriptCloudAdapter: CloudSyncAdapter, @unchecked Sendable {
    typealias Item = HRLibraryTranscript

    let library: TranscriptLibrary
    let sharedStore: SharedTranscriptStore
    let recordType = TranscriptCloudSync.recordType
    let conflictPolicy: SyncConflictPolicy = .lastWriterWins

    func itemID(of item: HRLibraryTranscript) -> String { item.id.uuidString }
    func version(of item: HRLibraryTranscript) -> Date { item.updatedAt }
    func createdAt(of item: HRLibraryTranscript) -> Date { item.createdAt }
    func recordName(forItemID id: String) -> String { id }
    func itemID(forDeletedRecordName name: String) -> String? {
        name.isEmpty ? nil : TranscriptCloudRecordIdentity.logicalID(name).uuidString
    }

    func encode(_ item: HRLibraryTranscript) -> [String: SyncValue] {
        var metadata = item.metadata
        metadata[TranscriptCloudSync.originMetadataKey] = item.originDeviceID ?? HeardRightShared.deviceID
        let metadataData = try? JSONEncoder().encode(metadata)
        var fields: [String: SyncValue] = [
            "id": .string(item.id.uuidString),
            "text": .string(item.text),
            "rawText": .string(item.rawText),
            "createdAt": .date(item.createdAt),
            "updatedAt": .date(item.updatedAt),
            "source": .string(item.source),
            "folder": item.folder.map(SyncValue.string) ?? .null,
            "tags": item.tags.isEmpty ? .null : .strings(item.tags),
        ]
        if let metadataData { fields["metadataJSON"] = .data(metadataData) }
        return fields
    }

    func decode(_ record: SyncRecord) throws -> SyncRemoteChange<HRLibraryTranscript> {
        let fallbackDate = record.modifiedAt ?? record.createdAt ?? Date()
        let rawID = record.fields["id"]?.stringValue.flatMap { $0.isEmpty ? nil : $0 } ?? record.recordName
        guard !rawID.isEmpty,
              let updatedAt = record.fields["updatedAt"]?.dateValue ?? (record.modifiedAt ?? record.createdAt)
        else {
            throw TranscriptCloudSyncError.malformedRecord("CloudKit record \(record.recordName) is missing id or updatedAt")
        }
        // Same product identity as desktop & prior iOS builds: prefixed UUIDs
        // and older record-name-only rows must not be quarantined or forked.
        let id = TranscriptCloudRecordIdentity.logicalID(rawID)
        if let deletedAt = record.fields["deletedAt"]?.dateValue {
            return .tombstone(itemID: id.uuidString, version: deletedAt)
        }
        guard let text = record.fields["text"]?.stringValue,
              let rawText = record.fields["rawText"]?.stringValue
        else {
            throw TranscriptCloudSyncError.malformedRecord("CloudKit record \(record.recordName) is missing transcript fields")
        }
        let createdAt = record.fields["createdAt"]?.dateValue ?? record.createdAt ?? fallbackDate
        let metadata: [String: String]
        if let data = record.fields["metadataJSON"]?.dataValue {
            guard let decoded = try? JSONDecoder().decode([String: String].self, from: data) else {
                throw TranscriptCloudSyncError.malformedRecord("CloudKit record \(record.recordName) has invalid metadata")
            }
            metadata = decoded
        } else if record.fields["metadataJSON"] == nil {
            metadata = [:]
        } else {
            throw TranscriptCloudSyncError.malformedRecord("CloudKit record \(record.recordName) has invalid metadata")
        }
        let origin = metadata[TranscriptCloudSync.originMetadataKey]
            ?? record.fields["originDeviceID"]?.stringValue
        var userMetadata = metadata
        userMetadata.removeValue(forKey: TranscriptCloudSync.originMetadataKey)
        let tags: [String]
        if let value = record.fields["tags"] {
            guard let decoded = value.stringsValue else {
                throw TranscriptCloudSyncError.malformedRecord("CloudKit record \(record.recordName) has invalid tags")
            }
            tags = decoded
        } else {
            tags = []
        }
        let transcript = HRLibraryTranscript(
            id: id,
            text: text,
            rawText: rawText,
            createdAt: createdAt,
            updatedAt: updatedAt,
            source: record.fields["source"]?.stringValue ?? "iPhone",
            audioRelativePath: nil,
            tags: tags,
            folder: record.fields["folder"]?.stringValue,
            metadata: userMetadata,
            originDeviceID: origin
        )
        return .upsert(transcript)
    }

    func tombstoneFields(itemID: String, deletedAt: Date) -> [String: SyncValue] {
        SyncTombstone.fields(
            idKey: "id",
            itemID: itemID,
            deletedAt: deletedAt,
            deletedAtKey: "deletedAt",
            updatedAtKey: "updatedAt",
            clearing: ["text", "rawText", "createdAt", "source", "tags", "folder", "metadataJSON"]
        )
    }

    func resolve(local: HRLibraryTranscript, remote: HRLibraryTranscript) -> SyncResolution {
        if remote.updatedAt < local.updatedAt { return .uploadLocal }
        if remote.updatedAt > local.updatedAt { return .applyRemote }
        // Preserve existing equal-version provenance repair for local phone/Watch rows.
        let source = local.source.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard source == "iphone" || source == "watch",
              local.originDeviceID == nil || local.originDeviceID == HeardRightShared.deviceID
        else { return .keepBoth }
        if remote.source.caseInsensitiveCompare(local.source) != .orderedSame { return .uploadLocal }
        if let origin = local.originDeviceID, remote.originDeviceID != origin { return .uploadLocal }
        return .keepBoth
    }

    func loadLocal() async throws -> [HRLibraryTranscript] {
        let blocked = Set(sharedStore.transcriptTombstones.keys).union(sharedStore.legacyLocalDeletionIDs)
        let current = library.all()
        let retained = current.filter { !blocked.contains($0.id) }
        if retained.count != current.count {
            guard library.replaceAll(retained, mutationOrigin: .cloudMerge, cloudTombstones: {
                Set(sharedStore.transcriptTombstones.keys).union(sharedStore.legacyLocalDeletionIDs)
            }) else {
                throw TranscriptCloudSyncError.persistence("couldn't remove locally deleted transcripts")
            }
        }
        return retained
    }

    func apply(upserts: [HRLibraryTranscript], deletedIDs: Set<String>) async throws {
        let ids = Set(deletedIDs.compactMap(UUID.init(uuidString:)))
        if !ids.isEmpty {
            guard sharedStore.applyCloudTranscriptDeletions(ids) == ids.count else {
                throw TranscriptCloudSyncError.persistence("couldn't persist downloaded transcript deletions")
            }
        }
        // Snapshot after CloudKit awaits. A local edit may have landed while
        // this page was in flight; keep newer local versions at commit time.
        var merged = library.all().filter { !ids.contains($0.id) }
        for remote in upserts where !ids.contains(remote.id) {
            if let local = merged.first(where: { $0.id == remote.id }), local.updatedAt > remote.updatedAt {
                continue
            }
            merged.removeAll { $0.id == remote.id }
            merged.append(remote)
        }
        guard library.replaceAll(merged, mutationOrigin: .cloudMerge, cloudTombstones: {
            Set(sharedStore.transcriptTombstones.keys).union(sharedStore.legacyLocalDeletionIDs)
        }) else {
            throw TranscriptCloudSyncError.persistence("couldn't save synced transcripts")
        }
    }

    func accountDidSwitch(from _: String, to current: String) async throws {
        guard sharedStore.activateCloudScope(current) else {
            throw TranscriptCloudSyncError.persistence("CloudKit account scope did not persist")
        }
    }
}

actor TranscriptCloudSync {
    static let shared = TranscriptCloudSync()

    static let containerIdentifier = "iCloud.app.heardright.ios.native"
    static let zoneName = "HeardRightTranscripts"
    static let subscriptionID = "heardright.transcripts.subscription.v1"
    static let recordType = "Transcript"
    static let enabledKey = "heardright.icloud.sync.enabled"
    static let deletionKey = "heardright.icloud.sync.deletedIDs"
    static let originMetadataKey = "heardright.originDeviceID"

    private let library: TranscriptLibrary
    private let sharedStore: SharedTranscriptStore
    private let defaults: UserDefaults
    private let makeTransport: () -> any SyncTransport
    private let stateURL: URL
    private let enabledStore: UserDefaultsSyncEnabledStore
    private var transport: (any SyncTransport)?
    private var engine: CloudSyncEngine<TranscriptCloudAdapter>?
    private let messages = SyncMessages(appName: "HeardRight")
    private var generation: UInt64 = 0

    init(
        defaults: UserDefaults = .standard,
        library: TranscriptLibrary = .shared,
        sharedStore: SharedTranscriptStore = SharedTranscriptStore(),
        applicationSupportRoot: URL? = nil,
        transport: (any SyncTransport)? = nil
    ) {
        self.library = library
        self.sharedStore = sharedStore
        self.defaults = defaults
        self.makeTransport = {
            transport ?? CloudKitSyncTransport(
                containerIdentifier: Self.containerIdentifier,
                zoneName: Self.zoneName,
                subscriptionID: Self.subscriptionID
            )
        }
        self.enabledStore = UserDefaultsSyncEnabledStore(key: Self.enabledKey, defaults: defaults)
        let root = applicationSupportRoot
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        self.stateURL = root.appendingPathComponent("HeardRight", isDirectory: true)
            .appendingPathComponent("cloud-sync-state.json")
    }

    private func ensureTransport() -> any SyncTransport {
        if let transport { return transport }
        let transport = makeTransport()
        self.transport = transport
        return transport
    }

    private func ensureEngine() -> CloudSyncEngine<TranscriptCloudAdapter> {
        if let engine { return engine }
        let transport = ensureTransport()
        var configuration = CloudSyncConfiguration()
        configuration.publication = .sequentialOldestFirst
        configuration.messages = messages
        let engine = CloudSyncEngine(
            adapter: TranscriptCloudAdapter(library: library, sharedStore: sharedStore),
            transport: transport,
            stateStore: FileSyncStateStore(fileURL: stateURL),
            enabledStore: enabledStore,
            configuration: configuration
        )
        self.engine = engine
        return engine
    }

    var isEnabled: Bool { enabledStore.isEnabled }
    func setEnabled(_ enabled: Bool) async {
        if enabled != enabledStore.isEnabled { generation &+= 1 }
        if let engine {
            await engine.setEnabled(enabled)
        } else {
            enabledStore.setEnabled(enabled)
        }
    }
    func accountDidChange() async {
        generation &+= 1
        _ = sharedStore.deactivateCloudScope(ifMatches: sharedStore.cloudSyncScope)
        if let engine { await engine.accountDidChange() }
    }
    func retryDelayRemaining() async -> TimeInterval? {
        guard let engine else { return nil }
        return await engine.retryDelayRemaining()
    }

    func status() async -> TranscriptCloudSyncStatus {
        guard enabledStore.isEnabled else { return .disabled }
        do {
            switch try await ensureTransport().accountState() {
            case .available: return .ready
            case .noAccount: return .unavailable(messages.noAccount)
            case .restricted: return .unavailable(messages.restricted)
            case .couldNotDetermine: return .unavailable(messages.undetermined)
            case .temporarilyUnavailable: return .unavailable(messages.temporarilyUnavailable)
            }
        } catch {
            return .unavailable(messages.failure(for: error))
        }
    }

    func sync() async -> TranscriptCloudSyncResult {
        guard enabledStore.isEnabled else {
            return TranscriptCloudSyncResult(uploaded: 0, downloaded: 0, status: .disabled)
        }
        let operationGeneration = generation
        let engine = ensureEngine()
        let transport = ensureTransport()
        let scope: String?
        do {
            // RightKit binds its first ledger pass directly to account identity;
            // activate HeardRight's shared deletion scope before that pass so
            // first-run tombstones use same account fence as later switches.
            scope = try await transport.accountIdentity()
            guard enabledStore.isEnabled, generation == operationGeneration else {
                return TranscriptCloudSyncResult(uploaded: 0, downloaded: 0, status: .disabled)
            }
            if let scope, !sharedStore.activateCloudScope(scope) {
                throw TranscriptCloudSyncError.persistence("CloudKit account scope did not persist")
            }
        } catch {
            return TranscriptCloudSyncResult(uploaded: 0, downloaded: 0, status: .unavailable(messages.failure(for: error)))
        }
        // Seed RightKit's durable tombstone queue from HeardRight's prior
        // shared queue and legacy scoped defaults before first migration pass.
        var pending = sharedStore.pendingCloudDeletionIDs
        if let scope {
            pending.formUnion((defaults.stringArray(forKey: "\(Self.deletionKey).\(scope)") ?? []).compactMap(UUID.init(uuidString:)))
        }
        // markDeleted persists against the engine's CURRENT account. Binding
        // an unowned ledger or switching accounts deliberately drops queued
        // unscoped deletes, so bind through its public sync entry point before
        // importing HeardRight's already-account-scoped queue.
        var preparation: SyncResult?
        if !pending.isEmpty, let scope, await engine.snapshot.accountID != scope {
            let result = await engine.sync()
            guard enabledStore.isEnabled, generation == operationGeneration else {
                return TranscriptCloudSyncResult(uploaded: 0, downloaded: 0, status: .disabled)
            }
            guard case .ready = result.status, await engine.snapshot.accountID == scope else {
                return productResult(result)
            }
            preparation = result
        }
        var durablyQueued = Set<UUID>()
        for id in pending {
            guard enabledStore.isEnabled, generation == operationGeneration else {
                return TranscriptCloudSyncResult(uploaded: 0, downloaded: 0, status: .disabled)
            }
            do {
                try await engine.markDeleted(id.uuidString)
                guard enabledStore.isEnabled, generation == operationGeneration else {
                    return TranscriptCloudSyncResult(uploaded: 0, downloaded: 0, status: .disabled)
                }
                durablyQueued.insert(id)
            } catch {
                return TranscriptCloudSyncResult(
                    uploaded: 0,
                    downloaded: 0,
                    status: .unavailable("iCloud sync will retry. (deletion queue: \(String(error.localizedDescription.prefix(100))))")
                )
            }
        }
        guard enabledStore.isEnabled, generation == operationGeneration else {
            return TranscriptCloudSyncResult(uploaded: 0, downloaded: 0, status: .disabled)
        }
        let result = await engine.sync()
        guard enabledStore.isEnabled, generation == operationGeneration else {
            return TranscriptCloudSyncResult(uploaded: 0, downloaded: 0, status: .disabled)
        }
        // RightKit clears its own pending set after successful writes; mirror
        // that acknowledgement into HeardRight's shared and legacy queues.
        let snapshot = await engine.snapshot
        let remaining = Set(snapshot.pendingDeletions.compactMap(UUID.init(uuidString:)))
        guard enabledStore.isEnabled, generation == operationGeneration else {
            return TranscriptCloudSyncResult(uploaded: 0, downloaded: 0, status: .disabled)
        }
        // RightKit need not call apply for a tombstone whose row is already
        // absent from this library. Extensions still need the same durable
        // suppression, otherwise their old fast-store row can resurrect it.
        if snapshot.accountID == scope {
            let confirmed = Set(snapshot.tombstones.keys.compactMap(UUID.init(uuidString:))).subtracting(remaining)
            let missing = confirmed.filter {
                sharedStore.transcriptTombstones[$0] == nil || sharedStore.pendingCloudDeletionIDs.contains($0)
            }
            if !missing.isEmpty {
                let aliases = Dictionary(uniqueKeysWithValues: missing.map { ($0, snapshot.aliases[$0.uuidString] ?? []) })
                guard sharedStore.applyCloudTranscriptDeletions(missing, aliases: aliases) == missing.count else {
                    return TranscriptCloudSyncResult(
                        uploaded: result.uploaded, downloaded: result.downloaded,
                        status: .unavailable("iCloud sync will retry. (shared deletion ledger couldn't persist)")
                    )
                }
            }
        }
        // An empty pending set alone is not an acknowledgement: account
        // binding can discard it. Require same-account retained tombstones.
        let acknowledged: Set<UUID> = snapshot.accountID == scope
            ? durablyQueued.subtracting(remaining).filter { snapshot.tombstones[$0.uuidString] != nil }
            : []
        if !acknowledged.isEmpty {
            _ = sharedStore.acknowledgeCloudDeletions(acknowledged)
            if let scope {
                let key = "\(Self.deletionKey).\(scope)"
                let old = Set((defaults.stringArray(forKey: key) ?? []).compactMap(UUID.init(uuidString:)))
                defaults.set(old.subtracting(acknowledged).map(\.uuidString).sorted(), forKey: key)
            }
        }
        return productResult(result, preparation: preparation)
    }

    private func productResult(_ result: SyncResult, preparation: SyncResult? = nil) -> TranscriptCloudSyncResult {
        let status: TranscriptCloudSyncStatus
        switch result.status {
        case .disabled: status = .disabled
        case let .unavailable(message): status = .unavailable(message)
        case .ready: status = .ready
        }
        return TranscriptCloudSyncResult(
            uploaded: result.uploaded + (preparation?.uploaded ?? 0),
            downloaded: result.downloaded + (preparation?.downloaded ?? 0), status: status
        )
    }

    func markDeleted(_ id: UUID) async -> Bool {
        let operationGeneration = generation
        guard sharedStore.markTranscriptsDeleted([id]) == 1 else { return false }
        guard let engine, let scope = sharedStore.cloudSyncScope else { return true }
        let boundAccount = await engine.snapshot.accountID
        guard generation == operationGeneration else { return false }
        // Otherwise leave it in HeardRight's durable queue until sync binds
        // the right account. Transition deletes stay local-only.
        guard boundAccount == scope else { return true }
        do {
            try await engine.markDeleted(id.uuidString)
            return generation == operationGeneration
        } catch {
            return false
        }
    }
}
