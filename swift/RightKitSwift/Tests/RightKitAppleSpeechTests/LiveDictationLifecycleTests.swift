import Foundation
import RightKitAudio
import Speech
import Testing

@testable import RightKitAppleSpeech
@testable import RightKitSpeech

/// `Event.stopped`, per-segment finals, audio-session interruption and the network opt-in (#595, #597, #599).
struct LiveDictationLifecycleTests {
    private final class Events: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: [LiveDictationSession.Event] = []
        func add(_ event: LiveDictationSession.Event) { lock.withLock { stored.append(event) } }
        var all: [LiveDictationSession.Event] { lock.withLock { stored } }
        var stops: Int { all.filter { $0 == .stopped }.count }
        var failures: [String] { all.compactMap { if case let .failed(text) = $0 { text } else { nil } } }
        func contains(_ event: LiveDictationSession.Event) -> Bool { all.contains(event) }
    }

    private final class FakeCapture: LegacyCaptureHandle, @unchecked Sendable {
        private let lock = NSLock()
        private var live = true
        var isInstalled: Bool { lock.withLock { live } }
        func tearDown() { lock.withLock { live = false } }
    }

    /// Hands the test each start's context, so it can play the recognizer.
    private final class Harness: @unchecked Sendable {
        private let lock = NSLock()
        private var _contexts: [LegacyCaptureContext] = []
        private var _captures: [FakeCapture] = []
        var contexts: [LegacyCaptureContext] { lock.withLock { _contexts } }
        var captures: [FakeCapture] { lock.withLock { _captures } }

        func session(
            onDeviceOnly: Bool = true, resumes: Bool = false,
            speech: @escaping @Sendable () async -> Bool = { true }, mic: @escaping @Sendable () async -> Bool = { true }
        ) -> LiveDictationSession {
            LiveDictationSession(
                localeCandidates: [Locale(identifier: "en_US")],
                speechAuthorization: speech, microphonePermission: mic,
                legacyFactory: { [self] context in
                    let capture = FakeCapture()
                    lock.withLock { _contexts.append(context); _captures.append(capture) }
                    return capture
                },
                modernEnabled: false, onDeviceOnly: onDeviceOnly, resumesAfterInterruption: resumes)
        }
    }

    private func started(_ h: Harness, _ session: LiveDictationSession, _ events: Events) async {
        session.start(onEvent: events.add)
        await session.lastStartTask?.value
        #expect(h.contexts.count >= 1)
    }

    // MARK: Event.stopped (#595)

    @Test func stopEmitsStoppedExactlyOnce() async {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        session.stop()
        session.stop()
        #expect(events.stops == 1)
        #expect(h.captures.first?.isInstalled == false)
    }

    @Test func aFinalResultOnTheLegacyPathEndsTheSessionWithStopped() async throws {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        let context = try #require(h.contexts.first)
        context.emit(.final("hello world"))
        context.stop()
        #expect(events.all.suffix(2) == [.final("hello world"), .stopped])
        #expect(events.stops == 1)
        #expect(session.finalizedText == "hello world")
        #expect(h.captures.first?.isInstalled == false)
    }

    @Test func segmentFinalsKeepTheSessionRunningAndAccumulate() async throws {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        let context = try #require(h.contexts.first)
        context.emit(.partial("so"))
        context.emit(.final("so we begin"))
        context.emit(.partial("then"))
        context.emit(.final("  then we pause "))
        #expect(events.stops == 0, "a segment final must not look like the end of the session")
        #expect(session.finalizedText == "so we begin then we pause")
        #expect(h.captures.first?.isInstalled == true)
        session.stop()
        #expect(events.stops == 1)
        #expect(session.finalizedText == "so we begin then we pause", "the text is still readable at .stopped")
    }

    @Test func aNewStartClearsTheFinalizedText() async throws {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        try #require(h.contexts.first).emit(.final("old"))
        session.start(onEvent: events.add)
        await session.lastStartTask?.value
        #expect(session.finalizedText.isEmpty)
    }

    @Test func aReplacedStartEndsSilently() async {
        let h = Harness(), a = Events(), b = Events()
        let session = h.session()
        session.start(onEvent: a.add)
        await session.lastStartTask?.value
        session.start(onEvent: b.add)
        await session.lastStartTask?.value
        #expect(a.stops == 0, "the owner asked for the replacement; a stale .stopped would read as the new run ending")
        #expect(b.stops == 0)
        session.stop()
        #expect(b.stops == 1)
        #expect(a.stops == 0)
    }

    @Test func eventsAfterStoppedNeverReachTheOwner() async throws {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        let context = try #require(h.contexts.first)
        session.stop()
        let count = events.all.count
        context.emit(.partial("late"))
        context.emit(.final("late"))
        #expect(events.all.count == count)
    }

    @Test func aPermissionDenialIsFailedThenStopped() async {
        let h = Harness(), events = Events()
        let session = h.session(speech: { false })
        session.start(onEvent: events.add)
        await session.lastStartTask?.value
        #expect(events.failures == ["Dictation needs Speech Recognition access."])
        #expect(events.all.last == .stopped)
        #expect(events.stops == 1)
        #expect(h.contexts.isEmpty)
    }

    @Test func aRecognizerFailureIsFailedThenStopped() async throws {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        let context = try #require(h.contexts.first)
        context.emit(.failed("Couldn't transcribe that."))
        context.stop()
        #expect(events.all.suffix(2) == [.failed("Couldn't transcribe that."), .stopped])
    }

    // MARK: Interruption (#597)

    @Test func anInterruptionEndsTheSessionAndTellsTheOwnerWhy() async {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        session.handleSessionEvent(.interruptionBegan)
        #expect(events.all.suffix(2) == [.interrupted(.began), .stopped])
        #expect(h.captures.first?.isInstalled == false, "the engine must not keep running through a call")
    }

    @Test func aLostInputRouteEndsTheSessionButANewDeviceDoesNot() async {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        session.handleSessionEvent(.routeChanged(.newDeviceAvailable))
        session.handleSessionEvent(.routeChanged(.categoryChange))
        #expect(events.stops == 0)
        session.handleSessionEvent(.routeChanged(.oldDeviceUnavailable))
        #expect(events.all.suffix(2) == [.interrupted(.inputRouteLost), .stopped])
    }

    @Test func aMediaServicesResetEndsTheSession() async {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        session.handleSessionEvent(.mediaServicesReset)
        #expect(events.all.suffix(2) == [.interrupted(.mediaServicesReset), .stopped])
    }

    @Test func sessionEventsWithNothingRunningAreIgnored() {
        let session = Harness().session()
        session.handleSessionEvent(.interruptionBegan)
        session.handleSessionEvent(.mediaServicesReset)
    }

    @Test func theSessionStaysEndedAfterAnInterruptionByDefault() async {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        session.handleSessionEvent(.interruptionBegan)
        session.handleSessionEvent(.interruptionEnded(shouldResume: true))
        await session.lastStartTask?.value
        #expect(h.contexts.count == 1, "dictation must not restart itself unless the app opted in")
    }

    @Test func anOptedInSessionResumesWhenTheSystemRecommendsIt() async {
        let h = Harness(), events = Events()
        let session = h.session(resumes: true)
        await started(h, session, events)
        session.handleSessionEvent(.interruptionBegan)
        #expect(events.stops == 1)
        session.handleSessionEvent(.interruptionEnded(shouldResume: true))
        await session.lastStartTask?.value
        #expect(h.contexts.count == 2, "a second capture starts on the same owner")
        #expect(h.captures[1].isInstalled)
        #expect(events.contains(.status(nil)))
        session.stop()
        #expect(events.stops == 2)
    }

    @Test func aResumeNotRecommendedLeavesTheSessionEnded() async {
        let h = Harness(), events = Events()
        let session = h.session(resumes: true)
        await started(h, session, events)
        session.handleSessionEvent(.interruptionBegan)
        session.handleSessionEvent(.interruptionEnded(shouldResume: false))
        session.handleSessionEvent(.interruptionEnded(shouldResume: true))  // the pending resume is already cleared
        await session.lastStartTask?.value
        #expect(h.contexts.count == 1)
    }

    @Test func anExplicitStopCancelsAPendingResume() async {
        let h = Harness(), events = Events()
        let session = h.session(resumes: true)
        await started(h, session, events)
        session.handleSessionEvent(.interruptionBegan)
        session.stop()
        session.handleSessionEvent(.interruptionEnded(shouldResume: true))
        await session.lastStartTask?.value
        #expect(h.contexts.count == 1)
    }

    // MARK: Network opt-in (#599)

    @Test func theLegacyPathFailsClosedByDefault() async throws {
        let h = Harness(), events = Events()
        let session = h.session()
        await started(h, session, events)
        #expect(try #require(h.contexts.first).onDeviceOnly)
    }

    @Test func networkRecognitionIsAnExplicitOptIn() async throws {
        let h = Harness(), events = Events()
        let session = h.session(onDeviceOnly: false)
        await started(h, session, events)
        #expect(try #require(h.contexts.first).onDeviceOnly == false)
    }
}

