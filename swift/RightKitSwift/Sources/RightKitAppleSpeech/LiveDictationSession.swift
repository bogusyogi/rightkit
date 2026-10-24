#if canImport(Speech) // Speech is unavailable on watchOS; RightKitAudio carries the watch recorder
import AVFoundation
import Foundation
import RightKitAudio
import RightKitSpeech
import Speech

/// Microphone dictation on Apple's on-device recognizers: `SpeechAnalyzer` on iOS/macOS 26+,
/// `SFSpeechAudioBufferRecognitionRequest` before that (CodeRight `SpeechDictation`, minus its
/// SwiftUI observation and app signposts). Permissions are injected so the app owns its prompts.
///
/// Recognition is on-device only unless the app opts in with `onDeviceOnly: false` (see ``init(localeCandidates:onDeviceOnly:resumesAfterInterruption:speechAuthorization:microphonePermission:)``).
///
/// Every start ends in exactly one ``Event/stopped``, whatever ended it (``stop()``, a final result on the
/// legacy recognizer, a failure, or an audio-session interruption). A start that is replaced by a newer
/// ``start(onEvent:)`` ends silently: its owner asked for the replacement.
///
/// Cancellation error codes from `kLSRErrorDomain` (209, 216) and `kAFAssistantErrorDomain` (216)
/// are the recognizer reporting a normal stop, not a failure.
public final class LiveDictationSession: @unchecked Sendable {
    public enum Event: Sendable, Equatable {
        case status(String?)
        case partial(String)
        /// One recognized segment is final. The `SpeechAnalyzer` path emits one per pause-delimited segment
        /// and keeps listening; the legacy recognizer emits one and then ends. Do not treat it as the end
        /// of the session: wait for ``stopped``. The segments so far are in ``LiveDictationSession/finalizedText``.
        case final(String)
        case failed(String)
        /// The system took the microphone (a call, Siri, an unplugged headset, a media-services restart).
        /// The session ends right after, with ``stopped``.
        case interrupted(InterruptionReason)
        /// The session ended; the composer should leave its listening state. Emitted once per start.
        case stopped
    }

    public enum InterruptionReason: Sendable, Equatable {
        /// An `AVAudioSession` interruption began (phone call, Siri, alarm, another app).
        case began
        /// The input the session was recording from went away.
        case inputRouteLost
        /// `mediaserverd` was lost or reset; the audio graph is invalid.
        case mediaServicesReset
    }

    public typealias SpeechAuthorizationProvider = @Sendable () async -> Bool
    public typealias MicrophonePermissionProvider = @Sendable () async -> Bool

    private struct PendingResume {
        let sink: @Sendable (Event) -> Void
    }

    private let lock = NSLock()
    private let speechAuthorization: SpeechAuthorizationProvider
    private let microphonePermission: MicrophonePermissionProvider
    private let localeCandidates: [Locale]
    private let legacyFactory: LegacyCaptureFactory
    private let modernEnabled: Bool
    private let onDeviceOnly: Bool
    private let resumesAfterInterruption: Bool
    /// Bumped by every `start()` and `stop()`. A start owns exactly the resources it publishes while
    /// its generation is still current; anything it built after that point is torn down by the
    /// start itself, never left for (or taken from) a later owner.
    private var generation = 0
    private var legacy: (any LegacyCaptureHandle)?
    private var modern: AnyObject?
    private var startTask: Task<Void, Never>?
    /// The current start's event gate; nil once it ended or was replaced.
    private var gate: EventGate?
    private var sink: (@Sendable (Event) -> Void)?
    private var monitor: AudioSessionMonitor?
    private var pendingResume: PendingResume?
    private var finalizedSegments: [String] = []
    /// Test seam: the most recent start's task, kept after `stop()` so tests can await it.
    private(set) var lastStartTask: Task<Void, Never>?

