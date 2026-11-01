#if os(watchOS)
import AVFoundation
import Foundation
import WatchConnectivity
import WatchKit
import Combine
import WidgetKit

struct WatchRecording: Codable, Identifiable, Equatable, Sendable {
    let id: UUID
    let fileName: String
    let startedAt: Date
    let duration: TimeInterval
    let byteCount: Int64
}

struct WatchProcessingResult: Equatable {
    let recordingID: String
    let succeeded: Bool
    let message: String
    /// Optional local-only preview. This value is never passed to
    /// WatchTelemetry or any network uploader.
    let preview: String?
}

enum WatchTransferStatus: Equatable {
    case idle
    case queued(Int)
    case awaitingAcknowledgement(Int)
    case failed(String, pending: Int)
}

// `internal` (not `private`): constructed from WatchRecordingTransfer.swift
// (split out of this file per docs/audits/2026-09-23-perfile-b1.md).
struct WatchTransferSnapshot: Equatable {
    let status: WatchTransferStatus
    let pendingCount: Int
}

enum WatchRecordingError: LocalizedError {
    case microphonePermission
    case recorderUnavailable
    case startFailed
    case tooShort
    case recordingNotFound
    case activeRecording
    case storageFull

    var errorDescription: String? {
        switch self {
        case .microphonePermission:
            return "Allow microphone access in Watch Settings."
        case .recorderUnavailable:
            return "Recorder is unavailable."
        case .startFailed:
            return "Recording could not start."
        case .tooShort:
            return "Too short — hold & speak."
        case .recordingNotFound:
            return "Recording is no longer available."
        case .storageFull:
            return WatchPendingPolicy.capMessage
        case .activeRecording:
            return "Stop recording before deleting it."
        }
    }
}

@MainActor
final class WatchRecordingController: ObservableObject {
    static let shared = WatchRecordingController()

    @Published private(set) var isRecording = false
    @Published private(set) var meterLevels: [Double] = Array(repeating: 0, count: 17)
    @Published private(set) var startedAt: Date?
    @Published private(set) var recordings: [WatchRecording] = []
    @Published private(set) var errorMessage: String?
    @Published private(set) var transferStatus: WatchTransferStatus = .idle
    @Published private(set) var pendingTransferCount = 0
    @Published private(set) var unsentItems: [WatchPendingItem] = []
    @Published private(set) var processingResult: WatchProcessingResult?
    @Published private(set) var availableModes: [WatchMode] = []
    @Published private(set) var selectedModeID: String?

    private static let manifestKey = "watch.recordings.v1"
    // `recorder`/`meterTimer`/`meterSampleCount` are `internal` rather than
    // `private` only so the `#if DEBUG` capture-smoke QA extension (moved to
    // WatchRecordingControllerSmokeTests.swift) can read them; both stay
    // module-internal, so nothing outside this app target can see them.
    var recorder: AVAudioRecorder?
    private var currentID: UUID?
    private var isStarting = false
    var meterTimer: Timer?
    var meterSampleCount = 0
    private var sceneIsActive = false
    private var durationWarningSent = false
    private var durationCapStopScheduled = false
    private var durationWarningTask: Task<Void, Never>?
    private var durationCapTask: Task<Void, Never>?
    private var isLuminanceReduced = false
    private var audioSessionObservers: [NSObjectProtocol] = []
    private let modeStore = WatchModeStore()

