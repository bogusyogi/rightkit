import Foundation
import Testing

@testable import RightKitSpeech

struct DictationDraftTests {
    @Test func draftReplacesTheSpokenPartAgainstOneFrozenBase() {
        // Partial results re-render the SAME draft; they must never accumulate.
        let base = "  Fix the build  "
        #expect(Dictation.draft(base: base, transcript: "please") == "Fix the build please")
        #expect(Dictation.draft(base: base, transcript: "please run the tests") == "Fix the build please run the tests")
        #expect(Dictation.draft(base: base, transcript: "  ") == "Fix the build")
        #expect(Dictation.draft(base: "", transcript: "hello") == "hello")
    }

    @Test func localeCandidatesAreOrderedBoundedAndUnique() {
        let candidates = Dictation.localeCandidates(
            preferredLanguages: ["fr-FR", "en-US", "de-DE", "es-ES", "it-IT"], current: Locale(identifier: "fr_FR"))
        let ids = candidates.map(Dictation.canonicalIdentifier)
        #expect(ids.first == "fr_fr")
        #expect(ids.count == Set(ids).count)
        #expect(!ids.contains("es_es"), "only the first three preferred languages count")
        #expect(ids.last == "de_de")
    }
}

// MARK: Legacy Apple Speech is local-only (A01)

import Speech

struct LegacySpeechPolicyTests {
    private struct FakeRecognizer { let available: Bool; let onDevice: Bool; let tag: String }

    @Test func selectionNeverFallsBackToANetworkCapableRecognizer() throws {
        let candidates = [
            FakeRecognizer(available: true, onDevice: false, tag: "network-only"),
            FakeRecognizer(available: false, onDevice: true, tag: "offline-unavailable"),
            FakeRecognizer(available: true, onDevice: true, tag: "local"),
        ]
        let picked = try LegacySpeechPolicy.select(from: candidates, isAvailable: { $0.available }, supportsOnDevice: { $0.onDevice })
        #expect(picked.tag == "local")

        #expect(throws: SpeechError.self) {
            try LegacySpeechPolicy.select(from: Array(candidates.prefix(2)), isAvailable: { $0.available }, supportsOnDevice: { $0.onDevice })
        }
        #expect(throws: SpeechError.self) {
            try LegacySpeechPolicy.select(from: [FakeRecognizer](), isAvailable: { $0.available }, supportsOnDevice: { $0.onDevice })
        }
    }

    @Test func requestsAreForcedOnDeviceAndUnsupportedRecognizersFailBeforeAnyTask() throws {
        let supported = SFSpeechURLRecognitionRequest(url: URL(fileURLWithPath: "/dev/null"))
        try LegacySpeechPolicy.configure(supported, supportsOnDevice: true)
        #expect(supported.requiresOnDeviceRecognition)

        let unsupported = SFSpeechURLRecognitionRequest(url: URL(fileURLWithPath: "/dev/null"))
        #expect(throws: SpeechError.self) {
            try LegacySpeechPolicy.configure(unsupported, supportsOnDevice: false, locale: Locale(identifier: "xx"))
        }
        #expect(!unsupported.requiresOnDeviceRecognition, "no network-capable request is left configured")

        let live = SFSpeechAudioBufferRecognitionRequest()
        #expect(throws: SpeechError.self) { try LegacySpeechPolicy.configure(live, supportsOnDevice: false) }
    }
}

// MARK: Legacy file transcription honors cancellation (A05)

struct CancellableRecognitionTests {
    /// Fake callback recognizer: records start/cancel counts and lets the test fire the callback.
    private final class Fake: @unchecked Sendable {
        private let lock = NSLock()
        private(set) var starts = 0
        private(set) var cancels = 0
        private var completion: CancellableRecognition.Completion?
        var started: Bool { lock.withLock { starts > 0 } }

        func start(_ complete: @escaping CancellableRecognition.Completion) -> (@Sendable () -> Void) {
            lock.withLock { starts += 1; completion = complete }
            return { [self] in lock.withLock { cancels += 1 } }
        }
        func fire(_ result: Result<String, Error>) { lock.withLock { completion }?(result) }
        func waitUntilStarted() async { while !started { await Task.yield() } }
    }

    @Test func completionResumesOnceAndLateDuplicatesAreIgnored() async throws {
        let fake = Fake()
        let task = Task { try await CancellableRecognition().run { fake.start($0) } }
        await fake.waitUntilStarted()
        fake.fire(.success("hello"))
        fake.fire(.success("again"))
        fake.fire(.failure(SpeechError.decodeFailed("late")))
        #expect(try await task.value == "hello")
        #expect(fake.cancels == 0)
    }

