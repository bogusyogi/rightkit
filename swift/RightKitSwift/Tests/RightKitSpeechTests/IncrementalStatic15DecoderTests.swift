import Foundation
import Testing

@testable import RightKitSpeech

// MARK: - Fakes

/// Opens once; every waiter (before or after `open`) resumes.
final class AsyncGate: @unchecked Sendable {
    private let lock = NSLock()
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        await withCheckedContinuation { continuation in
            let resumeNow = lock.withLock { () -> Bool in
                if opened { return true }
                waiters.append(continuation)
                return false
            }
            if resumeNow { continuation.resume() }
        }
    }

    func open() {
        let pending = lock.withLock { () -> [CheckedContinuation<Void, Never>] in
            opened = true
            defer { waiters.removeAll() }
            return waiters
        }
        pending.forEach { $0.resume() }
    }
}

/// Ordered, thread-safe event log shared by fakes and test bodies.
final class EventLog: @unchecked Sendable {
    private let lock = NSLock()
    private var entries: [String] = []
    func append(_ entry: String) { lock.withLock { entries.append(entry) } }
    var all: [String] { lock.withLock { entries } }
    func count(_ entry: String) -> Int { all.filter { $0 == entry }.count }
    func index(_ entry: String) -> Int? { all.firstIndex(of: entry) }

    /// Polls until `entry` is logged; fails the test after ~5 s.
    func waitFor(_ entry: String, timeoutMS: Int = 5_000) async throws {
        for _ in 0..<timeoutMS {
            if index(entry) != nil { return }
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        Issue.record("timed out waiting for \(entry); log: \(all)")
        throw CancellationError()
    }
}

/// Scripted decode closure: each call takes the next step, logs `decode-start-N`/`decode-end-N`,
/// records the segment length, and optionally waits on a gate (a non-interruptible prediction).
final class ScriptedDecode: @unchecked Sendable {
    enum Step { case text(String), fail, gated(AsyncGate, String) }
    private let lock = NSLock()
    private var steps: [Step]
    private var calls = 0
    private(set) var lengths: [Int] = []
    let log: EventLog

    init(_ steps: [Step], log: EventLog = EventLog()) {
        self.steps = steps
        self.log = log
    }

    var callCount: Int { lock.withLock { calls } }
    var segmentLengths: [Int] { lock.withLock { lengths } }

    var closure: IncrementalStatic15Decoder.Decode {
        { [self] segment in try await self.run(segment) }
    }

    private func run(_ segment: [Float]) async throws -> String {
        let (index, step) = lock.withLock { () -> (Int, Step) in
            calls += 1
            lengths.append(segment.count)
            return (calls, steps.isEmpty ? .text("") : steps.removeFirst())
        }
        log.append("decode-start-\(index)")
        defer { log.append("decode-end-\(index)") }
        switch step {
        case let .text(text): return text
        case .fail: throw TestDecodeFailure()
        case let .gated(gate, text):
            await gate.wait()
            return text
        }
    }
}

struct TestDecodeFailure: Error {}

/// Constant "speech" with exact-silence spans, so each quiet cut lands at a known index.
private func speech(count: Int, quiet: [Range<Int>] = []) -> [Float] {
    var samples = [Float](repeating: 0.5, count: count)
    for range in quiet { for index in range where index < count { samples[index] = 0 } }
    return samples
}

private let window = IncrementalStatic15Decoder.windowSamples
// First window: search [204_160, 240_000); silence at [220_000, 224_000) -> cut 221_760.
private let quiet1 = 220_000..<224_000
private let cut1 = 221_760
// Second window from cut1: search [425_920, 461_760); silence at [440_000, 444_000) -> cut 441_920.
private let quiet2 = 440_000..<444_000
private let cut2 = 441_920

// MARK: - Scheduling

@Suite struct IncrementalStatic15SchedulingTests {
    @Test func constantsMatchStatic15Contract() {
        #expect(IncrementalStatic15Decoder.sampleRate == 16_000)
        #expect(IncrementalStatic15Decoder.windowSamples == 240_000)
        #expect(IncrementalStatic15Decoder.paddingSamples == 35_840)
        #expect(IncrementalStatic15Decoder.silenceSamples == 3_200)
        #expect(IncrementalStatic15Decoder.silenceHopSamples == 1_600)
        #expect(IncrementalStatic15Decoder.maxOverlapCharacters == 16)
    }

