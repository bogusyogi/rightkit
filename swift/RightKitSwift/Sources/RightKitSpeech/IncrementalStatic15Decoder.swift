import Foundation

/// Scheduled Static-15 incremental decode: 16 kHz audio is cut into disjoint ~15 s windows at the
/// quietest 200 ms span of each window's trailing 2.24 s, decoded one at a time in the background,
/// and assembled with the shared character-overlap seam. Ported unchanged from HeardRight
/// `IncrementalStatic15Decoder` (Mac's scheduled Static-15 contract for the iOS capture worker).
///
/// This type owns only scheduling and transcript assembly. The caller owns the
/// audio buffer & supplies ordinary production decode closures, so final text
/// remains on same path as a deferred stop. Each decode receives one disjoint
/// quiet-cut window. A failed background window disables further attempts but
/// leaves its audio uncommitted for final decode.
public actor IncrementalStatic15Decoder {
    public static let sampleRate = 16_000
    public static let windowSamples = 15 * sampleRate
    public static let paddingSamples = 2_240 * sampleRate / 1_000
    public static let silenceSamples = 200 * sampleRate / 1_000
    public static let silenceHopSamples = 100 * sampleRate / 1_000
    public static let maxOverlapCharacters = 16

    public struct Snapshot: Equatable, Sendable {
        /// Recording generation the scheduler currently accepts.
        public let generation: UInt64
        /// Seamed transcript of every committed background window.
        public let committedText: String
        /// Sample cursor: audio before this index is committed; final decode starts here.
        public let committedSamples: Int
        /// Background windows committed so far.
        public let backgroundWindows: Int
        /// A background window failed or returned empty text; no further background decodes run
        /// and its audio stays uncommitted for `finish`.
        public let backgroundDisabled: Bool
        /// Stop/reset/cancel is in progress; late background results are discarded.
        public let stopping: Bool

        public init(
            generation: UInt64, committedText: String, committedSamples: Int,
            backgroundWindows: Int, backgroundDisabled: Bool, stopping: Bool
        ) {
            self.generation = generation
            self.committedText = committedText
            self.committedSamples = committedSamples
            self.backgroundWindows = backgroundWindows
            self.backgroundDisabled = backgroundDisabled
            self.stopping = stopping
        }
    }

    public enum DecodeError: Error, Equatable, Sendable {
        case staleGeneration
    }

    private struct Checkpoint: Sendable {
        let start: Int
        let end: Int
        let text: String
    }

    private var generation: UInt64 = 0
    private var checkpoints: [Checkpoint] = []
    private var committedText = ""
    private var committedSamples = 0
    private var backgroundDisabled = false
    private var stopping = false
    private var inFlight: Task<String, Error>?
    private var inFlightToken: UInt64?
    private var nextToken: UInt64 = 0

    public typealias Decode = @Sendable ([Float]) async throws -> String

    public init() {}

    public func reset(generation: UInt64) async {
        let previous = inFlight
        previous?.cancel()
        inFlight = nil
        inFlightToken = nil
        self.generation = generation
        stopping = true
        checkpoints.removeAll(keepingCapacity: true)
        committedText = ""
        committedSamples = 0
        backgroundDisabled = false
        // Do not let a canceled Core ML call overlap next recording's first
        // window. Generation already changed, so its eventual result cannot
        // commit while this await drains the old task.
        _ = try? await previous?.value
        stopping = false
    }

    /// Decode at most one ready window. Repeated calls from a meter/checkpoint
    /// tick drain additional windows without ever launching concurrent Core ML
    /// predictions. Stop marks `stopping` while this method is suspended; a
    /// late result is then discarded by generation/state checks.
    @discardableResult
    public func consume(
        samples: [Float], generation requestedGeneration: UInt64, decode: @escaping Decode
    ) async -> Snapshot {
        guard requestedGeneration == generation, !stopping, !backgroundDisabled,
              inFlight == nil else { return snapshot() }
        guard let end = Self.readyEnd(samples: samples, start: committedSamples) else {
            return snapshot()
        }

        let start = committedSamples
        let segment = Array(samples[start..<end])
        nextToken &+= 1
        let token = nextToken
        let task = Task.detached(priority: .utility) { try await decode(segment) }
        inFlight = task
        inFlightToken = token

        let result: Result<String, Error>
        do {
            result = .success(try await task.value)
        } catch {
            result = .failure(error)
        }
        if inFlightToken == token {
            inFlight = nil
            inFlightToken = nil
        }

        guard requestedGeneration == generation, !stopping else {
            return snapshot()
        }
        switch result {
        case let .success(text) where !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty:
            checkpoints.append(Checkpoint(start: start, end: end, text: text))
            committedSamples = end
            rebuildText()
        case .success:
            backgroundDisabled = true
        case .failure:
            backgroundDisabled = true
        }
        return snapshot()
    }

    /// Finish current generation against exact sealed audio. A KWS cut may
    /// shorten audio below an already committed window; those checkpoints are
    /// rolled back before tail decode, so discarded command audio cannot return.
    /// Every uncommitted sample is decoded exactly once in a bounded, disjoint
    /// quiet-cut window so a missed background tick cannot exceed model shape.
    public func finish(
        samples: [Float], generation requestedGeneration: UInt64, decode: @escaping Decode
    ) async throws -> String {
        guard requestedGeneration == generation else { throw DecodeError.staleGeneration }
        stopping = true
        if let task = inFlight {
            _ = try? await task.value
            inFlight = nil
            inFlightToken = nil
        }
        guard requestedGeneration == generation else { throw DecodeError.staleGeneration }

        rollback(to: samples.count)
        while committedSamples < samples.count {
            let start = committedSamples
            let end: Int
            if samples.count - start <= Self.windowSamples {
                end = samples.count
            } else {
                end = Self.readyEnd(samples: samples, start: start)
                    ?? min(start + Self.windowSamples, samples.count)
            }
            guard end > start else { break }

            let tail = Array(samples[start..<end])
            let tailText = try await decode(tail)
            // Reset/cancel can re-enter this actor while a Core ML tail is
            // suspended. Never append its result into a newer generation.
            guard requestedGeneration == generation else {
                throw DecodeError.staleGeneration
            }
            if !tailText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                appendSeamed(tailText)
            }
            committedSamples = end
        }
        return committedText.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    public func cancel(generation requestedGeneration: UInt64) async {
        guard requestedGeneration == generation else { return }
        stopping = true
        let previous = inFlight
        previous?.cancel()
        inFlight = nil
        inFlightToken = nil
        // Invalidate a suspended final-tail decode as well as background work.
        // Next recording calls reset with its own monotonically increasing ID.
        generation &+= 1
        checkpoints.removeAll(keepingCapacity: true)
        committedText = ""
        committedSamples = 0
        backgroundDisabled = false
        _ = try? await previous?.value
    }

    public func current() -> Snapshot { snapshot() }

    private func snapshot() -> Snapshot {
        Snapshot(
            generation: generation,
            committedText: committedText,
            committedSamples: committedSamples,
            backgroundWindows: checkpoints.count,
            backgroundDisabled: backgroundDisabled,
            stopping: stopping
        )
    }

    private func rollback(to sampleCount: Int) {
        checkpoints.removeAll { $0.end > sampleCount }
        committedSamples = checkpoints.last?.end ?? 0
        rebuildText()
    }

    private func rebuildText() {
        committedText = ""
        for checkpoint in checkpoints { appendSeamed(checkpoint.text) }
    }

    private func appendSeamed(_ next: String) {
        let incoming = next.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !incoming.isEmpty else { return }
        guard !committedText.isEmpty else {
            committedText = incoming
            return
        }
        let maxCount = min(committedText.count, incoming.count, Self.maxOverlapCharacters)
        var overlap = 0
        if maxCount > 0 {
            for count in 1...maxCount {
                let prefixEnd = incoming.index(incoming.startIndex, offsetBy: count)
                let prefixLast = incoming[incoming.index(before: prefixEnd)]
                let prefixNext = prefixEnd < incoming.endIndex ? incoming[prefixEnd] : nil
                let endsInsideWord = (prefixLast.isLetter || prefixLast.isNumber)
                    && (prefixNext?.isLetter == true || prefixNext?.isNumber == true)
                // Match Rust's canonical seam guard: a one/two-character
                // match inside an unrelated word is not evidence of overlap.
                guard !(endsInsideWord && count < 3) else { continue }
                if String(committedText.suffix(count)).caseInsensitiveCompare(
                    String(incoming.prefix(count))
                ) == .orderedSame { overlap = count }
            }
        }
        if overlap > 0 {
            let start = incoming.index(incoming.startIndex, offsetBy: overlap)
            committedText += incoming[start...]
        } else {
            if committedText.last?.isWhitespace == false, incoming.first?.isWhitespace == false {
                committedText += " "
            }
            committedText += incoming
        }
    }

    public static func readyEnd(samples: [Float], start: Int) -> Int? {
        guard start >= 0, start + windowSamples < samples.count else { return nil }
        let windowEnd = start + windowSamples
        let targetEnd = start + windowSamples - paddingSamples
        return quietCut(in: samples, lo: targetEnd, hi: windowEnd) ?? windowEnd
    }

    public static func quietCut(in samples: [Float], lo: Int, hi: Int) -> Int? {
        guard hi > lo, hi - lo >= silenceSamples else { return nil }
        var bestCut: Int?
        var bestRMS = Float.infinity
        var index = lo
        while index + silenceSamples <= hi {
            var sum: Float = 0
            for sample in samples[index..<(index + silenceSamples)] { sum += sample * sample }
            let rms = (sum / Float(silenceSamples)).squareRoot()
            if rms < bestRMS {
                bestRMS = rms
                bestCut = index + silenceSamples / 2
            }
            index += silenceHopSamples
        }
        return bestCut
    }
}

/// Realtime-safe threshold gate. It keeps AVAudioEngine's tap callback cheap
/// while allowing one scheduled request per newly available Static-15 window.
public final class IncrementalStatic15Trigger: @unchecked Sendable {
    private let lock = NSLock()
    private var observedSamples = 0
    private var nextSample = IncrementalStatic15Decoder.windowSamples + 1

    public init() {}

    public func reset() {
        lock.lock(); defer { lock.unlock() }
        observedSamples = 0
        nextSample = IncrementalStatic15Decoder.windowSamples + 1
    }

    /// Audio taps provide frame deltas; keep cumulative count here so the tap
    /// closure need not capture `WarmAudioCapture` across its Sendable boundary.
    public func shouldRequest(appendedSamples: Int) -> Bool {
        lock.lock(); defer { lock.unlock() }
        observedSamples += max(0, appendedSamples)
        return shouldRequestLocked(sampleCount: observedSamples)
    }

    public func shouldRequest(sampleCount: Int) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return shouldRequestLocked(sampleCount: sampleCount)
    }

    private func shouldRequestLocked(sampleCount: Int) -> Bool {
        guard sampleCount >= nextSample else { return false }
        nextSample = sampleCount + IncrementalStatic15Decoder.windowSamples
        return true
    }
}