struct LegacyNetworkFallbackPolicyTests {
    private struct Fake { let available: Bool; let onDevice: Bool; let tag: String }

    private let networkOnly = Fake(available: true, onDevice: false, tag: "network-only")
    private let local = Fake(available: true, onDevice: true, tag: "local")
    private let offline = Fake(available: false, onDevice: true, tag: "unavailable")

    @Test func aLocalRecognizerAlwaysWinsEvenWithNetworkAllowed() throws {
        let picked = try LegacySpeechPolicy.select(
            from: [networkOnly, local], allowsNetwork: true, isAvailable: { $0.available }, supportsOnDevice: { $0.onDevice })
        #expect(picked.tag == "local")
    }

    @Test func withNetworkAllowedAnAvailableRecognizerIsTheFallback() throws {
        let picked = try LegacySpeechPolicy.select(
            from: [offline, networkOnly], allowsNetwork: true, isAvailable: { $0.available }, supportsOnDevice: { $0.onDevice })
        #expect(picked.tag == "network-only")
    }

    @Test func withNetworkAllowedAnUnavailableRecognizerIsStillRefused() {
        #expect(throws: SpeechError.self) {
            try LegacySpeechPolicy.select(
                from: [offline], allowsNetwork: true, isAvailable: { $0.available }, supportsOnDevice: { $0.onDevice })
        }
    }

    @Test func withoutTheOptInNetworkOnlyRecognizersAreRefused() {
        #expect(throws: SpeechError.self) {
            try LegacySpeechPolicy.select(from: [networkOnly], isAvailable: { $0.available }, supportsOnDevice: { $0.onDevice })
        }
    }

    @Test func configureForcesOnDeviceWhenPossibleAndReleasesItOnlyForTheOptIn() throws {
        let supported = SFSpeechURLRecognitionRequest(url: URL(fileURLWithPath: "/dev/null"))
        try LegacySpeechPolicy.configure(supported, supportsOnDevice: true, allowsNetwork: true)
        #expect(supported.requiresOnDeviceRecognition, "a recognizer with the model is forced on-device even when network is allowed")

        let remote = SFSpeechURLRecognitionRequest(url: URL(fileURLWithPath: "/dev/null"))
        try LegacySpeechPolicy.configure(remote, supportsOnDevice: false, allowsNetwork: true)
        #expect(!remote.requiresOnDeviceRecognition)

        let refused = SFSpeechURLRecognitionRequest(url: URL(fileURLWithPath: "/dev/null"))
        #expect(throws: SpeechError.self) { try LegacySpeechPolicy.configure(refused, supportsOnDevice: false) }
    }
}