    private init() {
        loadManifest()
        recoverPendingFinalization()
        if let saved = WatchRecordingRecoveryStore.loadProcessingResult() {
            if WatchRecordingRecoveryStore.latestRecordingID() == nil {
                if let id = UUID(uuidString: saved.recordingID) {
                    WatchRecordingRecoveryStore.rememberLatestRecordingID(id)
                }
            }
            if WatchRecordingRecoveryStore.latestRecordingID() != saved.recordingID {
                WatchRecordingRecoveryStore.clearProcessingResult(recordingID: saved.recordingID)
            } else {
                processingResult = WatchProcessingResult(
                    recordingID: saved.recordingID,
                    succeeded: saved.succeeded,
                    message: saved.message,
                    preview: saved.preview
                )
            }
        }
        // A process restart cannot retain an AVAudioRecorder. Clear stale
        // complication state before publishing fresh app state.
        WatchRecordingStateStore.setRecording(false)
        availableModes = modeStore.modes
        selectedModeID = modeStore.selectedModeID
        installAudioSessionObservers()
        WatchRecordingTransfer.shared.onStateChange = { [weak self] snapshot in
            Task { @MainActor [weak self] in
                self?.transferStatus = snapshot.status
                self?.pendingTransferCount = snapshot.pendingCount
                self?.unsentItems = WatchRecordingTransfer.shared.pendingItems()
                WatchRecordingStateStore.setPendingCount(snapshot.pendingCount)
                self?.pruneMissingRecordings()
                WidgetCenter.shared.reloadTimelines(ofKind: "app.heardright.ios.native.watch.record")
            }
        }
        WatchRecordingTransfer.shared.onModesChange = { [weak self] modes in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.availableModes = modes
                self.selectedModeID = self.modeStore.selectedModeID
            }
        }
        WatchRecordingTransfer.shared.onProcessingResult = { [weak self] result in
            Task { @MainActor [weak self] in
                guard let self,
                      self.currentID == nil,
                      !self.isStarting,
                      WatchRecordingRecoveryStore.latestRecordingID() == result.recordingID else { return }
                self.processingResult = result
            }
        }
        WatchRecordingTransfer.shared.activate()
        applyTransferSnapshot(WatchRecordingTransfer.shared.snapshot)
    }

    func toggle(source: String) async throws {
        if isRecording {
            try await stop(source: source)
        } else {
            try await start(source: source)
        }
    }

    func start(source: String) async throws {
        guard !isRecording, !isStarting else { return }
        guard WatchRecordingTransfer.shared.admitsNewRecording() else {
            errorMessage = WatchRecordingError.storageFull.localizedDescription
            throw WatchRecordingError.storageFull
        }
        isStarting = true
        defer { isStarting = false }
        WatchTelemetry.record("recording_start_requested", fields: ["source": source])
        errorMessage = nil
        processingResult = nil
        WatchRecordingRecoveryStore.clearProcessingResult()
        durationWarningSent = false
        durationCapStopScheduled = false
        // Fast path only. `sendMessage` wakes the companion iOS app when it is
        // reachable, which lets the phone prime Core ML WHILE the user is still
        // speaking instead of paying an 11.6s cold load after the audio lands.
        // Never load-bearing: it is best effort, errors are ignored, and
        // `transferFile` below remains the canonical durable delivery.
        WatchRecordingTransfer.shared.sendWakeHint(phase: "started")
        guard await requestMicrophonePermission() else {
            WatchTelemetry.record("recording_permission_denied", fields: ["source": source])
            errorMessage = WatchRecordingError.microphonePermission.localizedDescription
            throw WatchRecordingError.microphonePermission
        }

        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .default)
            try await activateAudioSession(session, source: source)

            let id = UUID()
            let start = Date()
            let fileURL = try recordingDirectory().appendingPathComponent(fileName(id: id, date: start))
            let settings: [String: Any] = [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 16_000,
                AVNumberOfChannelsKey: 1,
                AVEncoderBitRateKey: 32_000,
                AVEncoderAudioQualityKey: AVAudioQuality.high.rawValue,
            ]
            let nextRecorder = try AVAudioRecorder(url: fileURL, settings: settings)
            nextRecorder.isMeteringEnabled = true
            nextRecorder.prepareToRecord()
            guard nextRecorder.record(forDuration: WatchRecordingDelivery.maximumDuration) else {
                throw WatchRecordingError.startFailed
            }

            recorder = nextRecorder
            currentID = id
            startedAt = start
            isRecording = true
            WatchRecordingStateStore.setRecording(true, startedAt: start)
            WidgetCenter.shared.reloadTimelines(ofKind: "app.heardright.ios.native.watch.record")
            durationWarningTask = Task { @MainActor [weak self] in
                do {
                    try await Task.sleep(nanoseconds: UInt64(WatchRecordingDelivery.warningDuration * 1_000_000_000))
                    guard let self, self.isRecording, !self.durationWarningSent else { return }
                    self.durationWarningSent = true
                    WatchTelemetry.record("recording_duration_warning", fields: [
                        "duration_ms": Int(WatchRecordingDelivery.warningDuration * 1_000),
                        "warning_ms": Int(WatchRecordingDelivery.warningDuration * 1_000),
                    ])
                    WKInterfaceDevice.current().play(.notification)
                } catch { }
            }
            durationCapTask = Task { @MainActor [weak self] in
                do {
                    try await Task.sleep(nanoseconds: UInt64(WatchRecordingDelivery.maximumDuration * 1_000_000_000))
                    guard let self, self.isRecording, !self.durationCapStopScheduled else { return }
                    self.durationCapStopScheduled = true
                    do { try await self.stop(source: "watch_duration_cap") }
                    catch { self.reportStopError(error) }
                } catch { }
            }
            startMetering()
            WatchTelemetry.record("recording_started", fields: [
                "source": source,
                "recording_id": id.uuidString,
            ])
            WKInterfaceDevice.current().play(.start)
        } catch {
            await deactivateAudioSession()
            errorMessage = WatchRecordingError.startFailed.localizedDescription
            WatchTelemetry.record("recording_start_failed", fields: [
                "source": source,
                "error": error.localizedDescription,
                "error_type": String(reflecting: type(of: error)),
            ])
            throw error
        }
    }

    func stop(source: String) async throws {
        guard let recorder, let id = currentID, let startedAt else {
            WatchTelemetry.record("recording_stop_failed", fields: [
                "source": source,
                "reason": "recorder_unavailable",
            ])
            throw WatchRecordingError.recorderUnavailable
        }
        WatchTelemetry.record("recording_stop_requested", fields: [
            "source": source,
            "recording_id": id.uuidString,
        ])
        let duration = recorder.currentTime
        let fileURL = recorder.url
        // Capture identity before releasing recorder/session. If metadata read
        // fails or Watch is killed after stop, launch can rebuild manifest from
        // this durable marker while preserving audio.
        WatchRecordingRecoveryStore.savePendingFinalization(WatchFinalizationRecord(
            id: id,
            fileName: fileURL.lastPathComponent,
            startedAt: startedAt,
            duration: duration
        ))
        recorder.stop()
        await finishCapture()

        guard duration >= WatchRecordingDelivery.minimumDuration else {
            try? FileManager.default.removeItem(at: fileURL)
            WatchRecordingRecoveryStore.clearPendingFinalization(id: id)
            errorMessage = WatchRecordingError.tooShort.localizedDescription
            WatchTelemetry.record("recording_discarded_too_short", fields: [
                "source": source,
                "recording_id": id.uuidString,
                "duration_ms": Int(duration * 1_000),
            ])
            WKInterfaceDevice.current().play(.failure)
            throw WatchRecordingError.tooShort
        }

        let attributes: [FileAttributeKey: Any]
        do {
            attributes = try FileManager.default.attributesOfItem(atPath: fileURL.path)
        } catch {
            errorMessage = "Recording stopped, but its audio metadata is unavailable. It will be recovered on next launch."
            WatchTelemetry.record("recording_finalization_deferred", fields: [
                "source": source,
                "recording_id": id.uuidString,
                "error": error.localizedDescription,
            ])
            throw error
        }
        let byteCount = (attributes[.size] as? NSNumber)?.int64Value ?? 0
        let recording = WatchRecording(
            id: id,
            fileName: fileURL.lastPathComponent,
            startedAt: startedAt,
            duration: duration,
            byteCount: byteCount
        )
        recordings.insert(recording, at: 0)
        persistManifest()
        WatchRecordingRecoveryStore.rememberLatestRecordingID(id)
        WatchRecordingTransfer.shared.enqueue(recording: recording, fileURL: fileURL)
        WatchRecordingRecoveryStore.clearPendingFinalization(id: id)
        WatchRecordingTransfer.shared.sendWakeHint(phase: "stopped")
        WatchTelemetry.record("recording_stopped", fields: [
            "source": source,
            "recording_id": id.uuidString,
            "duration_ms": Int(duration * 1_000),
            "bytes": byteCount,
        ])

        WKInterfaceDevice.current().play(.stop)
    }

    /// Discard only the active take. Never insert a manifest entry or enqueue it.
    func cancel(source: String) async throws {
        guard let recorder, let id = currentID else { return }
        let fileURL = recorder.url
        recorder.stop()
        await finishCapture()
        do {
            if FileManager.default.fileExists(atPath: fileURL.path) {
                try FileManager.default.removeItem(at: fileURL)
            }
            errorMessage = nil
            WatchRecordingTransfer.shared.sendWakeHint(phase: "cancelled")
            WatchTelemetry.record("recording_cancelled", fields: [
                "source": source, "recording_id": id.uuidString,
            ])
            WKInterfaceDevice.current().play(.stop)
        } catch {
            errorMessage = "Recording stopped, but its temporary audio could not be removed."
            throw error
        }
    }

    private func startMetering() {
        meterTimer?.invalidate()
        meterSampleCount = 0
        meterLevels = Array(repeating: 0, count: 17)
        guard sceneIsActive, !isLuminanceReduced else { return }
        let timer = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                guard let self, self.sceneIsActive, !self.isLuminanceReduced,
                      self.isRecording, let recorder = self.recorder else { return }
                recorder.updateMeters()
                let power = recorder.averagePower(forChannel: 0)
                let level = WatchRecordingMeter.level(powerDB: power)
                self.meterLevels = WatchRecordingMeter.levels(level: level)
                self.meterSampleCount += 1
            }
        }
        meterTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    private func finishCapture() async {
        durationWarningTask?.cancel()
        durationWarningTask = nil
        durationCapTask?.cancel()
        durationCapTask = nil
        meterTimer?.invalidate()
        meterTimer = nil
        meterLevels = Array(repeating: 0, count: 17)
        self.recorder = nil
        currentID = nil
        self.startedAt = nil
        isRecording = false
        WatchRecordingStateStore.setRecording(false)
        WidgetCenter.shared.reloadTimelines(ofKind: "app.heardright.ios.native.watch.record")
        durationCapStopScheduled = false
        await deactivateAudioSession()
    }

    /// SwiftUI scene lifecycle is the battery gate for the visible meter.
    /// Recording remains active while the wrist is down; only 10 Hz rendering
    /// work is paused until the scene is visible again.
    func setSceneActive(_ active: Bool) {
        sceneIsActive = active
        if active, isRecording, !isLuminanceReduced {
            startMetering()
        } else if !active {
            meterTimer?.invalidate()
            meterTimer = nil
            meterLevels = Array(repeating: 0, count: 17)
        }
    }

    /// Always-On Display is a wrist-down state. Keep recording alive, but stop
    /// the 10 Hz meter timer until the full-luminance scene returns.
    func setLuminanceReduced(_ reduced: Bool) {
        isLuminanceReduced = reduced
        if reduced {
            meterTimer?.invalidate()
            meterTimer = nil
            meterLevels = Array(repeating: 0, count: 17)
        } else if sceneIsActive, isRecording {
            startMetering()
        }
    }

    func reportStopError(_ error: Error) {
        errorMessage = error.localizedDescription
    }

    func clearError() {
        errorMessage = nil
    }

    func selectMode(_ modeID: String?) {
        modeStore.selectedModeID = modeID
        selectedModeID = modeStore.selectedModeID
    }

    private func applyTransferSnapshot(_ snapshot: WatchTransferSnapshot) {
        transferStatus = snapshot.status
        pendingTransferCount = snapshot.pendingCount
        unsentItems = WatchRecordingTransfer.shared.pendingItems()
        WatchRecordingStateStore.setPendingCount(snapshot.pendingCount)
    }

    private func requestMicrophonePermission() async -> Bool {
        switch AVAudioApplication.shared.recordPermission {
        case .granted:
            return true
        case .denied:
            return false
        case .undetermined:
            return await AVAudioApplication.requestRecordPermission()
        @unknown default:
            return false
        }
    }

    private func activateAudioSession(_ session: AVAudioSession, source: String) async throws {
        WatchTelemetry.record("audio_session_activation_requested", fields: [
            "source": source,
        ])

        if #available(watchOS 27.0, *) {
            let activated: Bool
            do {
                activated = try await session.activate(options: [])
            } catch {
                WatchTelemetry.record("audio_session_activation_failed", fields: [
                    "source": source,
                    "api": "async_watchos27",
                    "error": error.localizedDescription,
                    "error_type": String(reflecting: type(of: error)),
                ])
                throw error
            }
            WatchTelemetry.record("audio_session_activation_completed", fields: [
                "source": source,
                "api": "async_watchos27",
                "activated": activated,
            ])
            guard activated else {
                WatchTelemetry.record("audio_session_activation_failed", fields: [
                    "source": source,
                    "api": "async_watchos27",
                    "reason": "returned_false",
                ])
                throw WatchRecordingError.startFailed
            }
        } else {
            do {
                try session.setActive(true)
            } catch {
                WatchTelemetry.record("audio_session_activation_failed", fields: [
                    "source": source,
                    "api": "set_active_legacy",
                    "error": error.localizedDescription,
                    "error_type": String(reflecting: type(of: error)),
                ])
                throw error
            }
            WatchTelemetry.record("audio_session_activation_completed", fields: [
                "source": source,
                "api": "set_active_legacy",
                "activated": true,
            ])
        }
    }

    private func deactivateAudioSession() async {
        let session = AVAudioSession.sharedInstance()
        if #available(watchOS 27.0, *) {
            do {
                let deactivated = try await session.deactivate(options: [.notifyOthersOnDeactivation])
                WatchTelemetry.record("audio_session_deactivation_completed", fields: [
                    "api": "async_watchos27",
                    "deactivated": deactivated,
                ])
                if !deactivated {
                    WatchTelemetry.record("audio_session_deactivation_failed", fields: [
                        "api": "async_watchos27",
                        "reason": "returned_false",
                    ])
                }
            } catch {
                WatchTelemetry.record("audio_session_deactivation_failed", fields: [
                    "api": "async_watchos27",
                    "error": error.localizedDescription,
                    "error_type": String(reflecting: type(of: error)),
                ])
            }
        } else {
            do {
                try session.setActive(false, options: .notifyOthersOnDeactivation)
                WatchTelemetry.record("audio_session_deactivation_completed", fields: [
                    "api": "set_active_legacy",
                    "deactivated": true,
                ])
            } catch {
                WatchTelemetry.record("audio_session_deactivation_failed", fields: [
                    "api": "set_active_legacy",
                    "error": error.localizedDescription,
                    "error_type": String(reflecting: type(of: error)),
                ])
            }
        }
    }

    private func installAudioSessionObservers() {
        let center = NotificationCenter.default
        let session = AVAudioSession.sharedInstance()
        if #available(watchOS 27.0, *) {
            audioSessionObservers.append(
                center.addObserver(
                    forName: AVAudioSession.didBecomeActiveNotification,
                    object: session,
                    queue: nil
                ) { _ in
                    WatchTelemetry.record("audio_session_lifecycle", fields: [
                        "event": "did_become_active",
                        "state": "active",
                        "source": "notification",
                        "outcome": "observed",
                    ])
                }
            )
            audioSessionObservers.append(
                center.addObserver(
                    forName: AVAudioSession.didBecomeInactiveNotification,
                    object: session,
                    queue: nil
                ) { [weak self] notification in
                    Task { @MainActor [weak self] in
                        await self?.handleAudioSessionDidBecomeInactive(notification)
                    }
                }
            )
            audioSessionObservers.append(
                center.addObserver(
                    forName: AVAudioSession.resumptionRecommendationNotification,
                    object: session,
                    queue: nil
                ) { notification in
                    var recommendation = "unknown"
                    if let context = notification.userInfo?[AVAudioSession.resumptionContextKey]
                        as? AVAudioSession.ResumptionContext {
                        recommendation = context.recommendation.rawValue == 1
                            ? "should_resume"
                            : "should_not_resume"
                    }
                    WatchTelemetry.record("audio_session_lifecycle", fields: [
                        "event": "resumption_recommendation",
                        "state": "unknown",
                        "source": "notification",
                        "outcome": recommendation,
                        "recommendation": recommendation,
                    ])
                }
            )
        } else {
            audioSessionObservers.append(
                center.addObserver(
                    forName: AVAudioSession.interruptionNotification,
                    object: session,
                    queue: nil
                ) { [weak self] notification in
                    Task { @MainActor [weak self] in
                        await self?.handleLegacyInterruption(notification)
                    }
                }
            )
        }
    }

    @available(watchOS 27.0, *)
    private func handleAudioSessionDidBecomeInactive(_ notification: Notification) async {
        var wasSystemDeactivation = false
        var fields: [String: Any] = [
            "event": "did_become_inactive",
            "state": "inactive",
            "source": "notification",
            "outcome": "observed",
            "deactivation_context_present": notification.userInfo?[AVAudioSession.deactivationContextKey] != nil,
        ]
        if let context = notification.userInfo?[AVAudioSession.deactivationContextKey]
            as? AVAudioSession.DeactivationContext {
            fields["deactivation_source_code"] = context.source.rawValue
            fields["interruption_context_present"] = context.interruptionContext != nil
            wasSystemDeactivation = context.source == .system
        }
        WatchTelemetry.record("audio_session_lifecycle", fields: fields)

        guard wasSystemDeactivation, isRecording else { return }
        do {
            try await stop(source: "audio_session_inactive")
        } catch {
            try? await cancel(source: "audio_session_inactive")
        }
    }

    private func handleLegacyInterruption(_ notification: Notification) async {
        guard isRecording else { return }
        let typeValue = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
        guard typeValue == AVAudioSession.InterruptionType.began.rawValue else { return }
        WatchTelemetry.record("audio_session_lifecycle", fields: [
            "event": "interruption_began",
            "state": "inactive",
            "source": "notification",
            "outcome": "observed",
        ])
        do {
            try await stop(source: "audio_session_interruption")
        } catch {
            try? await cancel(source: "audio_session_interruption")
        }
    }

    func refreshTransferState() {
        WatchRecordingTransfer.shared.refresh()
    }

    // `internal` (not `private`) only so the moved `#if DEBUG` capture-smoke
    // QA extension in WatchRecordingControllerSmokeTests.swift can call it.
    func recordingDirectory() throws -> URL {
        let root = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Recordings", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }

    private func fileName(id: UUID, date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        return "HeardRight-\(formatter.string(from: date).replacingOccurrences(of: ":", with: "-"))-\(id.uuidString.prefix(8)).m4a"
    }

    private func loadManifest() {
        guard let data = UserDefaults.standard.data(forKey: Self.manifestKey),
              let decoded = try? JSONDecoder().decode([WatchRecording].self, from: data)
        else { return }
        recordings = decoded.filter { recording in
            guard let directory = try? recordingDirectory() else { return false }
            return FileManager.default.fileExists(atPath: directory.appendingPathComponent(recording.fileName).path)
        }
    }

    /// Rebuild a row when stop completed capture but metadata/manifest work was
    /// interrupted. The marker is cleared only after durable manifest + queue
    /// insertion, so an unreadable metadata path never deletes audio.
    private func recoverPendingFinalization() {
        guard let directory = try? recordingDirectory() else { return }
        let pendingFinalizations = WatchRecordingRecoveryStore.loadPendingFinalizations()
        if let newest = pendingFinalizations.first(where: {
            FileManager.default.fileExists(atPath: directory.appendingPathComponent($0.fileName).path)
        }) {
            WatchRecordingRecoveryStore.rememberLatestRecordingID(newest.id)
        }
        for pending in pendingFinalizations {
            let fileURL = directory.appendingPathComponent(pending.fileName)
            guard FileManager.default.fileExists(atPath: fileURL.path) else {
                WatchTelemetry.record("recording_orphan_pending", fields: [
                    "recording_id": pending.id.uuidString,
                    "reason": "audio_file_missing",
                ])
                continue
            }
            let recording: WatchRecording
            if let existing = recordings.first(where: { $0.id == pending.id }) {
                recording = existing
            } else {
                guard let attributes = try? FileManager.default.attributesOfItem(atPath: fileURL.path),
                      let byteCount = (attributes[.size] as? NSNumber)?.int64Value else {
                    WatchTelemetry.record("recording_orphan_pending", fields: [
                        "recording_id": pending.id.uuidString,
                        "reason": "metadata_unavailable",
                    ])
                    continue
                }
                recording = WatchRecording(
                    id: pending.id,
                    fileName: pending.fileName,
                    startedAt: pending.startedAt,
                    duration: pending.duration,
                    byteCount: byteCount
                )
                recordings.insert(recording, at: 0)
                persistManifest()
            }
            // A crash can occur after manifest persistence but before enqueue;
            // enqueue is idempotent and must precede marker removal.
            WatchRecordingTransfer.shared.enqueue(recording: recording, fileURL: fileURL)
            WatchRecordingRecoveryStore.clearPendingFinalization(id: pending.id)
            WatchTelemetry.record("recording_orphan_recovered", fields: [
                "recording_id": pending.id.uuidString,
                "bytes": recording.byteCount,
            ])
        }
    }

    private func persistManifest() {
        let retained = Array(recordings.prefix(250))
        guard let data = try? JSONEncoder().encode(retained) else { return }
        UserDefaults.standard.set(data, forKey: Self.manifestKey)
    }

    /// Transfer acknowledgements remove local audio. Keep the published list
    /// aligned with that file ownership while retaining queued retry items.
    private func pruneMissingRecordings() {
        guard let directory = try? recordingDirectory() else { return }
        let retained = recordings.filter { recording in
            let fileURL = directory.appendingPathComponent(recording.fileName)
            return FileManager.default.fileExists(atPath: fileURL.path)
                || WatchRecordingTransfer.shared.isPending(recordingID: recording.id)
        }
        guard retained.count != recordings.count else { return }
        recordings = retained
        persistManifest()
    }
}
#endif