    /// - Parameters:
    ///   - onDeviceOnly: `true` (the default) fails closed: a locale with no on-device model is never sent to
    ///     Apple's servers. `false` lets the pre-iOS 26 recognizer fall back to network recognition for such a
    ///     locale, and the session emits a ``Event/status(_:)`` saying so. It is a product decision, never a default.
    ///   - resumesAfterInterruption: when `true`, a session ended by an interruption starts again, with the
    ///     same event handler, when the system says the interruption is over and recommends resuming.
    ///     Defaults to `false`: dictation that restarts itself after a phone call surprises people.
    public convenience init(
        localeCandidates: [Locale] = Dictation.localeCandidates(),
        onDeviceOnly: Bool = true,
        resumesAfterInterruption: Bool = false,
        speechAuthorization: @escaping SpeechAuthorizationProvider = { await LiveDictationSession.requestSpeechAuthorization() },
        microphonePermission: @escaping MicrophonePermissionProvider = { await LiveDictationSession.requestMicrophonePermission() }
    ) {
        self.init(
            localeCandidates: localeCandidates, speechAuthorization: speechAuthorization,
            microphonePermission: microphonePermission, legacyFactory: LegacyCapture.start, modernEnabled: true,
            onDeviceOnly: onDeviceOnly, resumesAfterInterruption: resumesAfterInterruption)
    }

    /// Test seam: substitutes the legacy capture factory and can disable the iOS/macOS 26 path.
    init(
        localeCandidates: [Locale],
        speechAuthorization: @escaping SpeechAuthorizationProvider,
        microphonePermission: @escaping MicrophonePermissionProvider,
        legacyFactory: @escaping LegacyCaptureFactory,
        modernEnabled: Bool,
        onDeviceOnly: Bool = true,
        resumesAfterInterruption: Bool = false
    ) {
        self.localeCandidates = localeCandidates
        self.speechAuthorization = speechAuthorization
        self.microphonePermission = microphonePermission
        self.legacyFactory = legacyFactory
        self.modernEnabled = modernEnabled
        self.onDeviceOnly = onDeviceOnly
        self.resumesAfterInterruption = resumesAfterInterruption
    }

