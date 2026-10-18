import Foundation
@preconcurrency import AVFoundation
import Observation
import RightKitAudio

/// Reads text aloud: streams chunks from a ``SynthesisPipeline``, plays each as soon as it is rendered, and
/// lets the user pause, resume, skip by chunk and change the speed, while handling the audio-session
/// interruptions and route changes that would otherwise leave playback stuck.
///
/// Synthesis and playback stay on the device; no provider or network path is involved.
///
/// - **Interruption**: a call or Siri pauses playback; it resumes by itself only if the system recommends
///   it and the user had not paused it.
/// - **Route change**: unplugging headphones pauses (and does not auto-resume), the usual convention.
/// - **Media services reset / engine reconfiguration**: the audio graph is rebuilt and playback continues
///   from the chunk in progress, paused if it was.
/// - **Seeking** is by chunk, among the chunks rendered so far.
@MainActor
@Observable
public final class PlaybackController {
    public enum State: Sendable, Equatable {
        case idle
        /// Rendering the first chunk; nothing is audible yet.
        case synthesizing
        case playing
        case paused
    }

    // MARK: Observable state

    public private(set) var state: State = .idle
    public private(set) var errorMessage: String?
    /// Zero-based index of the chunk being played (or about to be, while paused or between chunks).
    public private(set) var currentChunkIndex = 0
    /// Chunks rendered so far; the request's full count once ``isSynthesisComplete``.
    public private(set) var chunkCount = 0
    public private(set) var isSynthesisComplete = false
    /// The text of the chunk in progress, for highlighting what is being read.
    public private(set) var currentChunkText: String?

    public var isReading: Bool { state != .idle }
    public var isPlaying: Bool { state == .playing }
    public var isPaused: Bool { state == .paused }
    public var isSynthesizing: Bool { state == .synthesizing }

    /// Playback speed, 0.5...2.0, pitch preserved. Change it with ``setRate(_:)``.
    public private(set) var rate: Float = 1.0

    public static let rateRange: ClosedRange<Float> = 0.5 ... 2.0

    // MARK: Collaborators (replaced in tests)

    @ObservationIgnored private let chunkSource: @Sendable (String) -> AsyncThrowingStream<AudioChunk, Error>
    @ObservationIgnored private let makeEngine: @MainActor () -> any PlaybackEngine
    @ObservationIgnored private let session: AudioSessionConfiguration?
    @ObservationIgnored private let activateSession: @Sendable (AudioSessionConfiguration) async throws -> Void
    @ObservationIgnored private let releaseSession: @Sendable (Bool) async -> Void
    @ObservationIgnored private let makeMonitor: @Sendable (@escaping @Sendable (AudioSessionEvent) -> Void) -> AudioSessionMonitor

    // MARK: Internal state

    @ObservationIgnored private var engine: (any PlaybackEngine)?
    @ObservationIgnored private var monitor: AudioSessionMonitor?
    @ObservationIgnored private var synthesisTask: Task<Void, Never>?
    @ObservationIgnored private var chunks: [AudioChunk] = []
    @ObservationIgnored private var buffers: [AVAudioPCMBuffer] = []
    /// Chunks handed to the engine since the last queue reset.
    @ObservationIgnored private var scheduledCount = 0
    /// Chunks that have played out completely (counted from the start of the request).
    @ObservationIgnored private var finishedCount = 0
    /// Identifies one `read`/`stop` cycle; the synthesis task and session work for an older value stop.
    @ObservationIgnored private var requestID = 0
    /// Identifies one engine queue; bumped by every read, stop, skip and rebuild so a completion from a
    /// dropped queue is ignored.
    @ObservationIgnored private var queueGeneration = 0
    @ObservationIgnored private var pausedBySystem = false
    @ObservationIgnored private var format: AVAudioFormat?

    public convenience init(pipeline: SynthesisPipeline = SynthesisPipeline(), session: AudioSessionConfiguration? = .spokenPlayback) {
        self.init(
            chunkSource: { pipeline.chunks(for: $0) },
            makeEngine: { AVAudioPlaybackEngine() },
            session: session,
            activateSession: { try await AudioSessionController.activate($0) },
            releaseSession: { await AudioSessionController.deactivate(notifyOthers: $0) },
            makeMonitor: { AudioSessionMonitor(handler: $0) })
    }

