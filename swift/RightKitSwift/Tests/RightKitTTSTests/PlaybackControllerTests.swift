import AVFoundation
import Foundation
import RightKitAudio
import Testing

@testable import RightKitTTS

@MainActor
private final class FakeEngine: PlaybackEngine {
    struct Queued {
        let chunk: Int
        let completion: @Sendable () -> Void
    }

    var onConfigurationChange: (@MainActor () -> Void)?
    private(set) var log: [String] = []
    private(set) var queue: [Queued] = []
    private(set) var rate: Float = 1
    var failStart = false

    struct StartFailure: Error {}

    func start(format: AVAudioFormat, rate: Float) throws {
        if failStart { throw StartFailure() }
        log.append("start")
        self.rate = rate
        queue = []
    }

    func schedule(_ buffer: AVAudioPCMBuffer, completion: @escaping @Sendable () -> Void) {
        // Chunk `n` renders `100 + n` frames, so the buffer says which chunk it is.
        queue.append(Queued(chunk: Int(buffer.frameLength) - 100, completion: completion))
        log.append("schedule \(Int(buffer.frameLength) - 100)")
    }

    func play() { log.append("play") }
    func pause() { log.append("pause") }
    func resetQueue() { queue = []; log.append("reset") }
    func stop() { queue = []; log.append("stop") }
    func setRate(_ rate: Float) { self.rate = rate; log.append("rate \(rate)") }

    /// Plays a queued chunk out, as the audio hardware would.
    func finish(_ chunk: Int) {
        guard let position = queue.firstIndex(where: { $0.chunk == chunk }) else { return }
        queue.remove(at: position).completion()
    }

    /// Runs a completion captured earlier (one the controller has since dropped).
    func rerun(_ completion: @Sendable () -> Void) { completion() }

    var scheduledChunks: [Int] { queue.map(\.chunk) }
    func count(_ entry: String) -> Int { log.filter { $0 == entry }.count }
}

private final class Tally: @unchecked Sendable {
    private let lock = NSLock()
    private var _activations = 0
    private var _releases = 0
    var failActivation = false
    var activations: Int { lock.withLock { _activations } }
    var releases: Int { lock.withLock { _releases } }
    func activate() { lock.withLock { _activations += 1 } }
    func release() { lock.withLock { _releases += 1 } }
}

private func chunk(_ index: Int, of count: Int) -> AudioChunk {
    let samples = (0..<(100 + index)).map { 0.1 * sin(Float($0) * 0.3) }
    return AudioChunk(
        index: index, count: count, text: "chunk \(index)", sourceRange: index ..< index + 1, samples: samples,
        format: PCMFormat(sampleRate: 22_050))
}

@MainActor
private struct Rig {
    let engine = FakeEngine()
    let tally = Tally()
    let continuation: AsyncThrowingStream<AudioChunk, Error>.Continuation
    let controller: PlaybackController

    init(session: AudioSessionConfiguration? = .spokenPlayback) {
        let (stream, continuation) = AsyncThrowingStream.makeStream(of: AudioChunk.self, throwing: Error.self)
        let engine = self.engine
        let tally = self.tally
        self.continuation = continuation
        controller = PlaybackController(
            chunkSource: { _ in stream },
            makeEngine: { engine },
            session: session,
            activateSession: { _ in
                tally.activate()
                if tally.failActivation { throw FakeEngine.StartFailure() }
            },
            releaseSession: { _ in tally.release() },
            makeMonitor: { AudioSessionMonitor(handler: $0) })
    }

    /// Reads and delivers `count` chunks, leaving synthesis running.
    func startReading(delivering delivered: Int, of count: Int) async {
        controller.read(text: "anything")
        for index in 0..<delivered { continuation.yield(chunk(index, of: count)) }
        await settle { controller.chunkCount == delivered && (delivered == 0 || controller.state != .synthesizing) }
    }
}

/// Lets the controller's tasks run until `condition` holds.
@MainActor
private func settle(_ condition: @MainActor () -> Bool) async {
    for _ in 0..<2_000 where !condition() { try? await Task.sleep(nanoseconds: 1_000_000) }
}

