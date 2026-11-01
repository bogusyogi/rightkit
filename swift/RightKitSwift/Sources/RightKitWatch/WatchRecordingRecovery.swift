import Foundation

/// Small Foundation-only stores shared by capture/transfer code & focused
/// production-logic tests. UserDefaults keeps state across Watch process death.
struct WatchFinalizationRecord: Codable, Equatable {
    let id: UUID
    let fileName: String
    let startedAt: Date
    let duration: TimeInterval
}

struct WatchProcessingResultRecord: Codable, Equatable {
    let recordingID: String
    let succeeded: Bool
    let message: String
    let preview: String?
}

enum WatchRecordingRecoveryStore {
    static let pendingFinalizationKey = "watch.pending-finalization.v1"
    static let processingResultKey = "watch.processing-result.v1"
    static let latestRecordingIDKey = "watch.latest-recording-id.v1"

    static func savePendingFinalization(_ record: WatchFinalizationRecord) {
        var records = loadPendingFinalizations()
        records.removeAll { $0.id == record.id }
        records.insert(record, at: 0)
        guard let data = try? JSONEncoder().encode(records) else { return }
        UserDefaults.standard.set(data, forKey: pendingFinalizationKey)
    }

    static func loadPendingFinalizations() -> [WatchFinalizationRecord] {
        guard let data = UserDefaults.standard.data(forKey: pendingFinalizationKey) else { return [] }
        if let records = try? JSONDecoder().decode([WatchFinalizationRecord].self, from: data) {
            return records
        }
        // Migrate the original single-record encoding without dropping it.
        guard let record = try? JSONDecoder().decode(WatchFinalizationRecord.self, from: data) else { return [] }
        return [record]
    }

    static func clearPendingFinalization(id: UUID? = nil) {
        guard let id else {
            UserDefaults.standard.removeObject(forKey: pendingFinalizationKey)
            return
        }
        var records = loadPendingFinalizations()
        records.removeAll { $0.id == id }
        persistPendingFinalizations(records)
    }

    static func rememberLatestRecordingID(_ id: UUID) {
        UserDefaults.standard.set(id.uuidString, forKey: latestRecordingIDKey)
    }

    static func forgetLatestRecordingID(_ id: String) {
        guard latestRecordingID() == id else { return }
        UserDefaults.standard.removeObject(forKey: latestRecordingIDKey)
    }

    static func latestRecordingID() -> String? {
        UserDefaults.standard.string(forKey: latestRecordingIDKey)
    }

    /// Accept first result for legacy installs with no marker, then reject
    /// out-of-order results once latest recording identity is known.
    static func shouldAcceptProcessingResult(recordingID: String) -> Bool {
        if let latest = latestRecordingID() { return latest == recordingID }
        UserDefaults.standard.set(recordingID, forKey: latestRecordingIDKey)
        return true
    }

    static func saveProcessingResult(_ result: WatchProcessingResultRecord) {
        guard let data = try? JSONEncoder().encode(result) else { return }
        UserDefaults.standard.set(data, forKey: processingResultKey)
    }

    static func loadProcessingResult() -> WatchProcessingResultRecord? {
        guard let data = UserDefaults.standard.data(forKey: processingResultKey) else { return nil }
        return try? JSONDecoder().decode(WatchProcessingResultRecord.self, from: data)
    }

    static func clearProcessingResult(recordingID: String? = nil) {
        guard let recordingID else {
            UserDefaults.standard.removeObject(forKey: processingResultKey)
            return
        }
        guard let current = loadProcessingResult(), current.recordingID == recordingID else { return }
        UserDefaults.standard.removeObject(forKey: processingResultKey)
    }

    private static func persistPendingFinalizations(_ records: [WatchFinalizationRecord]) {
        guard !records.isEmpty else {
            UserDefaults.standard.removeObject(forKey: pendingFinalizationKey)
            return
        }
        // Never discard recovery metadata while its captured audio still
        // needs finalization. Each successful enqueue removes its own entry.
        guard let data = try? JSONEncoder().encode(records) else { return }
        UserDefaults.standard.set(data, forKey: pendingFinalizationKey)
    }
}
