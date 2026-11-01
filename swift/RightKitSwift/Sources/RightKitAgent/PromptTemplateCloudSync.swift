#if os(iOS)
import Foundation
import RightKitCloudSync

/// Opt-in iCloud sync for prompt templates, modelled on the app's transcript sync.
/// RightKit owns zone, subscription, change tokens, tombstone propagation, and retries;
/// this file owns the `PromptTemplate` record mapping and the local merge. Sync is OFF
/// until the consent flag is set, and every path no-ops while it is off.
enum PromptTemplateCloudSyncStatus: Equatable, Sendable {
    case disabled
    case unavailable(String)
    case ready
}

struct PromptTemplateCloudSyncResult: Equatable, Sendable {
    let uploaded: Int
    let downloaded: Int
    let status: PromptTemplateCloudSyncStatus
}

private struct PromptTemplateCloudAdapter: CloudSyncAdapter {
    typealias Item = LocalPromptTemplate

    let store: LocalPromptTemplateStore
    let recordType = PromptTemplateCloudSync.recordType
    let conflictPolicy: SyncConflictPolicy = .lastWriterWins

    func itemID(of item: LocalPromptTemplate) -> String { item.id.uuidString }
    func version(of item: LocalPromptTemplate) -> Date { item.updatedAt }
    // Templates carry no creation time; publication order is irrelevant for this tiny catalog.
    func createdAt(of item: LocalPromptTemplate) -> Date { item.updatedAt }
    func itemID(forDeletedRecordName name: String) -> String? {
        UUID(uuidString: name)?.uuidString
    }

    func encode(_ item: LocalPromptTemplate) -> [String: SyncValue] {
        [
            "id": .string(item.id.uuidString),
            "command": .string(item.command),
            "title": .string(item.title),
            "body": .string(item.body),
            "updatedAt": .date(item.updatedAt),
        ]
    }

    func decode(_ record: SyncRecord) throws -> SyncRemoteChange<LocalPromptTemplate> {
        let rawID = record.fields["id"]?.stringValue ?? record.recordName
        guard let id = UUID(uuidString: rawID),
              let updatedAt = record.fields["updatedAt"]?.dateValue ?? record.modifiedAt
        else {
            throw CloudSyncError.malformedRecord("CloudKit record \(record.recordName) is missing id or updatedAt")
        }
        if let deletedAt = record.fields["deletedAt"]?.dateValue {
            return .tombstone(itemID: id.uuidString, version: deletedAt)
        }
        guard let command = record.fields["command"]?.stringValue,
              let title = record.fields["title"]?.stringValue,
              let body = record.fields["body"]?.stringValue
        else {
            throw CloudSyncError.malformedRecord("CloudKit record \(record.recordName) is missing template fields")
        }
        return .upsert(LocalPromptTemplate(id: id, command: command, title: title, body: body, updatedAt: updatedAt))
    }

    func tombstoneFields(itemID: String, deletedAt: Date) -> [String: SyncValue] {
        SyncTombstone.fields(
            itemID: itemID,
            deletedAt: deletedAt,
            clearing: ["command", "title", "body"]
        )
    }

    func loadLocal() async throws -> [LocalPromptTemplate] {
        // Locally deleted rows must never be offered for upload again.
        await MainActor.run { store.templates.filter { store.tombstones[$0.id] == nil } }
    }

    func apply(upserts: [LocalPromptTemplate], deletedIDs: Set<String>) async throws {
        let ids = Set(deletedIDs.compactMap(UUID.init(uuidString:)))
        // Runs at commit time on the main actor, so a local edit that landed while the
        // page was in flight is kept by the store's newer-wins check.
        await MainActor.run { store.applyRemote(upserts: upserts, deletedIDs: ids) }
    }
}