    init(
        chunkSource: @escaping @Sendable (String) -> AsyncThrowingStream<AudioChunk, Error>,
        makeEngine: @escaping @MainActor () -> any PlaybackEngine,
        session: AudioSessionConfiguration?,
        activateSession: @escaping @Sendable (AudioSessionConfiguration) async throws -> Void,
        releaseSession: @escaping @Sendable (Bool) async -> Void,
        makeMonitor: @escaping @Sendable (@escaping @Sendable (AudioSessionEvent) -> Void) -> AudioSessionMonitor
    ) {
        self.chunkSource = chunkSource
        self.makeEngine = makeEngine
        self.session = session
        self.activateSession = activateSession
        self.releaseSession = releaseSession
        self.makeMonitor = makeMonitor
    }

    // MARK: Transport

    /// Starts reading `text`, replacing anything already being read.
    public func read(text: String) {
        reset(releasingSession: false)  // the new read re-activates the session; releasing it here would race that
        let myRequest = requestID
        state = .synthesizing
        errorMessage = nil
        let stream = chunkSource(text)
        synthesisTask = Task { [weak self] in
            guard let self else { return }
            do {
                if let session = self.session { try await self.activateSession(session) }
                guard self.requestID == myRequest else { return }
                self.startMonitor()
                for try await chunk in stream {
                    guard self.requestID == myRequest else { return }
                    try self.accept(chunk)
                }
                guard self.requestID == myRequest else { return }
                self.isSynthesisComplete = true
                self.finishIfDone()
            } catch is CancellationError {
                guard self.requestID == myRequest else { return }
                self.endPlayback(error: nil)
            } catch {
                guard self.requestID == myRequest else { return }
                self.endPlayback(error: error.localizedDescription)
            }
        }
    }

    /// Pauses. The engine and audio session stay up, so ``resume()`` is instant.
    public func pause() {
        guard state == .playing else { return }
        engine?.pause()
        pausedBySystem = false
        state = .paused
    }

    /// Resumes after ``pause()`` or a system pause.
    public func resume() {
        guard state == .paused else { return }
        pausedBySystem = false
        Task { await self.resumeAfterReactivating() }
    }

    /// Stops reading and releases the audio session. Safe to call when idle.
    public func stop() {
        reset(releasingSession: true)
    }

    /// Changes the playback speed, clamped to ``rateRange``. Applies immediately, pitch preserved.
    public func setRate(_ value: Float) {
        rate = min(max(value, Self.rateRange.lowerBound), Self.rateRange.upperBound)
        engine?.setRate(rate)
    }

    /// Jumps to a chunk that has already been rendered (clamped to the available ones).
    public func skip(to index: Int) {
        guard state == .playing || state == .paused, !buffers.isEmpty else { return }
        let target = min(max(index, 0), buffers.count - 1)
        queueGeneration += 1
        engine?.resetQueue()
        scheduledCount = target
        finishedCount = target
        currentChunkIndex = target
        currentChunkText = chunks[target].text
        scheduleRemaining()
        if state == .playing { engine?.play() }
    }

    /// Jumps to the next rendered chunk; does nothing at the last one rendered so far.
    public func skipForward() {
        guard currentChunkIndex + 1 < buffers.count else { return }
        skip(to: currentChunkIndex + 1)
    }

    /// Goes back one chunk (the first chunk restarts).
    public func skipBackward() {
        skip(to: max(currentChunkIndex - 1, 0))
    }

    public func dismissError() {
        errorMessage = nil
    }

    // MARK: Chunk flow

    private func accept(_ chunk: AudioChunk) throws {
        guard let buffer = chunk.makeBuffer() else {
            throw SynthesisError.synthesisFailed(segment: chunk.index, reason: "the device could not allocate a playback buffer")
        }
        chunks.append(chunk)
        buffers.append(buffer)
        chunkCount = chunks.count
        guard engine == nil else {
            scheduleRemaining()
            return
        }
        let created = makeEngine()
        created.onConfigurationChange = { [weak self] in
            Task { @MainActor [weak self] in await self?.rebuildEngine() }
        }
        do {
            try created.start(format: buffer.format, rate: rate)
        } catch {
            throw SynthesisError.synthesisFailed(segment: chunk.index, reason: "the audio engine could not start: \(error.localizedDescription)")
        }
        engine = created
        format = buffer.format
        currentChunkIndex = 0
        currentChunkText = chunk.text
        scheduleRemaining()
        created.play()
        state = .playing
    }