    @Test func cancelBeforeStartNeverStartsRecognition() async {
        let fake = Fake()
        let gate = AsyncStream<Void>.makeStream()
        let task = Task {
            for await _ in gate.stream {}  // returns only after the task has been cancelled
            return try await CancellableRecognition().run { fake.start($0) }
        }
        task.cancel()
        gate.continuation.finish()
        await #expect(throws: CancellationError.self) { try await task.value }
        #expect(fake.starts == 0)
    }

    @Test func cancelWhileRecognizingCancelsTheTaskOnceAndDropsALateTranscript() async {
        let fake = Fake()
        let task = Task { try await CancellableRecognition().run { fake.start($0) } }
        await fake.waitUntilStarted()
        task.cancel()
        await #expect(throws: CancellationError.self) { try await task.value }
        #expect(fake.cancels == 1)
        fake.fire(.success("too late"))  // racing final callback: must not resume a second time
        fake.fire(.failure(SpeechError.decodeFailed("late error")))
        #expect(fake.cancels == 1)
    }
}

// MARK: Stale permission failures (A04)

struct LiveDictationPermissionTests {
    private final class Events: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: [LiveDictationSession.Event] = []
        func add(_ event: LiveDictationSession.Event) { lock.withLock { stored.append(event) } }
        var all: [LiveDictationSession.Event] { lock.withLock { stored } }
        var failures: [String] { all.compactMap { if case let .failed(text) = $0 { text } else { nil } } }
    }

    /// A permission provider the test releases by hand, answering the call that is waiting.
    private final class Gate: @unchecked Sendable {
        private let lock = NSLock()
        private var waiting: [CheckedContinuation<Bool, Never>] = []
        func ask() async -> Bool { await withCheckedContinuation { c in lock.withLock { waiting.append(c) } } }
        var pending: Int { lock.withLock { waiting.count } }
        func answer(_ value: Bool) { lock.withLock { waiting.removeFirst() }.resume(returning: value) }
    }

    @Test func aReplacedStartDoesNotReportItsDelayedSpeechDenial() async throws {
        let speech = Gate()
        let session = LiveDictationSession(localeCandidates: [Locale(identifier: "en_US")],
                                           speechAuthorization: { await speech.ask() },
                                           microphonePermission: { false })
        let eventsA = Events(), eventsB = Events()
        session.start(onEvent: eventsA.add)
        let taskA = try #require(session.lastStartTask)
        while speech.pending < 1 { await Task.yield() }
        session.start(onEvent: eventsB.add)  // replaces A before A's prompt answers
        let taskB = try #require(session.lastStartTask)
        while speech.pending < 2 { await Task.yield() }

        speech.answer(false)  // A's delayed denial arrives after it was replaced
        await taskA.value
        #expect(eventsA.failures.isEmpty, "a replaced start must not emit an obsolete terminal event")
        #expect(eventsB.failures.isEmpty, "A's denial must not leak to the current start either")

        speech.answer(false)  // the current start keeps normal failure behavior
        await taskB.value
        #expect(eventsB.failures == ["Dictation needs Speech Recognition access."])
    }

    @Test func aStoppedStartDoesNotReportItsDelayedMicrophoneDenial() async throws {
        let mic = Gate()
        let session = LiveDictationSession(localeCandidates: [Locale(identifier: "en_US")],
                                           speechAuthorization: { true },
                                           microphonePermission: { await mic.ask() })
        let events = Events()
        session.start(onEvent: events.add)
        let task = try #require(session.lastStartTask)
        while mic.pending < 1 { await Task.yield() }
        session.stop()
        mic.answer(false)
        await task.value
        #expect(events.failures.isEmpty)
    }

    @Test func aCurrentStartStillReportsMicrophoneDenial() async throws {
        let session = LiveDictationSession(localeCandidates: [Locale(identifier: "en_US")],
                                           speechAuthorization: { true }, microphonePermission: { false })
        let events = Events()
        session.start(onEvent: events.add)
        await session.lastStartTask?.value
        #expect(events.failures == ["Dictation needs Microphone access."])
    }
}

// MARK: Stop during legacy setup (S02)