    @Test func readyEndWaitsForMoreThanOneFullWindowThenCutsInQuiet() {
        #expect(IncrementalStatic15Decoder.readyEnd(samples: speech(count: window, quiet: [quiet1]), start: 0) == nil)
        #expect(IncrementalStatic15Decoder.readyEnd(samples: speech(count: window + 1, quiet: [quiet1]), start: 0) == cut1)
        #expect(IncrementalStatic15Decoder.readyEnd(samples: speech(count: 500_000, quiet: [quiet1, quiet2]), start: cut1) == cut2)
        #expect(IncrementalStatic15Decoder.readyEnd(samples: speech(count: 500_000), start: -1) == nil)
        // Uniform audio: the first (earliest) span of the trailing region wins.
        #expect(IncrementalStatic15Decoder.readyEnd(samples: speech(count: window + 1), start: 0) == 204_160 + 1_600)
    }

    @Test func quietCutRejectsRegionsShorterThanOneSilenceSpan() {
        let samples = speech(count: 10_000)
        #expect(IncrementalStatic15Decoder.quietCut(in: samples, lo: 0, hi: 3_199) == nil)
        #expect(IncrementalStatic15Decoder.quietCut(in: samples, lo: 10, hi: 10) == nil)
        #expect(IncrementalStatic15Decoder.quietCut(in: samples, lo: 0, hi: 3_200) == 1_600)
    }

    @Test func triggerRequestsOncePerNewlyAvailableWindow() {
        let trigger = IncrementalStatic15Trigger()
        #expect(!trigger.shouldRequest(appendedSamples: window))
        #expect(trigger.shouldRequest(appendedSamples: 1))
        #expect(!trigger.shouldRequest(appendedSamples: window - 1))
        #expect(trigger.shouldRequest(appendedSamples: 1))
        #expect(!trigger.shouldRequest(appendedSamples: -50), "negative deltas are ignored")
        trigger.reset()
        #expect(!trigger.shouldRequest(sampleCount: window))
        #expect(trigger.shouldRequest(sampleCount: window + 1))
        #expect(!trigger.shouldRequest(sampleCount: window + 2))
    }
}

// MARK: - Background windows

@Suite struct IncrementalStatic15BackgroundTests {
    @Test func consumeCommitsQuietCutWindowsAndAdvancesCursor() async {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        let decode = ScriptedDecode([.text("hello world"), .text("world again")])
        let samples = speech(count: 500_000, quiet: [quiet1, quiet2])

        let first = await decoder.consume(samples: samples, generation: 1, decode: decode.closure)
        #expect(first == IncrementalStatic15Decoder.Snapshot(
            generation: 1, committedText: "hello world", committedSamples: cut1,
            backgroundWindows: 1, backgroundDisabled: false, stopping: false))
        let second = await decoder.consume(samples: samples, generation: 1, decode: decode.closure)
        #expect(second.committedSamples == cut2)
        #expect(second.backgroundWindows == 2)
        // Overlap assembly: the repeated "world" at the seam appears once.
        #expect(second.committedText == "hello world again")
        #expect(decode.segmentLengths == [cut1, cut2 - cut1])
        // Nothing further is ready; no decode is launched.
        _ = await decoder.consume(samples: samples, generation: 1, decode: decode.closure)
        #expect(decode.callCount == 2)
        #expect(await decoder.current() == second)
    }

    @Test func seamIgnoresShortInWordMatchesButJoinsDisjointText() async {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        let decode = ScriptedDecode([.text("the cat"), .text("atom bomb")])
        let samples = speech(count: 500_000, quiet: [quiet1, quiet2])
        _ = await decoder.consume(samples: samples, generation: 1, decode: decode.closure)
        let snapshot = await decoder.consume(samples: samples, generation: 1, decode: decode.closure)
        #expect(snapshot.committedText == "the cat atom bomb", "two-character in-word match is not overlap")
    }

    @Test func staleGenerationConsumeLaunchesNothing() async {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 5)
        let decode = ScriptedDecode([.text("x")])
        let snapshot = await decoder.consume(samples: speech(count: 300_000, quiet: [quiet1]), generation: 4, decode: decode.closure)
        #expect(decode.callCount == 0)
        #expect(snapshot.generation == 5)
        #expect(snapshot.committedSamples == 0)
    }

    @Test func onlyOnePredictionIsInFlight() async throws {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        let gate = AsyncGate()
        let decode = ScriptedDecode([.gated(gate, "first"), .text("second")])
        let samples = speech(count: 500_000, quiet: [quiet1, quiet2])

        let firstConsume = Task { await decoder.consume(samples: samples, generation: 1, decode: decode.closure) }
        try await decode.log.waitFor("decode-start-1")
        // A meter tick while window 1 is predicting must not launch window 2.
        let concurrent = await decoder.consume(samples: samples, generation: 1, decode: decode.closure)
        #expect(decode.callCount == 1)
        #expect(concurrent.committedSamples == 0)

        gate.open()
        #expect(await firstConsume.value.committedSamples == cut1)
        _ = await decoder.consume(samples: samples, generation: 1, decode: decode.closure)
        #expect(decode.callCount == 2)
        #expect(await decoder.current().committedText == "first second")
    }

    @Test func failedBackgroundWindowDisablesFurtherAttemptsAndLeavesAudioUncommitted() async throws {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        let decode = ScriptedDecode([.text("committed"), .fail])
        let samples = speech(count: 500_000, quiet: [quiet1, quiet2])
        _ = await decoder.consume(samples: samples, generation: 1, decode: decode.closure)
        let failed = await decoder.consume(samples: samples, generation: 1, decode: decode.closure)
        #expect(failed.backgroundDisabled)
        #expect(failed.committedSamples == cut1, "failed window stays uncommitted")
        _ = await decoder.consume(samples: samples, generation: 1, decode: decode.closure)
        #expect(decode.callCount == 2, "no background decode after a failure")

        // Fallback: final decode covers the uncommitted audio from the committed cursor.
        let final = ScriptedDecode([.text("tail"), .text("end")])
        let text = try await decoder.finish(samples: samples, generation: 1, decode: final.closure)
        #expect(final.segmentLengths == [cut2 - cut1, samples.count - cut2])
        #expect(text == "committed tail end")
    }

    @Test func emptyBackgroundTextDisablesBackgroundDecode() async {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        let decode = ScriptedDecode([.text("   ")])
        let snapshot = await decoder.consume(samples: speech(count: 300_000, quiet: [quiet1]), generation: 1, decode: decode.closure)
        #expect(snapshot.backgroundDisabled)
        #expect(snapshot.committedSamples == 0)
    }
}

