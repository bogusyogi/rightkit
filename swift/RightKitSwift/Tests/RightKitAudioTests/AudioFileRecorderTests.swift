import AVFoundation
import Foundation
import Testing

@testable import RightKitAudio

struct AudioFileRecorderSettingsTests {
    @Test func defaultIsSixteenKilohertzMonoSixteenBitLinearPCM() {
        let settings = AudioFileRecorderSettings().avSettings
        #expect(settings[AVFormatIDKey] as? Int == Int(kAudioFormatLinearPCM))
        #expect(settings[AVSampleRateKey] as? Double == 16_000)
        #expect(settings[AVNumberOfChannelsKey] as? Int == 1)
        #expect(settings[AVLinearPCMBitDepthKey] as? Int == 16)
        #expect(settings[AVLinearPCMIsFloatKey] as? Bool == false)
        #expect(settings[AVLinearPCMIsBigEndianKey] as? Bool == false)
    }

    @Test func aacCarriesTheBitRate() {
        let settings = AudioFileRecorderSettings(encoding: .aac(bitRate: 32_000)).avSettings
        #expect(settings[AVFormatIDKey] as? Int == Int(kAudioFormatMPEG4AAC))
        #expect(settings[AVEncoderBitRateKey] as? Int == 32_000)
        #expect(settings[AVSampleRateKey] as? Double == 16_000)
    }

    @Test func meterLevelClampsAtTheFloorAndCeiling() {
        #expect(AudioFileRecorder.level(powerDB: -160) == 0)
        #expect(AudioFileRecorder.level(powerDB: -60) == 0)
        #expect(AudioFileRecorder.level(powerDB: -30) == 0.5)
        #expect(AudioFileRecorder.level(powerDB: 0) == 1)
        #expect(AudioFileRecorder.level(powerDB: 12) == 1)
    }
}

/// Drives the recorder's state machine without a microphone.
struct AudioFileRecorderStateTests {
    final class FakeBackend: RecorderBackend, @unchecked Sendable {
        let url: URL
        private let lock = NSLock()
        private var _onFinish: (@Sendable (Bool) -> Void)?
        private(set) var calls: [String] = []
        var failRecord = false
        var failResume = false
        var power: Float = -30

        init(url: URL) { self.url = url }

        var currentTime: TimeInterval { 2.5 }
        var onFinish: (@Sendable (Bool) -> Void)? {
            get { lock.withLock { _onFinish } }
            set { lock.withLock { _onFinish = newValue } }
        }
        func prepare() -> Bool { lock.withLock { calls.append("prepare") }; return true }
        func record(forDuration: TimeInterval?) -> Bool {
            lock.withLock { calls.append("record") }
            guard !failRecord else { return false }
            try? Data(repeating: 1, count: 64).write(to: url)
            return true
        }
        func pause() { lock.withLock { calls.append("pause") } }
        func resume() -> Bool { lock.withLock { calls.append("resume") }; return !failResume }
        func stop() {
            lock.withLock { calls.append("stop") }
            onFinish?(true)
        }
        func averagePowerDB() -> Float { power }
        func simulateFinish(success: Bool) { onFinish?(success) }
        var callLog: [String] { lock.withLock { calls } }
    }

    final class Harness: @unchecked Sendable {
        let lock = NSLock()
        var backend: FakeBackend?
        var sessionHandler: (@Sendable (AudioSessionEvent) -> Void)?
        var activations = 0
        var releases = 0
        var granted = true
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("rk-audio-\(UUID().uuidString)")