    public static func requestSpeechAuthorization() async -> Bool {
        await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0 == .authorized) }
        }
    }

    public static func requestMicrophonePermission() async -> Bool {
        #if os(iOS)
        await AVAudioApplication.requestRecordPermission()
        #else
        await AVCaptureDevice.requestAccess(for: .audio)
        #endif
    }

    /// The text of every segment finalized since the last ``start(onEvent:)``, joined by spaces. Read it
    /// when ``Event/stopped`` arrives to get the whole dictation, however many pauses it spanned.
    public var finalizedText: String {
        lock.withLock { finalizedSegments.joined(separator: " ") }
    }

    /// Starts capture; events arrive on an arbitrary queue until ``Event/stopped``.
    public func start(onEvent: @escaping @Sendable (Event) -> Void) {
        endCurrentRun(announce: false)
        let newGate = EventGate { [weak self] event in
            if case let .final(text) = event { self?.recordFinal(text) }
            onEvent(event)
        }
        let current: Int = lock.withLock {
            generation += 1
            gate = newGate
            sink = onEvent
            pendingResume = nil
            finalizedSegments = []
            return generation
        }
        newGate.emit(.status("Preparing dictation…"))
        let task = Task<Void, Never> { [weak self] in
            guard let self else { return }
            await self.run(generation: current, gate: newGate)
        }
        lock.withLock { startTask = task; lastStartTask = task }
    }

    /// Ends the session. Emits ``Event/stopped`` if a session was running.
    public func stop() {
        endCurrentRun(announce: true)
    }

    private func isCurrent(_ value: Int) -> Bool { lock.withLock { generation == value } }

    private func recordFinal(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        lock.withLock { finalizedSegments.append(trimmed) }
    }

    /// Tears the current run down. `announce` emits `.stopped` to its owner; a replacement does not.
    private func endCurrentRun(announce: Bool) {
        let (task, ended) = lock.withLock { () -> (Task<Void, Never>?, EventGate?) in
            generation += 1
            defer { startTask = nil; gate = nil; pendingResume = nil }
            return (startTask, gate)
        }
        task?.cancel()
        teardown()
        if announce { ended?.end() } else { ended?.close() }
    }

    /// Ends a run from inside (a failure, a final result, an interruption): the same as `stop()` but only
    /// if `value` is still the current generation, and without cancelling the start task it may be running on.
    private func finishRun(generation value: Int) {
        let ended = lock.withLock { () -> EventGate? in
            guard generation == value else { return nil }
            generation += 1
            defer { startTask = nil; gate = nil }
            return gate
        }
        guard let ended else { return }
        teardown()
        ended.end()
    }

    private func fail(_ message: String, generation value: Int, gate: EventGate) {
        gate.emit(.failed(message))
        finishRun(generation: value)
    }

    private func run(generation value: Int, gate: EventGate) async {
        // Permission prompts can outlive the start that asked for them. Recheck after each await so a
        // canceled or replaced start never reports a stale failure to the current owner.
        let speechGranted = await speechAuthorization()
        guard isCurrent(value), !Task.isCancelled else { return }
        guard speechGranted else {
            fail("Dictation needs Speech Recognition access.", generation: value, gate: gate)
            return
        }
        let microphoneGranted = await microphonePermission()
        guard isCurrent(value), !Task.isCancelled else { return }
        guard microphoneGranted else {
            fail("Dictation needs Microphone access.", generation: value, gate: gate)
            return
        }
        do {
            #if os(iOS)
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
            #endif
            installMonitor()
            if modernEnabled, #available(iOS 26.0, macOS 26.0, *), let locale = await ModernCapture.supportedLocale(from: localeCandidates) {
                do {
                    let capture = try await ModernCapture.start(locale: locale, onEvent: { [weak self] event in
                        guard let self, self.isCurrent(value) else { return }
                        if case .stopped = event {  // the recognizer's result stream ended on its own
                            self.finishRun(generation: value)
                            return
                        }
                        gate.emit(event)
                        if case .failed = event { self.finishRun(generation: value) }
                    })
                    // Publish only if this start still owns the session; otherwise unwind it here.
                    guard !Task.isCancelled, adopt(generation: value, modern: capture) else {
                        capture.stop()
                        releaseAudioSessionIfIdle()
                        return
                    }
                    gate.emit(.status(nil))
                    return
                } catch {
                    // Fall through to the legacy recognizer.
                }
            }
            guard isCurrent(value), !Task.isCancelled else { releaseAudioSessionIfIdle(); return }
            guard try beginLegacy(generation: value, gate: gate) else {
                releaseAudioSessionIfIdle()
                return
            }
            gate.emit(.status(nil))
        } catch {
            // A superseded start owns nothing and must neither tear down its successor's capture
            // nor report a failure the current owner never asked for.
            guard isCurrent(value) else { releaseAudioSessionIfIdle(); return }
            fail("Couldn't start the microphone.", generation: value, gate: gate)
        }
    }

    /// Builds the legacy capture and publishes it only while `value` is still the current
    /// generation. Returns `false` (after disposing everything it built) when a `stop()` or a newer
    /// `start()` arrived during setup.
    private func beginLegacy(generation value: Int, gate: EventGate) throws -> Bool {
        let context = LegacyCaptureContext(
            localeCandidates: localeCandidates,
            onDeviceOnly: onDeviceOnly,
            isCurrent: { [weak self] in self?.isCurrent(value) ?? false },
            emit: { gate.emit($0) },
            stop: { [weak self] in self?.finishRun(generation: value) }
        )
        let capture = try legacyFactory(context)
        let adopted = lock.withLock { () -> Bool in
            guard generation == value else { return false }
            legacy = capture
            return true
        }
        if !adopted { capture.tearDown() }  // idempotent; the stale start owns this capture alone
        return adopted
    }

    private func adopt(generation value: Int, modern capture: AnyObject) -> Bool {
        lock.withLock { () -> Bool in
            guard generation == value else { return false }
            modern = capture
            return true
        }
    }

    /// Deactivates the shared iOS audio session after a superseded start activated it, unless a
    /// newer start (or a published capture) now owns it.
    private func releaseAudioSessionIfIdle() {
        #if os(iOS)
        let idle = lock.withLock { startTask == nil && legacy == nil && modern == nil }
        if idle { try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation) }
        #endif
    }

    private func teardown() {
        let (legacy, modern, observer) = lock.withLock { () -> ((any LegacyCaptureHandle)?, AnyObject?, AudioSessionMonitor?) in
            defer { self.legacy = nil; self.modern = nil; self.monitor = nil }
            return (self.legacy, self.modern, self.monitor)
        }
        observer?.stop()
        if #available(iOS 26.0, macOS 26.0, *) { (modern as? ModernCapture)?.stop() }
        legacy?.tearDown()
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
    }

    // MARK: Audio-session events

    private func installMonitor() {
        let created = AudioSessionMonitor { [weak self] event in self?.handleSessionEvent(event) }
        let installed = lock.withLock { () -> Bool in
            guard monitor == nil else { return false }
            monitor = created
            return true
        }
        if installed { created.start() }
    }

    /// Reacts to an audio-session event. Internal so tests can drive it without a device.
    func handleSessionEvent(_ event: AudioSessionEvent) {
        let pending = lock.withLock { pendingResume }
        if let pending {
            // The session already ended for an interruption; only the end of that interruption matters now.
            guard case let .interruptionEnded(shouldResume) = event else { return }
            let observer = lock.withLock { () -> AudioSessionMonitor? in
                defer { pendingResume = nil; monitor = nil }
                return monitor
            }
            observer?.stop()
            if shouldResume { start(onEvent: pending.sink) }
            return
        }
        guard lock.withLock({ gate != nil }) else { return }
        switch event {
        case .interruptionBegan: interrupt(.began)
        case .interruptionEnded: break
        case let .routeChanged(reason): if reason.losesActiveInput { interrupt(.inputRouteLost) }
        case .mediaServicesLost, .mediaServicesReset: interrupt(.mediaServicesReset)
        }
    }

    private func interrupt(_ reason: InterruptionReason) {
        let (current, owner, value) = lock.withLock { (gate, sink, generation) }
        guard let current else { return }
        current.emit(.interrupted(reason))
        finishRun(generation: value)
        // Keep listening for the end of the interruption so the owner's dictation can pick up again.
        if reason == .began, resumesAfterInterruption, let owner {
            lock.withLock { pendingResume = PendingResume(sink: owner) }
            installMonitor()
        }
    }
}

