#if os(iOS)
import Foundation
import RightKitImportQueue

/// RightKit owns payload schema, staging compatibility, durable file writes,
/// locking, retry metadata, & legacy UserDefaults migration.
public typealias PendingSharePayload = RightKitImportQueue.PendingSharePayload

/// Product adapter retaining HeardRight's no-argument queue API.
public struct SharedImportQueue: @unchecked Sendable {
    public typealias QueueError = RightKitImportQueue.SharedImportQueue.QueueError
    private static let groupID = "group.app.heardright.ios.native"
    private static let legacyQueueKey = "heardright.pending.share.payloads"
    private static let legacyPayloadKey = "heardright.pending.share.payload"

    private let queue: RightKitImportQueue.SharedImportQueue

    public init(root: URL? = nil, defaults: UserDefaults? = nil) {
        let configuration = ShareIntakeConfiguration(
            groupID: Self.groupID,
            rootOverride: root,
            defaultsOverride: defaults,
            legacyQueueKey: Self.legacyQueueKey,
            legacyPayloadKey: Self.legacyPayloadKey
        )
        queue = RightKitImportQueue.SharedImportQueue(configuration: configuration)
    }

    public func enqueue(_ payload: PendingSharePayload) throws {
        try queue.enqueue(payload)
    }

    public func pending() throws -> [PendingSharePayload] {
        try queue.pending()
    }

    public func acknowledge(_ id: UUID) throws {
        try queue.acknowledge(id)
    }

    public func markFailed(_ id: UUID, error: Error) throws {
        try queue.markFailed(id, error: error)
    }
}
#endif