struct LiveDictationLegacySetupTests {
    private final class Events: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: [LiveDictationSession.Event] = []
        func add(_ event: LiveDictationSession.Event) { lock.withLock { stored.append(event) } }
        var all: [LiveDictationSession.Event] { lock.withLock { stored } }
        var failures: [String] { all.compactMap { if case let .failed(text) = $0 { text } else { nil } } }
    }

    /// Stands in for the audio engine + tap + recognition task: "installed" until `tearDown()`.
    private final class FakeCapture: LegacyCaptureHandle, @unchecked Sendable {
        private let lock = NSLock()
        private var live = true
        private var count = 0
        var isInstalled: Bool { lock.withLock { live } }
        var tearDowns: Int { lock.withLock { count } }
        func tearDown() { lock.withLock { live = false; count += 1 } }
    }

    private final class Harness: @unchecked Sendable {
        let captures = Captures()
        let events = Events()
        var session: LiveDictationSession!
        /// Runs inside the factory after a capture exists but before it is returned for publication,
        /// i.e. exactly where the race window used to be.
        var duringSetup: [@Sendable (Harness, Int) throws -> Void] = []

        final class Captures: @unchecked Sendable {
            private let lock = NSLock()
            private var stored: [FakeCapture] = []
            func add(_ capture: FakeCapture) -> Int { lock.withLock { stored.append(capture); return stored.count - 1 } }
            var all: [FakeCapture] { lock.withLock { stored } }
        }

        func makeSession() {
            session = LiveDictationSession(
                localeCandidates: [Locale(identifier: "en_US")],
                speechAuthorization: { true }, microphonePermission: { true },
                legacyFactory: { [unowned self] _ in
                    let capture = FakeCapture()
                    let index = captures.add(capture)
                    if index < duringSetup.count {
                        do { try duringSetup[index](self, index) } catch { capture.tearDown(); throw error }
                    }
                    return capture
                },
                modernEnabled: false)
        }
    }

    @Test func aStopDuringSetupLeavesNothingInstalled() async throws {
        let h = Harness()
        h.duringSetup = [{ harness, _ in harness.session.stop() }]
        h.makeSession()
        h.session.start(onEvent: h.events.add)
        await h.session.lastStartTask?.value

        let capture = try #require(h.captures.all.first)
        #expect(!capture.isInstalled, "capture built during a stopped start must be torn down")
        #expect(!h.events.all.contains(.status(nil)), "a stopped start must not report it is listening")
        #expect(h.events.failures.isEmpty)
    }

    @Test func aReplacingStartDuringSetupDisposesOnlyTheStaleCapture() async throws {
        let h = Harness()
        h.makeSession()
        let eventsB = Events()
        h.duringSetup = [{ harness, _ in harness.session.start(onEvent: eventsB.add) }]
        h.session.start(onEvent: h.events.add)
        let taskA = try #require(h.session.lastStartTask)
        await taskA.value
        await h.session.lastStartTask?.value  // start B

        let captures = h.captures.all
        #expect(captures.count == 2)
        #expect(!captures[0].isInstalled, "stale start A must dispose its own capture")
        #expect(captures[1].isInstalled, "start B's capture must survive A's unwinding")
        #expect(!h.events.all.contains(.status(nil)))
        #expect(eventsB.all.contains(.status(nil)))

        h.session.stop()
        #expect(!captures[1].isInstalled)
        h.session.stop()  // idempotent
        #expect(captures[1].tearDowns >= 1)
    }

    @Test func aStaleStartFailureNeitherTearsDownNorReportsToItsSuccessor() async throws {
        let h = Harness()
        h.makeSession()
        let eventsB = Events()
        struct Boom: Error {}
        h.duringSetup = [{ harness, _ in
            harness.session.start(onEvent: eventsB.add)
            throw Boom()
        }]
        h.session.start(onEvent: h.events.add)
        let taskA = try #require(h.session.lastStartTask)
        await taskA.value
        await h.session.lastStartTask?.value

        let captures = h.captures.all
        #expect(captures.count == 2)
        #expect(captures[1].isInstalled, "A's failure path must not tear down B")
        #expect(h.events.failures.isEmpty, "a superseded start reports nothing")
        #expect(eventsB.failures.isEmpty)
        h.session.stop()
    }

    @Test func aCurrentStartPublishesAndStopReleasesIt() async throws {
        let h = Harness()
        h.makeSession()
        h.session.start(onEvent: h.events.add)
        await h.session.lastStartTask?.value
        let capture = try #require(h.captures.all.first)
        #expect(capture.isInstalled)
        #expect(h.events.all.contains(.status(nil)))
        h.session.stop()
        #expect(!capture.isInstalled)
    }

    @Test func aCurrentSetupFailureIsReportedAndUnwound() async throws {
        let h = Harness()
        struct Boom: Error {}
        h.duringSetup = [{ _, _ in throw Boom() }]
        h.makeSession()
        h.session.start(onEvent: h.events.add)
        await h.session.lastStartTask?.value
        #expect(h.events.failures == ["Couldn't start the microphone."])
        #expect(try #require(h.captures.all.first).isInstalled == false)
    }
}