/// Delivers a start's events to its owner until the run ends, then drops everything: after `.stopped`
/// (or after the run was replaced) no late callback can reach the owner.
final class EventGate: @unchecked Sendable {
    private let lock = NSLock()
    private let sink: @Sendable (LiveDictationSession.Event) -> Void
    private var closed = false

    init(sink: @escaping @Sendable (LiveDictationSession.Event) -> Void) {
        self.sink = sink
    }

    func emit(_ event: LiveDictationSession.Event) {
        guard lock.withLock({ !closed }) else { return }
        sink(event)
    }

    /// Emits `.stopped` once, then closes.
    func end() {
        let first = lock.withLock { () -> Bool in
            defer { closed = true }
            return !closed
        }
        if first { sink(.stopped) }
    }

    /// Closes without announcing (the run was replaced).
    func close() {
        lock.withLock { closed = true }
    }
}

// MARK: - Legacy capture

/// Everything one legacy start installed: audio engine, input tap, recognition request and task.
/// `tearDown()` releases all of it and is idempotent, so the owner and a stale start can both call it.
protocol LegacyCaptureHandle: AnyObject, Sendable {
    func tearDown()
}

struct LegacyCaptureContext: Sendable {
    let localeCandidates: [Locale]
    /// `true` fails closed: never use a recognizer that would send audio to Apple's servers.
    let onDeviceOnly: Bool
    /// `false` once the owning start was stopped or replaced; callbacks must drop their output.
    let isCurrent: @Sendable () -> Bool
    let emit: @Sendable (LiveDictationSession.Event) -> Void
    /// Ends the session (a final result or a recognizer error).
    let stop: @Sendable () -> Void
}