@MainActor
struct PlaybackControllerTests {
    @Test func playsAsSoonAsTheFirstChunkIsRendered() async {
        let rig = Rig()
        rig.controller.read(text: "hello")
        #expect(rig.controller.state == .synthesizing)
        #expect(rig.controller.isReading && !rig.controller.isPlaying)

        rig.continuation.yield(chunk(0, of: 3))
        await settle { rig.controller.state == .playing }
        #expect(rig.controller.state == .playing)
        #expect(rig.engine.log == ["start", "schedule 0", "play"])
        #expect(rig.controller.currentChunkText == "chunk 0")
        #expect(rig.tally.activations == 1)
    }

    @Test func laterChunksAreQueuedAsTheyArriveAndAdvanceTheCurrentChunk() async {
        let rig = Rig()
        await rig.startReading(delivering: 3, of: 3)
        #expect(rig.engine.scheduledChunks == [0, 1, 2])
        #expect(rig.controller.chunkCount == 3 && rig.controller.currentChunkIndex == 0)

        rig.engine.finish(0)
        await settle { rig.controller.currentChunkIndex == 1 }
        #expect(rig.controller.currentChunkIndex == 1)
        #expect(rig.controller.currentChunkText == "chunk 1")
        rig.engine.finish(1)
        await settle { rig.controller.currentChunkIndex == 2 }
        #expect(rig.controller.currentChunkText == "chunk 2")
        #expect(rig.controller.state == .playing, "synthesis has not finished, so the request is still open")
    }

    @Test func playbackEndsWhenTheLastChunkFinishesAfterSynthesisCompletes() async {
        let rig = Rig()
        await rig.startReading(delivering: 2, of: 2)
        rig.continuation.finish()
        await settle { rig.controller.isSynthesisComplete }
        rig.engine.finish(0)
        rig.engine.finish(1)
        await settle { rig.controller.state == .idle }
        #expect(rig.controller.state == .idle)
        #expect(rig.engine.log.last == "stop")
        await settle { rig.tally.releases == 1 }
        #expect(rig.tally.releases == 1, "the audio session is handed back when reading ends")
        #expect(rig.controller.errorMessage == nil)
    }

    @Test func aRequestWhosePlaybackOutranSynthesisStillEndsWhenSynthesisFinishes() async {
        let rig = Rig()
        await rig.startReading(delivering: 1, of: 2)
        rig.engine.finish(0)  // chunk 0 plays out before chunk 1 exists
        await settle { rig.controller.currentChunkIndex == 0 && rig.engine.queue.isEmpty }
        #expect(rig.controller.state == .playing, "an underrun waits for the next chunk")

        rig.continuation.yield(chunk(1, of: 2))
        await settle { rig.engine.scheduledChunks == [1] }
        #expect(rig.engine.scheduledChunks == [1])
        rig.engine.finish(1)
        await settle { rig.controller.currentChunkIndex == 1 }
        rig.continuation.finish()
        await settle { rig.controller.state == .idle }
        #expect(rig.controller.state == .idle)
    }

    @Test func pauseAndResumeKeepTheEngineAndReactivateTheSession() async {
        let rig = Rig()
        await rig.startReading(delivering: 1, of: 2)
        rig.controller.pause()
        #expect(rig.controller.state == .paused && rig.controller.isPaused)
        #expect(rig.engine.log.last == "pause")
        rig.controller.pause()  // already paused: nothing happens
        #expect(rig.engine.count("pause") == 1)

        rig.controller.resume()
        await settle { rig.controller.state == .playing }
        #expect(rig.controller.state == .playing)
        #expect(rig.engine.log.last == "play")
        #expect(rig.tally.activations == 2)
        #expect(rig.engine.count("start") == 1, "the graph is not rebuilt for an ordinary pause")
    }

    @Test func pausingWhileNothingIsPlayingDoesNothing() {
        let rig = Rig()
        rig.controller.pause()
        rig.controller.resume()
        #expect(rig.controller.state == .idle)
        #expect(rig.engine.log.isEmpty)
    }

