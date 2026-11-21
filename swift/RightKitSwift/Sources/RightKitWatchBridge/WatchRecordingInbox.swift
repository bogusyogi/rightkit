import Foundation

/// Durable receipt & result routing, independent of an app's WCSessionDelegate ownership.
/// Host delegate forwards callbacks synchronously so temporary WCSessionFile URLs stay valid.
public final class WatchRecordingInbox: Sendable {
    public let storage: WatchRecordingStorage
    public let delivery: WatchRecordingDelivery
    public init(storage: WatchRecordingStorage, delivery: WatchRecordingDelivery = .init()) {
        self.storage = storage; self.delivery = delivery
    }
    @discardableResult
    public func receive(fileURL: URL, metadata: [String: Any], transport: any WatchMessageTransport) throws -> String {
        do {
            let id = try storage.receive(fileURL: fileURL, metadata: metadata)
            // Storage's explicit receive policy owns identity. Always acknowledge its durable
            // returned ID, including an assigned UUID, never the unvalidated metadata value.
            DualPathSender(transport: transport).send([delivery.keys.acknowledgement: id])
            return id
        } catch {
            if let id = metadata["id"] as? String {
                DualPathSender(transport: transport).send([
                    delivery.keys.rollback: id, delivery.keys.rollbackReason: error.localizedDescription,
                ])
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
        for id in try storage.receivedRecordingIDs() {
            DualPathSender(transport: transport).send([delivery.keys.acknowledgement: id])
        }
    }
    public func sendProcessingResult(recordingID: String, succeeded: Bool, message: String,
                                     preview: String? = nil, transport: any WatchMessageTransport) {
        DualPathSender(transport: transport).send(delivery.result(
            recordingID: recordingID, succeeded: succeeded, message: message, preview: preview))
    }
}