typealias LegacyCaptureFactory = @Sendable (LegacyCaptureContext) throws -> any LegacyCaptureHandle

private final class LegacyCapture: LegacyCaptureHandle, @unchecked Sendable {
    private static let cancellationCodes: [String: Set<Int>] = [
        "kLSRErrorDomain": [209, 216],
        "kAFAssistantErrorDomain": [216],
    ]

    private let lock = NSLock()
    private var engine: AVAudioEngine?
    private var tapInstalled = false
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?

    private init(engine: AVAudioEngine, request: SFSpeechAudioBufferRecognitionRequest) {
        self.engine = engine
        self.request = request
    }

    /// Installs tap, engine and recognition task one step at a time; any failure unwinds every step
    /// already taken before it propagates.
    static func start(_ context: LegacyCaptureContext) throws -> any LegacyCaptureHandle {
        // Local-only unless the app opted in: pick a recognizer that recognizes on-device, otherwise
        // fail closed (or, with `onDeviceOnly == false`, fall back to a network-capable one).
        let allowsNetwork = !context.onDeviceOnly
        let recognizer = try LegacySpeechPolicy.select(
            from: context.localeCandidates.compactMap { SFSpeechRecognizer(locale: $0) },
            allowsNetwork: allowsNetwork,
            isAvailable: { $0.isAvailable },
            supportsOnDevice: { $0.supportsOnDeviceRecognition }
        )
        let engine = AVAudioEngine()
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        request.taskHint = .dictation
        try LegacySpeechPolicy.configure(
            request, supportsOnDevice: recognizer.supportsOnDeviceRecognition, allowsNetwork: allowsNetwork)
        if !recognizer.supportsOnDeviceRecognition {
            context.emit(.status("Using Apple's servers to recognize speech in this language."))
        }
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.channelCount > 0 else { throw SpeechError.engineUnavailable("no audio input") }
        let capture = LegacyCapture(engine: engine, request: request)
        do {
            input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in request.append(buffer) }
            capture.lock.withLock { capture.tapInstalled = true }
            engine.prepare()
            try engine.start()
            let recognition = recognizer.recognitionTask(with: request) { result, error in
                guard context.isCurrent() else { return }
                if let result {
                    let text = result.bestTranscription.formattedString
                    if result.isFinal { context.emit(.final(text)); context.stop(); return }
                    context.emit(.partial(text))
                }
                if let error {
                    let value = error as NSError
                    if cancellationCodes[value.domain]?.contains(value.code) == true { context.stop(); return }
                    context.emit(.failed("Couldn't transcribe that."))
                    context.stop()
                }
            }
            capture.lock.withLock { capture.task = recognition }
        } catch {
            capture.tearDown()
            throw error
        }
        return capture
    }

    func tearDown() {
        let (engine, installed, request, task) = lock.withLock { () -> (AVAudioEngine?, Bool, SFSpeechAudioBufferRecognitionRequest?, SFSpeechRecognitionTask?) in
            defer { self.engine = nil; self.tapInstalled = false; self.request = nil; self.task = nil }
            return (self.engine, self.tapInstalled, self.request, self.task)
        }
        if let engine {
            if installed { engine.inputNode.removeTap(onBus: 0) }
            if engine.isRunning { engine.stop() }
        }
        task?.cancel()
        request?.endAudio()
    }
}

@available(iOS 26.0, macOS 26.0, *)
private final class ModernCapture: @unchecked Sendable {
    private let analyzer: SpeechAnalyzer
    private let audioEngine: AVAudioEngine
    private let inputContinuation: AsyncStream<AnalyzerInput>.Continuation
    private var analysisTask: Task<Void, Never>?
    private var resultTask: Task<Void, Never>?
    private var tapInstalled = false