actor PromptTemplateCloudSync {
    static let containerIdentifier = "iCloud.app.coderight.ios"
    static let zoneName = "PromptTemplates"
    static let subscriptionID = "coderight.prompt-templates.subscription.v1"
    static let recordType = "PromptTemplate"
    /// Consent flag (UserDefaults bool, absent = off).
    static let enabledKey = "coderight.icloud.promptTemplates.sync.enabled"

    private let store: LocalPromptTemplateStore
    private let transport: any SyncTransport
    private let stateStore: any CloudSyncStateStoring
    private let enabledStore: UserDefaultsSyncEnabledStore
    private var engine: CloudSyncEngine<PromptTemplateCloudAdapter>?
    private let messages = SyncMessages(appName: "CodeRight")
    private var generation: UInt64 = 0

    /// `transport` is injected so tests run against `InMemorySyncTransport`.
    init(
        store: LocalPromptTemplateStore,
        transport: any SyncTransport,
        defaults: UserDefaults = .standard,
        stateStore: (any CloudSyncStateStoring)? = nil
    ) {
        self.store = store
        self.transport = transport
        self.enabledStore = UserDefaultsSyncEnabledStore(key: Self.enabledKey, defaults: defaults)
        self.stateStore = stateStore ?? FileSyncStateStore(
            fileURL: FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("CodeRight", isDirectory: true)
                .appendingPathComponent("prompt-template-cloud-sync-state.json")
        )
    }

    /// The one place the real CloudKit transport is built. NOTHING calls this yet: the app has
    /// no iCloud entitlement or provisioned container, and constructing it would touch CloudKit.
    /// Wire it in only once entitlements ship and a settings toggle exists.
    static func makeProductionCloudKitTransport() -> any SyncTransport {
        CloudKitSyncTransport(
            containerIdentifier: containerIdentifier,
            zoneName: zoneName,
            subscriptionID: subscriptionID
        )
    }

    private func ensureEngine() -> CloudSyncEngine<PromptTemplateCloudAdapter> {
        if let engine { return engine }
        var configuration = CloudSyncConfiguration()
        configuration.messages = messages
        let engine = CloudSyncEngine(
            adapter: PromptTemplateCloudAdapter(store: store),
            transport: transport,
            stateStore: stateStore,
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
        if let engine { await engine.accountDidChange() }
    }

    /// One full pass: queue local deletions, upload newer local rows, apply remote changes.
    func sync() async -> PromptTemplateCloudSyncResult {
        let disabled = PromptTemplateCloudSyncResult(uploaded: 0, downloaded: 0, status: .disabled)
        guard enabledStore.isEnabled else { return disabled }
        let operationGeneration = generation
        let engine = ensureEngine()
        func stale() -> Bool { !enabledStore.isEnabled || generation != operationGeneration }

        let scope: String?
        do {
            scope = try await transport.accountIdentity()
        } catch {
            return PromptTemplateCloudSyncResult(uploaded: 0, downloaded: 0, status: .unavailable(messages.failure(for: error)))
        }
        guard !stale() else { return disabled }

        // RightKit drops unscoped queued deletes when it binds the ledger to an account, so a
        // never-synced ledger runs one pass first (loadLocal already hides tombstoned rows).
        var uploaded = 0
        var downloaded = 0
        let tombstoned = await MainActor.run { Set(store.tombstones.keys.map(\.uuidString)) }
        if !tombstoned.isEmpty, let scope, await engine.snapshot.accountID != scope {
            let result = await engine.sync()
            guard !stale() else { return disabled }
            uploaded += result.uploaded
            downloaded += result.downloaded
            guard case .ready = result.status, await engine.snapshot.accountID == scope else {
                return Self.productResult(result, uploaded: uploaded, downloaded: downloaded)
            }
        }
        let known = await engine.snapshot.tombstones
        for id in tombstoned where known[id] == nil {
            do { try await engine.markDeleted(id) } catch {
                return PromptTemplateCloudSyncResult(
                    uploaded: uploaded, downloaded: downloaded,
                    status: .unavailable("iCloud sync will retry. (deletion queue: \(String(error.localizedDescription.prefix(100))))")
                )
            }
            guard !stale() else { return disabled }
        }
        let result = await engine.sync()
        guard !stale() else { return disabled }
        return Self.productResult(result, uploaded: uploaded + result.uploaded, downloaded: downloaded + result.downloaded)
    }

    private static func productResult(_ result: SyncResult, uploaded: Int, downloaded: Int) -> PromptTemplateCloudSyncResult {
        let status: PromptTemplateCloudSyncStatus
        switch result.status {
        case .disabled: status = .disabled
        case let .unavailable(message): status = .unavailable(message)
        case .ready: status = .ready
        }
        return PromptTemplateCloudSyncResult(uploaded: uploaded, downloaded: downloaded, status: status)
    }
}
#endif
