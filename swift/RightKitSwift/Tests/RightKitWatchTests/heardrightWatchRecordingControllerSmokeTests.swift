import Foundation
import AVFoundation
import WatchConnectivity

// Split out of WatchRecordingController.swift (pure move, no logic changes).
// `runCaptureSmoke()` is a `#if DEBUG` QA smoke test embedded in production
// WatchRecordingController; per docs/agent-rules.md ("Move inline tests out
// of large app source files") and the finding in
// docs/audits/2026-09-23-perfile-b1.md, it belongs in a separate QA-only
// file. Runs only in an explicitly launched, unpaired QA simulator.

#if DEBUG
extension WatchRecordingController {
    /// Multi-process native simulator flow. Audio/remote callback fixtures are
    /// synthetic; production startup recovery, queue, ack & result paths run.
    func runRecoveryE2E(stage: String) async {
        #if targetEnvironment(simulator)
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let output = documents.appendingPathComponent("recovery-e2e-\(stage).json")
        var result: [String: Any] = ["stage": stage, "pid": ProcessInfo.processInfo.processIdentifier]
        let fixtureKey = "qa.watch-recovery.fixture"
        func require(_ condition: Bool, _ message: String) throws {
            if !condition { throw NSError(domain: "WatchRecoveryE2E", code: 1,
                userInfo: [NSLocalizedDescriptionKey: message]) }
        }
        do {
            let directory = try recordingDirectory()
            if stage == "prepare" {
                try require(recordings.isEmpty && WatchRecordingTransfer.shared.snapshot.pendingCount == 0,
                            "Requires disposable empty Watch simulator")
                let oldID = UUID(), currentID = UUID()
                let ids = [oldID, currentID]
                for id in ids {
                    let name = "RecoveryE2E-\(id).m4a"
                    let url = directory.appendingPathComponent(name)
                    let format = AVAudioFormat(standardFormatWithSampleRate: 16_000, channels: 1)!
                    let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 32_000)!
                    buffer.frameLength = 32_000
                    for index in 0..<Int(buffer.frameLength) { buffer.floatChannelData![0][index] = 0 }
                    let file = try AVAudioFile(forWriting: url, settings: [
                        AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: 16_000,
                        AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 32_000
                    ])
                    try file.write(from: buffer)
                    WatchRecordingRecoveryStore.savePendingFinalization(.init(
                        id: id, fileName: name, startedAt: Date(), duration: 2))
                }
                UserDefaults.standard.set(ids.map(\.uuidString), forKey: fixtureKey)
                WatchRecordingStateStore.setRecording(true)
                result["pendingFinalizations"] = WatchRecordingRecoveryStore.loadPendingFinalizations().count
                try require(WatchRecordingRecoveryStore.loadPendingFinalizations().count == 2, "Crash markers persisted")
            } else {
                let ids = (UserDefaults.standard.stringArray(forKey: fixtureKey) ?? []).compactMap(UUID.init(uuidString:))
                try require(ids.count == 2, "Preparation fixture survives process restart")
                let oldID = ids[0], currentID = ids[1]
                let oldURL = directory.appendingPathComponent("RecoveryE2E-\(oldID).m4a")
                let currentURL = directory.appendingPathComponent("RecoveryE2E-\(currentID).m4a")
                let transfer = WatchRecordingTransfer.shared
                try require(!isRecording && !WatchRecordingStateStore.snapshot.isRecording,
                            "Cold launch clears stale capture state")
                try require(WatchRecordingRecoveryStore.loadPendingFinalizations().isEmpty,
                            "Startup recovery durably completes markers")
                if stage == "recovered" {
                    try require(Set(recordings.map(\.id)) == Set(ids), "Both History rows recovered")
                    try require(transfer.snapshot.pendingCount == 2, "Both offline recordings queued after restart")
                    try require(FileManager.default.fileExists(atPath: oldURL.path) &&
                                FileManager.default.fileExists(atPath: currentURL.path), "Unacknowledged audio retained")
                    try require(WatchRecordingRecoveryStore.latestRecordingID() == currentID.uuidString,
                                "Newest recovered session owns processing result")
                } else if stage == "ack" {
                    try require(transfer.snapshot.pendingCount == 2, "Offline queue survives second restart")
                    let keys = WatchRecordingDelivery.bridgeKeys
                    transfer.session(WCSession.default, didReceiveMessage: [keys.acknowledgement: UUID().uuidString])
                    try require(transfer.snapshot.pendingCount == 2, "Unknown ack cannot remove pending audio")
                    func processing(_ id: UUID, message: String) -> [String: Any] {
                        [WatchTelemetryKeys.processingResultKey: ["recordingID": id.uuidString,
                            "succeeded": true, "message": message, "preview": "fixture result"]]
                    }
                    transfer.session(WCSession.default, didReceiveMessage: processing(oldID, message: "stale"))
                    try require(WatchRecordingRecoveryStore.loadProcessingResult() == nil, "Old result rejected")
                    transfer.session(WCSession.default, didReceiveMessage: [keys.acknowledgement: oldID.uuidString])
                    transfer.session(WCSession.default, didReceiveMessage: [keys.acknowledgement: oldID.uuidString])
                    try require(transfer.snapshot.pendingCount == 1 && !FileManager.default.fileExists(atPath: oldURL.path),
                                "Duplicate old ack removes only its owned audio")
                    transfer.session(WCSession.default, didReceiveMessage: [keys.rollback: currentID.uuidString,
                        keys.rollbackReason: "fixture phone persistence failure"])
                    try require(transfer.snapshot.pendingCount == 1 && FileManager.default.fileExists(atPath: currentURL.path),
                                "Phone rollback retains retryable current audio")
                    let retainedAudio = try Data(contentsOf: currentURL)
                    transfer.session(WCSession.default, didReceiveMessage: [keys.acknowledgement: currentID.uuidString])
                    transfer.session(WCSession.default, didReceiveMessage: processing(currentID, message: "current"))
                    transfer.session(WCSession.default, didReceiveMessage: processing(currentID, message: "current"))
                    transfer.session(WCSession.default, didReceiveMessage: processing(oldID, message: "late stale"))
                    let deadline = Date().addingTimeInterval(5)
                    while processingResult?.recordingID != currentID.uuidString && Date() < deadline {
                        try await Task.sleep(for: .milliseconds(20))
                    }
                    try require(transfer.snapshot.pendingCount == 0 && !FileManager.default.fileExists(atPath: currentURL.path),
                                "Durable current ack closes queue & removes audio")
                    try require(processingResult?.message == "current" &&
                        WatchRecordingRecoveryStore.loadProcessingResult()?.recordingID == currentID.uuidString,
                        "Ack-before-result remains visible & persisted; duplicates/stale result cannot replace it")
                    // Crash between persisted ack receipt & file cleanup: next
                    // startup must sweep this acknowledged orphan only.
                    try retainedAudio.write(to: currentURL)
                } else if stage == "terminal" {
                    try require(transfer.snapshot.pendingCount == 0 && recordings.isEmpty,
                                "Final relaunch retains closed queue & prunes removed History")
                    try require(!FileManager.default.fileExists(atPath: oldURL.path) &&
                                !FileManager.default.fileExists(atPath: currentURL.path), "Acknowledged orphan swept")
                    try require(processingResult?.recordingID == currentID.uuidString && processingResult?.message == "current",
                                "Current terminal result survives cold relaunch")
                } else { throw NSError(domain: "UnknownRecoveryStage", code: 1) }
                result["pendingTransfers"] = transfer.snapshot.pendingCount
                result["historyCount"] = recordings.count
                result["currentRecordingID"] = currentID.uuidString
            }
            UserDefaults.standard.synchronize()
            result["status"] = "PASS"
        } catch {
            result["status"] = "FAIL"
            result["error"] = error.localizedDescription
        }
        if let data = try? JSONSerialization.data(withJSONObject: result, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: output, options: .atomic)
        }
        #endif
    }

    /// Runs only in an explicitly launched, unpaired QA simulator.
    func runCaptureSmoke() async {
        let output = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("capture-smoke.json")
        var result: [String: String] = [:]
        do {
            let originalIDs = recordings.map(\.id)
            let originalPending = pendingTransferCount
            try await start(source: "watch_capture_smoke")
            try await Task.sleep(for: .milliseconds(450))
            guard let url = recorder?.url, meterLevels.count == 17,
                  meterSampleCount >= 2,
                  meterLevels.allSatisfy({ $0.isFinite && (0...1).contains($0) }) else {
                throw WatchRecordingError.startFailed
            }
            try await cancel(source: "watch_capture_smoke")
            guard !isRecording, recorder == nil, startedAt == nil, meterTimer == nil,
                  meterLevels.allSatisfy({ $0 == 0 }), !FileManager.default.fileExists(atPath: url.path),
                  recordings.map(\.id) == originalIDs, pendingTransferCount == originalPending else {
                throw WatchRecordingError.startFailed
            }
            result["cancel"] = "PASS: audio removed, capture released, history & queue unchanged"
            try await start(source: "watch_capture_smoke")
            // Exercise accepted audio, beyond production minimum duration.
            try await Task.sleep(for: .seconds(WatchRecordingDelivery.minimumDuration + 0.5))
            try await stop(source: "watch_capture_smoke")
            guard !isRecording, recorder == nil, meterTimer == nil,
                  recordings.count == originalIDs.count + 1,
                  let saved = recordings.first,
                  FileManager.default.fileExists(atPath: try recordingDirectory().appendingPathComponent(saved.fileName).path)
            else { throw WatchRecordingError.startFailed }
            result["stop"] = "PASS: recording saved & enqueue path completed"
            result["status"] = "PASS"
        } catch {
            try? await cancel(source: "watch_capture_smoke_cleanup")
            result["status"] = "FAIL"
            result["error"] = error.localizedDescription
        }
        if let data = try? JSONSerialization.data(withJSONObject: result, options: .prettyPrinted) {
            try? data.write(to: output)
        }
    }
}
#endif