    private init(analyzer: SpeechAnalyzer, audioEngine: AVAudioEngine, inputContinuation: AsyncStream<AnalyzerInput>.Continuation) {
        self.analyzer = analyzer
        self.audioEngine = audioEngine
        self.inputContinuation = inputContinuation
    }

    static func supportedLocale(from candidates: [Locale]) async -> Locale? {
        guard SpeechTranscriber.isAvailable else { return nil }
        for candidate in candidates {
            if let supported = await SpeechTranscriber.supportedLocale(equivalentTo: candidate) { return supported }
        }
        return nil
    }

    static func start(locale: Locale, onEvent: @escaping @Sendable (LiveDictationSession.Event) -> Void) async throws -> ModernCapture {
        let transcriber = SpeechTranscriber(locale: locale, preset: .progressiveTranscription)
        let modules: [any SpeechModule] = [transcriber]
        let assetStatus = await AssetInventory.status(forModules: modules)
        guard assetStatus != .unsupported else { throw SpeechError.engineUnavailable("speech assets unsupported") }
        if assetStatus != .installed, let installation = try await AssetInventory.assetInstallationRequest(supporting: modules) {
            onEvent(.status("Downloading speech support…"))
            try await installation.downloadAndInstall()
        }
        onEvent(.status("Starting microphone…"))
        let engine = AVAudioEngine()
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.channelCount > 0 else { throw SpeechError.engineUnavailable("no audio input") }
        let analyzer = SpeechAnalyzer(modules: modules)
        try await analyzer.prepareToAnalyze(in: format)
        let (stream, continuation) = AsyncStream.makeStream(of: AnalyzerInput.self)
        let capture = ModernCapture(analyzer: analyzer, audioEngine: engine, inputContinuation: continuation)
        capture.resultTask = Task {
            do {
                for try await result in transcriber.results {
                    let text = String(result.text.characters)
                    onEvent(result.isFinal ? .final(text) : .partial(text))
                }
                // The result stream finished without stop(): the analyzer is done, so the session is over.
                if !Task.isCancelled { onEvent(.stopped) }
            } catch {
                onEvent(.failed("Couldn't transcribe that."))
            }
        }
        capture.analysisTask = Task {
            do { try await analyzer.start(inputSequence: stream) } catch { onEvent(.failed("Couldn't transcribe that.")) }
        }
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
            guard let copy = Self.copyBuffer(buffer) else { return }
            continuation.yield(AnalyzerInput(buffer: copy))
        }
        capture.tapInstalled = true
        engine.prepare()
        do { try engine.start() } catch { capture.stop(); throw error }
        return capture
    }

    func stop() {
        if tapInstalled { audioEngine.inputNode.removeTap(onBus: 0); tapInstalled = false }
        if audioEngine.isRunning { audioEngine.stop() }
        inputContinuation.finish()
        analysisTask?.cancel()
        resultTask?.cancel()
        analysisTask = nil
        resultTask = nil
        Task { [analyzer] in await analyzer.cancelAndFinishNow() }
    }

    private static func copyBuffer(_ source: AVAudioPCMBuffer) -> AVAudioPCMBuffer? {
        guard let copy = AVAudioPCMBuffer(pcmFormat: source.format, frameCapacity: source.frameCapacity) else { return nil }
        copy.frameLength = source.frameLength
        let sourceBuffers = UnsafeMutableAudioBufferListPointer(source.mutableAudioBufferList)
        let destinationBuffers = UnsafeMutableAudioBufferListPointer(copy.mutableAudioBufferList)
        guard sourceBuffers.count == destinationBuffers.count else { return nil }
        for index in sourceBuffers.indices {
            guard let sourceData = sourceBuffers[index].mData, let destinationData = destinationBuffers[index].mData else { return nil }
            memcpy(destinationData, sourceData, Int(sourceBuffers[index].mDataByteSize))
            destinationBuffers[index].mDataByteSize = sourceBuffers[index].mDataByteSize
        }
        return copy
    }
}
#endif