// MARK: - Generation fences, drain and finish

@Suite struct IncrementalStatic15FenceTests {
    @Test func resetDrainsInFlightPredictionAndDiscardsItsResult() async throws {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        let gate = AsyncGate()
        let decode = ScriptedDecode([.gated(gate, "old recording")])
        let log = decode.log
        let samples = speech(count: 300_000, quiet: [quiet1])

        let consume = Task { await decoder.consume(samples: samples, generation: 1, decode: decode.closure) }
        try await log.waitFor("decode-start-1")
        let reset = Task {
            await decoder.reset(generation: 2)
            log.append("reset-returned")
        }
        try await Task.sleep(nanoseconds: 20_000_000)
        #expect(log.index("reset-returned") == nil, "reset waits for the active prediction")
        #expect(await decoder.current().stopping)

        gate.open()
        await reset.value
        _ = await consume.value
        let end = try #require(log.index("decode-end-1"))
        let returned = try #require(log.index("reset-returned"))
        #expect(end < returned, "prediction drained before reset returned")
        #expect(await decoder.current() == IncrementalStatic15Decoder.Snapshot(
            generation: 2, committedText: "", committedSamples: 0,
            backgroundWindows: 0, backgroundDisabled: false, stopping: false))
    }

    @Test func cancelDrainsAndFencesTheGeneration() async throws {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 7)
        let gate = AsyncGate()
        let decode = ScriptedDecode([.gated(gate, "canceled")])
        let log = decode.log
        let samples = speech(count: 300_000, quiet: [quiet1])

        let consume = Task { await decoder.consume(samples: samples, generation: 7, decode: decode.closure) }
        try await log.waitFor("decode-start-1")
        let cancel = Task {
            await decoder.cancel(generation: 7)
            log.append("cancel-returned")
        }
        try await Task.sleep(nanoseconds: 20_000_000)
        #expect(log.index("cancel-returned") == nil)
        gate.open()
        await cancel.value
        _ = await consume.value
        #expect(try #require(log.index("decode-end-1")) < (try #require(log.index("cancel-returned"))))

        let snapshot = await decoder.current()
        #expect(snapshot.generation == 8, "cancel advances the generation")
        #expect(snapshot.committedText.isEmpty)
        // The canceled generation can no longer consume or finish.
        _ = await decoder.consume(samples: samples, generation: 7, decode: decode.closure)
        #expect(decode.callCount == 1)
        await #expect(throws: IncrementalStatic15Decoder.DecodeError.staleGeneration) {
            try await decoder.finish(samples: samples, generation: 7, decode: decode.closure)
        }
        // Cancel of a stale generation is a no-op.
        await decoder.cancel(generation: 7)
        #expect(await decoder.current().generation == 8)
    }

    @Test func finishWaitsForBackgroundWindowThenDecodesAllUncommittedAudio() async throws {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        let gate = AsyncGate()
        let background = ScriptedDecode([.gated(gate, "late background")])
        let log = background.log
        let samples = speech(count: 300_000, quiet: [quiet1])

        let consume = Task { await decoder.consume(samples: samples, generation: 1, decode: background.closure) }
        try await log.waitFor("decode-start-1")
        let final = ScriptedDecode([.text("final"), .text("text")], log: log)
        let finish = Task { try await decoder.finish(samples: samples, generation: 1, decode: final.closure) }
        try await Task.sleep(nanoseconds: 20_000_000)
        #expect(final.callCount == 0, "finish never overlaps the active prediction")
        gate.open()
        let text = try await finish.value
        _ = await consume.value
        // Stop discards the late background result; final decode covers everything once.
        #expect(text == "final text")
        #expect(final.segmentLengths == [cut1, samples.count - cut1])
        #expect(log.all == ["decode-start-1", "decode-end-1", "decode-start-1", "decode-end-1", "decode-start-2", "decode-end-2"])
    }

    @Test func finishRollsBackCheckpointsBeyondSealedAudio() async throws {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        let samples = speech(count: 300_000, quiet: [quiet1])
        _ = await decoder.consume(samples: samples, generation: 1, decode: ScriptedDecode([.text("discarded command")]).closure)
        #expect(await decoder.current().committedSamples == cut1)

        // A KWS cut seals audio shorter than the committed window: that checkpoint must not return.
        let sealed = Array(samples.prefix(200_000))
        let final = ScriptedDecode([.text("kept speech")])
        let text = try await decoder.finish(samples: sealed, generation: 1, decode: final.closure)
        #expect(text == "kept speech")
        #expect(final.segmentLengths == [200_000])
    }

    @Test func finishSplitsLongUncommittedTailIntoBoundedDisjointWindows() async throws {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        let samples = speech(count: 600_000, quiet: [quiet1, quiet2])
        let final = ScriptedDecode([.text("one"), .text("two"), .text("three")])
        let text = try await decoder.finish(samples: samples, generation: 1, decode: final.closure)
        #expect(final.segmentLengths == [cut1, cut2 - cut1, samples.count - cut2])
        #expect(final.segmentLengths.allSatisfy { $0 <= IncrementalStatic15Decoder.windowSamples })
        #expect(final.segmentLengths.reduce(0, +) == samples.count, "every sample decoded exactly once")
        #expect(text == "one two three")
        #expect(await decoder.current().committedSamples == samples.count)
    }

    @Test func finishWithStaleGenerationThrows() async {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 3)
        await #expect(throws: IncrementalStatic15Decoder.DecodeError.staleGeneration) {
            try await decoder.finish(samples: speech(count: 1_000), generation: 2, decode: ScriptedDecode([]).closure)
        }
    }

    @Test func cancelDuringFinalTailFencesTheResult() async throws {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        let gate = AsyncGate()
        let final = ScriptedDecode([.gated(gate, "must not append")])
        let finish = Task { try await decoder.finish(samples: speech(count: 100_000), generation: 1, decode: final.closure) }
        try await final.log.waitFor("decode-start-1")
        let cancel = Task { await decoder.cancel(generation: 1) }
        try await Task.sleep(nanoseconds: 20_000_000)
        gate.open()
        await cancel.value
        await #expect(throws: IncrementalStatic15Decoder.DecodeError.staleGeneration) {
            try await finish.value
        }
        #expect(await decoder.current().committedText.isEmpty)
    }

    @Test func finishPropagatesFinalDecodeFailure() async {
        let decoder = IncrementalStatic15Decoder()
        await decoder.reset(generation: 1)
        await #expect(throws: TestDecodeFailure.self) {
            try await decoder.finish(samples: speech(count: 1_000), generation: 1, decode: ScriptedDecode([.fail]).closure)
        }
    }
}