    @Test func skippingDropsTheQueueAndReplaysFromTheChosenChunk() async {
        let rig = Rig()
        await rig.startReading(delivering: 4, of: 4)
        let stale = rig.engine.queue[0].completion  // chunk 0's completion, from the queue about to be dropped

        rig.controller.skip(to: 2)
        #expect(rig.controller.currentChunkIndex == 2)
        #expect(rig.controller.currentChunkText == "chunk 2")
        #expect(rig.engine.scheduledChunks == [2, 3])
        #expect(rig.engine.log.suffix(4) == ["reset", "schedule 2", "schedule 3", "play"])

        rig.engine.rerun(stale)  // a completion from the old queue must not move the position
        try? await Task.sleep(nanoseconds: 20_000_000)
        #expect(rig.controller.currentChunkIndex == 2)

        rig.controller.skipBackward()
        #expect(rig.controller.currentChunkIndex == 1 && rig.engine.scheduledChunks == [1, 2, 3])
        rig.controller.skipForward()
        #expect(rig.controller.currentChunkIndex == 2)
    }

    @Test func skippingIsClampedToTheRenderedChunks() async {
        let rig = Rig()
        await rig.startReading(delivering: 2, of: 5)
        rig.controller.skipForward()
        #expect(rig.controller.currentChunkIndex == 1)
        rig.controller.skipForward()  // chunk 2 does not exist yet
        #expect(rig.controller.currentChunkIndex == 1)
        rig.controller.skip(to: 99)
        #expect(rig.controller.currentChunkIndex == 1)
        rig.controller.skip(to: -4)
        #expect(rig.controller.currentChunkIndex == 0)
        rig.controller.skipBackward()
        #expect(rig.controller.currentChunkIndex == 0)
    }

    @Test func skippingWhilePausedStaysPausedUntilResumed() async {
        let rig = Rig()
        await rig.startReading(delivering: 3, of: 3)
        rig.controller.pause()
        let plays = rig.engine.count("play")
        rig.controller.skip(to: 2)
        #expect(rig.controller.state == .paused)
        #expect(rig.engine.count("play") == plays, "a paused skip must not start the audio")
        #expect(rig.engine.scheduledChunks == [2])
    }

    @Test func rateIsClampedAndAppliedToTheEngine() async {
        let rig = Rig()
        rig.controller.setRate(1.5)
        #expect(rig.controller.rate == 1.5)
        await rig.startReading(delivering: 1, of: 1)
        #expect(rig.engine.rate == 1.5, "a new graph starts at the chosen speed")
        rig.controller.setRate(9)
        #expect(rig.controller.rate == 2 && rig.engine.rate == 2)
        rig.controller.setRate(0.1)
        #expect(rig.controller.rate == 0.5 && rig.engine.rate == 0.5)
    }

    // MARK: Interruption and route changes

    @Test func anInterruptionPausesAndTheSystemsGoAheadResumes() async {
        let rig = Rig()
        await rig.startReading(delivering: 2, of: 2)
        await rig.controller.handleSessionEvent(.interruptionBegan)
        #expect(rig.controller.state == .paused)
        await rig.controller.handleSessionEvent(.interruptionEnded(shouldResume: true))
        #expect(rig.controller.state == .playing)
        #expect(rig.tally.activations == 2, "the session is taken back before the audio resumes")
    }

    @Test func anInterruptionWithoutAResumeRecommendationLeavesPlaybackPaused() async {
        let rig = Rig()
        await rig.startReading(delivering: 1, of: 1)
        await rig.controller.handleSessionEvent(.interruptionBegan)
        await rig.controller.handleSessionEvent(.interruptionEnded(shouldResume: false))
        #expect(rig.controller.state == .paused)
    }

    @Test func aUserPauseIsNeverOverriddenByAnInterruptionEnding() async {
        let rig = Rig()
        await rig.startReading(delivering: 1, of: 1)
        rig.controller.pause()
        await rig.controller.handleSessionEvent(.interruptionBegan)
        await rig.controller.handleSessionEvent(.interruptionEnded(shouldResume: true))
        #expect(rig.controller.state == .paused, "the user paused first; a call ending must not start the speech again")
    }

    @Test func unpluggingHeadphonesPausesAndNeverResumesByItself() async {
        let rig = Rig()
        await rig.startReading(delivering: 1, of: 1)
        await rig.controller.handleSessionEvent(.routeChanged(.newDeviceAvailable))
        await rig.controller.handleSessionEvent(.routeChanged(.categoryChange))
        #expect(rig.controller.state == .playing)
        await rig.controller.handleSessionEvent(.routeChanged(.oldDeviceUnavailable))
        #expect(rig.controller.state == .paused)
        await rig.controller.handleSessionEvent(.interruptionEnded(shouldResume: true))
        #expect(rig.controller.state == .paused)
    }