        func recorder(policy: AudioFileRecorder.InterruptionPolicy = .stopAndKeepFile, session: AudioSessionConfiguration? = .recordOnly,
                      failRecord: Bool = false, failResume: Bool = false) -> AudioFileRecorder {
            try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            return AudioFileRecorder(
                configuration: .init(session: session, interruptionPolicy: policy, finalizeTimeout: 0.5),
                makeBackend: { [self] url, _ in
                    let fake = FakeBackend(url: url)
                    fake.failRecord = failRecord
                    fake.failResume = failResume
                    lock.withLock { backend = fake }
                    return fake
                },
                requestPermission: { [self] in lock.withLock { granted } },
                activateSession: { [self] _ in lock.withLock { activations += 1 } },
                releaseSession: { [self] _ in lock.withLock { releases += 1 } },
                makeMonitor: { [self] handler in
                    lock.withLock { sessionHandler = handler }
                    return AudioSessionMonitor(handler: handler)
                },
            )
        }

        var url: URL { directory.appendingPathComponent("take.caf") }
        func send(_ event: AudioSessionEvent) { lock.withLock { sessionHandler }?(event) }
        func cleanup() { try? FileManager.default.removeItem(at: directory) }
    }

    private func nextEvent(_ recorder: AudioFileRecorder) async -> AudioFileRecorderEvent? {
        var iterator = recorder.events.makeAsyncIterator()
        return await iterator.next()
    }

    @Test func startThenStopReturnsTheFinalizedFileAndReleasesTheSession() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder()
        try await recorder.start(to: h.url)
        #expect(recorder.state == .recording)
        #expect(recorder.isRecording)
        #expect(recorder.meterLevel == 0.5)
        #expect(recorder.currentTime == 2.5)

