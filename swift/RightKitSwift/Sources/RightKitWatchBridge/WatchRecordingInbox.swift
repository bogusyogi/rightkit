import Foundation

/// Receipt routing observations contain values safe to forward across concurrency domains.
public enum WatchRecordingReceiptKind: Sendable, Equatable { case acknowledgement, rollback }

public enum WatchRecordingReceiptEvent: Sendable, Equatable {
    case queued(kind: WatchRecordingReceiptKind, recordingID: String, immediateAttempted: Bool)
    case immediateFailed(kind: WatchRecordingReceiptKind, recordingID: String, error: String)
}

/// Durable receipt & result routing, independent of an app's WCSessionDelegate ownership.
/// Host delegate forwards callbacks synchronously so temporary WCSessionFile URLs stay valid.
public final class WatchRecordingInbox: Sendable {
    public let storage: WatchRecordingStorage
    public let delivery: WatchRecordingDelivery
    private let rollbackReason: @Sendable (any Error) -> String
    private let missingIdentifierRollbackID: String?
    private let activationReplayLimit: Int?
    private let observe: @Sendable (WatchRecordingReceiptEvent) -> Void

    /// Observers may run on transport callback queues. Missing sender identity sends no rollback
    /// unless the host explicitly supplies its legacy replacement ID. Replay limits never limit queries.
    public init(storage: WatchRecordingStorage, delivery: WatchRecordingDelivery = .init(),
                rollbackReason: @escaping @Sendable (any Error) -> String = { $0.localizedDescription },
                missingIdentifierRollbackID: String? = nil, activationReplayLimit: Int? = nil,
                observe: @escaping @Sendable (WatchRecordingReceiptEvent) -> Void = { _ in }) {
        self.storage = storage; self.delivery = delivery
        self.rollbackReason = rollbackReason
        self.missingIdentifierRollbackID = missingIdentifierRollbackID
        self.activationReplayLimit = activationReplayLimit.map { max(0, $0) }
        self.observe = observe
    }
    @discardableResult
    public func receive(fileURL: URL, metadata: [String: Any], transport: any WatchMessageTransport) throws -> String {
        do {
            let id = try storage.receive(fileURL: fileURL, metadata: metadata)
            // Storage's explicit receive policy owns identity. Always acknowledge its durable
            // returned ID, including an assigned UUID, never the unvalidated metadata value.
            sendReceipt(.acknowledgement, recordingID: id,
                        payload: [delivery.keys.acknowledgement: id], transport: transport)
            return id
        } catch {
            if let id = (metadata["id"] as? String) ?? missingIdentifierRollbackID {
                sendReceipt(.rollback, recordingID: id, payload: [
                    delivery.keys.rollback: id, delivery.keys.rollbackReason: rollbackReason(error),
                ], transport: transport)
            }
            throw error
        }
    }
    public func receiptReply(to message: [String: Any]) throws -> [String: Any] {
        guard let ids = message[delivery.keys.receiptQuery] as? [String] else { return [:] }
        return [delivery.keys.receipts: WatchReceipts.confirmed(
            requested: Array(ids.prefix(WatchReceipts.queryLimit)), received: Set(try storage.receivedRecordingIDs()))]
    }
    public func resendDurableAcknowledgements(transport: any WatchMessageTransport) throws {
        var records = try storage.receivedRecordings()
        if let activationReplayLimit {
            records.sort {
                if $0.receivedAt == $1.receivedAt { return $0.id < $1.id }
                return $0.receivedAt > $1.receivedAt
            }
            records = Array(records.prefix(activationReplayLimit))
        }
        let sender = DualPathSender(transport: transport)
        for record in records {
            sender.send([delivery.keys.acknowledgement: record.id])
        }
    }
    private func sendReceipt(_ kind: WatchRecordingReceiptKind, recordingID: String,
                             payload: [String: Any], transport: any WatchMessageTransport) {
        let observe = self.observe
        let attempted = DualPathSender(transport: transport).send(payload) { error in
            observe(.immediateFailed(kind: kind, recordingID: recordingID, error: error.localizedDescription))
        }
        observe(.queued(kind: kind, recordingID: recordingID, immediateAttempted: attempted))
    }
    public func sendProcessingResult(recordingID: String, succeeded: Bool, message: String,
                                     preview: String? = nil, transport: any WatchMessageTransport) {
        DualPathSender(transport: transport).send(delivery.result(
            recordingID: recordingID, succeeded: succeeded, message: message, preview: preview))
    }
}