    @Test func aMediaServicesResetRebuildsTheGraphAndContinuesFromTheChunkInProgress() async {
        let rig = Rig()
        await rig.startReading(delivering: 3, of: 3)
        rig.engine.finish(0)
        await settle { rig.controller.currentChunkIndex == 1 }

        await rig.controller.handleSessionEvent(.mediaServicesReset)
        #expect(rig.engine.count("start") == 2)
        #expect(rig.engine.scheduledChunks == [1, 2])
        #expect(rig.controller.state == .playing)
        #expect(rig.tally.activations == 2, "the session is reconfigured after the reset")
    }

    @Test func aPausedReadStaysPausedAcrossAGraphRebuild() async {
        let rig = Rig()
        await rig.startReading(delivering: 2, of: 2)
        rig.controller.pause()
        let plays = rig.engine.count("play")
        rig.engine.onConfigurationChange?()  // Core Audio reconfigured the engine
        await settle { rig.engine.count("start") == 2 }
        #expect(rig.engine.count("start") == 2)
        #expect(rig.controller.state == .paused)
        #expect(rig.engine.count("play") == plays)
        #expect(rig.engine.scheduledChunks == [0, 1])
    }

    // MARK: Failure, stop, replacement

    @Test func aSynthesisFailureEndsReadingWithAMessage() async {
        let rig = Rig()
        await rig.startReading(delivering: 1, of: 3)
        rig.continuation.finish(throwing: SynthesisError.synthesisFailed(segment: 1, reason: "boom"))
        await settle { rig.controller.state == .idle }
        #expect(rig.controller.state == .idle)
        #expect(rig.controller.errorMessage == "Speech synthesis failed for segment 2: boom")
        rig.controller.dismissError()
        #expect(rig.controller.errorMessage == nil)
    }

    @Test func aFailureBeforeAnyAudioIsReportedToo() async {
        let rig = Rig()
        rig.controller.read(text: "x")
        rig.continuation.finish(throwing: SynthesisError.noVoiceAvailable(voice: "en-US"))
        await settle { rig.controller.state == .idle }
        #expect(rig.controller.errorMessage == "No speech voice is available for en-US.")
        #expect(rig.engine.log.isEmpty)
    }

    @Test func anEngineThatCannotStartEndsReadingWithAMessage() async {
        let rig = Rig()
        rig.engine.failStart = true
        rig.controller.read(text: "x")
        rig.continuation.yield(chunk(0, of: 1))
        await settle { rig.controller.state == .idle }
        #expect(rig.controller.state == .idle)
        #expect(rig.controller.errorMessage?.contains("audio engine could not start") == true)
    }

    @Test func aSessionThatCannotBeActivatedEndsReadingWithAMessage() async {
        let rig = Rig()
        rig.tally.failActivation = true
        rig.controller.read(text: "x")
        await settle { rig.controller.state == .idle }
        #expect(rig.controller.state == .idle)
        #expect(rig.controller.errorMessage != nil)
        #expect(rig.engine.log.isEmpty)
    }

    @Test func stopReleasesEverythingAndIsSafeToRepeat() async {
        let rig = Rig()
        await rig.startReading(delivering: 2, of: 3)
        rig.controller.stop()
        #expect(rig.controller.state == .idle)
        #expect(rig.controller.chunkCount == 0 && rig.controller.currentChunkIndex == 0 && rig.controller.currentChunkText == nil)
        #expect(rig.engine.log.last == "stop")
        await settle { rig.tally.releases == 1 }
        #expect(rig.tally.releases == 1)
        rig.controller.stop()
        rig.controller.skipForward()
        rig.controller.pause()
        #expect(rig.controller.state == .idle)
    }

    @Test func chunksFromAReplacedReadNeverReachTheEngine() async {
        let rig = Rig()
        rig.controller.read(text: "first")
        rig.controller.read(text: "second")  // replaces the first before anything arrived
        rig.continuation.yield(chunk(0, of: 1))
        await settle { rig.controller.state == .playing }
        // Both reads share the test's one stream; only the current read may consume it.
        #expect(rig.engine.count("start") == 1)
        #expect(rig.engine.scheduledChunks == [0])
    }
}
