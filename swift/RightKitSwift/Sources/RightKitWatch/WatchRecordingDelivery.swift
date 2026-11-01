import Foundation
import RightKitWatchBridge

/// Stable recording names survive sandbox relocation; absolute URLs do not.
enum WatchRecordingDelivery {
    /// One capture contract shared by Watch recorder & phone receiver.
    /// Keeping this here prevents a long take from reaching a decoder that
    /// was sized for ordinary dictation.
    static let maximumDuration: TimeInterval = 10 * 60
    static let warningDuration: TimeInterval = 9 * 60 + 30
    static let minimumDuration: TimeInterval = 1.5
    static let transcriptPreviewCharacterLimit = 80

    /// Keep already-shipped the app wire names while using RightKit's
    /// transport helpers. The package defaults are neutral for new apps.
    static var bridgeKeys: WatchBridgeKeys {
        var keys = WatchBridgeKeys()
        keys.acknowledgement = "watchRecordingAck"
        keys.rollback = "watchRecordingRollback"
        keys.rollbackReason = "watchRecordingRollbackReason"
        keys.receiptQuery = "watchRecordingReceiptQuery"
        keys.receipts = "watchRecordingReceipts"
        keys.wakeHint = "watchRecordingPhase"
        return keys
    }

    static func restoredURL(fileName: String, documents: URL) -> URL {
        RelocatableFile.url(
            fileName: URL(fileURLWithPath: fileName).lastPathComponent,
            in: documents.appendingPathComponent("Recordings", isDirectory: true)
        )
    }

    static func confirmedIDs(requested: [String], received: Set<String>) -> [String] {
        WatchReceipts.confirmed(requested: requested, received: received)
    }
}