        let recording = try #require(await recorder.stop())
        #expect(recording.url == h.url)
        #expect(recording.byteCount == 64)
        #expect(recording.duration == 2.5)
        #expect(recorder.state == .idle)
        #expect(recorder.meterLevel == 0)
        #expect(h.activations == 1 && h.releases == 1)
        #expect(await recorder.stop() == nil, "stopping an idle recorder is a no-op")
    }

    @Test func deniedPermissionFailsBeforeTouchingTheSession() async {
        let h = Harness()
        defer { h.cleanup() }
        h.granted = false
        let recorder = h.recorder()
        await #expect(throws: AudioFileRecorderError.permissionDenied) { try await recorder.start(to: h.url) }
        #expect(recorder.state == .idle)
        #expect(h.activations == 0)
    }

    @Test func aFailedRecordStartLeavesTheRecorderIdleAndReleasesTheSession() async {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder(failRecord: true)
        await #expect(throws: AudioFileRecorderError.startFailed) { try await recorder.start(to: h.url) }
        #expect(recorder.state == .idle)
        #expect(h.activations == 1 && h.releases == 1)
    }

    @Test func aSecondStartWhileRecordingIsRefused() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder()
        try await recorder.start(to: h.url)
        await #expect(throws: AudioFileRecorderError.alreadyRecording) { try await recorder.start(to: h.url) }
        #expect(recorder.state == .recording, "the refused start must not disturb the live recording")
        _ = await recorder.stop()
    }

    @Test func cancelDeletesTheFile() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder()
        try await recorder.start(to: h.url)
        #expect(FileManager.default.fileExists(atPath: h.url.path))
        await recorder.cancel()
        #expect(!FileManager.default.fileExists(atPath: h.url.path))
        #expect(recorder.state == .idle)
    }

    @Test func anInterruptionEndsTheRecordingAndKeepsTheFileUnderTheStopPolicy() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder(policy: .stopAndKeepFile)
        try await recorder.start(to: h.url)
        h.send(.interruptionBegan)
        let event = await nextEvent(recorder)
        guard case let .endedBySystem(reason, recording)? = event else {
            Issue.record("expected endedBySystem, got \(String(describing: event))")
            return
        }
        #expect(reason == .interruption)
        #expect(recording?.byteCount == 64)
        #expect(recorder.state == .idle)
        #expect(h.releases == 1)
    }

    @Test func aLostInputRouteEndsTheRecordingButANewDeviceDoesNot() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder()
        try await recorder.start(to: h.url)
        h.send(.routeChanged(.newDeviceAvailable))
        h.send(.routeChanged(.categoryChange))
        try await Task.sleep(nanoseconds: 50_000_000)
        #expect(recorder.state == .recording)
        h.send(.routeChanged(.oldDeviceUnavailable))
        let event = await nextEvent(recorder)
        guard case let .endedBySystem(reason, _)? = event else {
            Issue.record("expected endedBySystem, got \(String(describing: event))")
            return
        }
        #expect(reason == .inputRouteLost)
    }

    @Test func aMediaServicesResetEndsTheRecording() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder()
        try await recorder.start(to: h.url)
        h.send(.mediaServicesReset)
        let event = await nextEvent(recorder)
        guard case let .endedBySystem(reason, _)? = event else {
            Issue.record("expected endedBySystem, got \(String(describing: event))")
            return
        }
        #expect(reason == .mediaServicesReset)
        #expect(recorder.state == .idle)
    }

    @Test func pauseAndResumePolicyPausesThenContinuesTheSameFile() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder(policy: .pauseAndResume)
        try await recorder.start(to: h.url)
        var iterator = recorder.events.makeAsyncIterator()

        h.send(.interruptionBegan)
        let step1 = await iterator.next()
        #expect(step1 == .paused)
        #expect(recorder.state == .paused)

        h.send(.interruptionEnded(shouldResume: true))
        let step2 = await iterator.next()
        #expect(step2 == .resumed)
        #expect(recorder.state == .recording)
        #expect(h.activations == 2, "the session is reactivated before resuming")
        #expect(h.backend?.callLog.contains("resume") == true)
        _ = await recorder.stop()
    }

    @Test func anInterruptionThatDoesNotRecommendResumingEndsThePausedRecording() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder(policy: .pauseAndResume)
        try await recorder.start(to: h.url)
        var iterator = recorder.events.makeAsyncIterator()
        h.send(.interruptionBegan)
        let step3 = await iterator.next()
        #expect(step3 == .paused)
        h.send(.interruptionEnded(shouldResume: false))
        guard case let .endedBySystem(reason, recording)? = await iterator.next() else {
            Issue.record("expected endedBySystem")
            return
        }
        #expect(reason == .interruption)
        #expect(recording != nil)
        #expect(recorder.state == .idle)
    }

    @Test func aFailedResumeEndsTheRecordingInsteadOfClaimingToRecord() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder(policy: .pauseAndResume, failResume: true)
        try await recorder.start(to: h.url)
        var iterator = recorder.events.makeAsyncIterator()
        h.send(.interruptionBegan)
        let step4 = await iterator.next()
        #expect(step4 == .paused)
        h.send(.interruptionEnded(shouldResume: true))
        guard case .endedBySystem? = await iterator.next() else {
            Issue.record("expected endedBySystem")
            return
        }
        #expect(recorder.state == .idle)
    }

    @Test func theDurationCapEndsTheRecordingAndReportsTheFile() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder()
        try await recorder.start(to: h.url)
        h.backend?.simulateFinish(success: true)  // AVAudioRecorder stopping itself at the cap
        let event = await nextEvent(recorder)
        guard case let .endedBySystem(reason, recording)? = event else {
            Issue.record("expected endedBySystem, got \(String(describing: event))")
            return
        }
        #expect(reason == .maximumDuration)
        #expect(recording?.byteCount == 64)
        #expect(recorder.state == .idle)
    }

    @Test func anEncoderErrorIsReportedAsSuch() async throws {
        let h = Harness()
        defer { h.cleanup() }
        let recorder = h.recorder()
        try await recorder.start(to: h.url)
        h.backend?.simulateFinish(success: false)
        guard case let .endedBySystem(reason, _)? = await nextEvent(recorder) else {
            Issue.record("expected endedBySystem")
            return
        }
        #expect(reason == .encoderError)
    }
}