    /// Hands every rendered-but-unscheduled chunk to the engine.
    private func scheduleRemaining() {
        guard let engine else { return }
        let myQueue = queueGeneration
        while scheduledCount < buffers.count {
            let index = scheduledCount
            scheduledCount += 1
            engine.schedule(buffers[index]) { [weak self] in
                Task { @MainActor [weak self] in self?.chunkFinished(index, queue: myQueue) }
            }
        }
    }

    private func chunkFinished(_ index: Int, queue: Int) {
        guard queueGeneration == queue, state == .playing || state == .paused else { return }
        finishedCount = max(finishedCount, index + 1)
        currentChunkIndex = min(finishedCount, max(chunks.count - 1, 0))
        if currentChunkIndex < chunks.count { currentChunkText = chunks[currentChunkIndex].text }
        finishIfDone()
    }

    /// Ends playback once every chunk has played and no more are coming. While synthesis still runs, the
    /// engine just idles until the next chunk arrives.
    private func finishIfDone() {
        guard isSynthesisComplete, finishedCount >= chunks.count else { return }
        reset(releasingSession: true)
    }

    // MARK: Session events

    private func startMonitor() {
        monitor?.stop()
        let created = makeMonitor { [weak self] event in
            Task { @MainActor [weak self] in await self?.handleSessionEvent(event) }
        }
        monitor = created
        created.start()
    }

    /// Reacts to an audio-session event. Internal so tests can drive it without a device.
    func handleSessionEvent(_ event: AudioSessionEvent) async {
        switch event {
        case .interruptionBegan:
            guard state == .playing else { return }
            engine?.pause()
            pausedBySystem = true
            state = .paused
        case let .interruptionEnded(shouldResume):
            guard pausedBySystem else { return }
            pausedBySystem = false
            if shouldResume { await resumeAfterReactivating() }
        case let .routeChanged(reason):
            // Headphones unplugged: pause rather than play on the speaker. Never resumes by itself.
            if reason == .oldDeviceUnavailable, state == .playing {
                engine?.pause()
                pausedBySystem = false
                state = .paused
            }
        case .mediaServicesLost, .mediaServicesReset:
            // Everything audio is invalid: reconfigure the session, then rebuild the graph.
            if let session { try? await activateSession(session) }
            await rebuildEngine()
        }
    }

    private func resumeAfterReactivating() async {
        guard state == .paused else { return }
        let myRequest = requestID
        if let session {
            do { try await activateSession(session) } catch {
                guard requestID == myRequest else { return }
                endPlayback(error: error.localizedDescription)
                return
            }
        }
        guard requestID == myRequest, state == .paused else { return }
        engine?.play()
        state = .playing
    }

    /// Rebuilds the audio graph and continues from the chunk in progress, paused if it was paused.
    private func rebuildEngine() async {
        guard let engine, let format, state == .playing || state == .paused, !buffers.isEmpty else { return }
        let wasPlaying = state == .playing
        queueGeneration += 1
        do {
            try engine.start(format: format, rate: rate)
        } catch {
            endPlayback(error: "Audio playback stopped: \(error.localizedDescription)")
            return
        }
        scheduledCount = currentChunkIndex
        finishedCount = currentChunkIndex
        scheduleRemaining()
        if wasPlaying { engine.play() }
    }

    // MARK: Teardown

    private func endPlayback(error message: String?) {
        reset(releasingSession: true)
        errorMessage = message
    }

    private func reset(releasingSession: Bool) {
        requestID += 1
        queueGeneration += 1
        synthesisTask?.cancel()
        synthesisTask = nil
        monitor?.stop()
        monitor = nil
        engine?.onConfigurationChange = nil
        engine?.stop()
        engine = nil
        format = nil
        chunks = []
        buffers = []
        scheduledCount = 0
        finishedCount = 0
        currentChunkIndex = 0
        chunkCount = 0
        currentChunkText = nil
        isSynthesisComplete = false
        pausedBySystem = false
        state = .idle
        errorMessage = nil
        if releasingSession, let session {
            let release = releaseSession
            let notify = session.notifyOthersOnDeactivation
            Task { await release(notify) }
        }
    }
}
