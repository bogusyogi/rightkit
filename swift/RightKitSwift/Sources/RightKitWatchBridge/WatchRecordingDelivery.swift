import Foundation

public struct WatchRecordingDelivery: Sendable {
    public var maximumDuration: TimeInterval = 600
    public var warningDuration: TimeInterval = 570
    public var minimumDuration: TimeInterval = 1.5
    public var transcriptPreviewCharacterLimit = 80
    public var keys: WatchBridgeKeys
    public var processingResultKey: String
    public init(keys: WatchBridgeKeys = .init(), processingResultKey: String = "watchTransferResult") {
        self.keys = keys; self.processingResultKey = processingResultKey
    }
    public func result(recordingID: String, succeeded: Bool, message: String, preview: String? = nil) -> [String: Any] {
        var result: [String: Any] = ["recordingID": recordingID, "succeeded": succeeded, "message": message]
        if let preview, !preview.isEmpty {
            result["preview"] = String(preview.prefix(max(0, transcriptPreviewCharacterLimit)))
        }
        return [processingResultKey: result]
    }
    public static func restoredURL(fileName: String, documents: URL, directoryName: String = "Recordings") -> URL {
        RelocatableFile.url(fileName: fileName, in: documents.appendingPathComponent(directoryName, isDirectory: true))
    }
}
