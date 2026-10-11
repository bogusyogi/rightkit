import AVFoundation
import Foundation
import Speech

/// Microphone dictation on Apple's on-device recognizers: `SpeechAnalyzer` on iOS/macOS 26+,
/// `SFSpeechAudioBufferRecognitionRequest` before that (CodeRight `SpeechDictation`, minus its
/// SwiftUI observation and app signposts). Permissions are injected so the app owns its prompts.
///
/// Cancellation error codes from `kLSRErrorDomain` (209, 216) and `kAFAssistantErrorDomain` (216)
/// are the recognizer reporting a normal stop, not a failure.
public final class LiveDictationSession: @unchecked Sendable {
    public enum Event: Sendable, Equatable {
        case status(String?)
        case partial(String)
        case final(String)
        case failed(String)
    }

    public typealias SpeechAuthorizationProvider = @Sendable () async -> Bool
    public typealias MicrophonePermissionProvider = @Sendable () async -> Bool

    private let lock = NSLock()
    private let speechAuthorization: SpeechAuthorizationProvider
    private let microphonePermission: MicrophonePermissionProvider
    private let localeCandidates: [Locale]
    private let legacyFactory: LegacyCaptureFactory
    private let modernEnabled: Bool
    /// Bumped by every `start()` and `stop()`. A start owns exactly the resources it publishes while
    /// its generation is still current; anything it built after that point is torn down by the
    /// start itself, never left for (or taken from) a later owner.
    private var generation = 0
    private var legacy: (any LegacyCaptureHandle)?
    private var modern: AnyObject?
    private var startTask: Task<Void, Never>?
    /// Test seam: the most recent start's task, kept after `stop()` so tests can await it.
    private(set) var lastStartTask: Task<Void, Never>?

    public convenience init(
        localeCandidates: [Locale] = Dictation.localeCandidates(),
        speechAuthorization: @escaping SpeechAuthorizationProvider = { await LiveDictationSession.requestSpeechAuthorization() },
        microphonePermission: @escaping MicrophonePermissionProvider = { await LiveDictationSession.requestMicrophonePermission() }
    ) {
        self.init(
            localeCandidates: localeCandidates, speechAuthorization: speechAuthorization,
            microphonePermission: microphonePermission, legacyFactory: LegacyCapture.start, modernEnabled: true)
    }

    /// Test seam: substitutes the legacy capture factory and can disable the iOS/macOS 26 path.
    init(
        localeCandidates: [Locale],
        speechAuthorization: @escaping SpeechAuthorizationProvider,
        microphonePermission: @escaping MicrophonePermissionProvider,
        legacyFactory: @escaping LegacyCaptureFactory,
        modernEnabled: Bool
    ) {
        self.localeCandidates = localeCandidates
        self.speechAuthorization = speechAuthorization
        self.microphonePermission = microphonePermission
        self.legacyFactory = legacyFactory
        self.modernEnabled = modernEnabled
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

    /// Starts capture; events arrive on an arbitrary queue until `stop()` or a final result.
    public func start(onEvent: @escaping @Sendable (Event) -> Void) {
        stop()
        let current: Int = lock.withLock { generation += 1; return generation }
        onEvent(.status("Preparing dictation…"))
        let task = Task<Void, Never> { [weak self] in
            guard let self else { return }
            await self.run(generation: current, onEvent: onEvent)
        }
        lock.withLock { startTask = task; lastStartTask = task }
    }

    public func stop() {
        let task: Task<Void, Never>? = lock.withLock { generation += 1; defer { startTask = nil }; return startTask }
        task?.cancel()
        teardown()
    }

    private func isCurrent(_ value: Int) -> Bool { lock.withLock { generation == value } }

    private func run(generation value: Int, onEvent: @escaping @Sendable (Event) -> Void) async {
        // Permission prompts can outlive the start that asked for them. Recheck after each await so a
        // canceled or replaced start never reports a stale failure to the current owner.
        let speechGranted = await speechAuthorization()
        guard isCurrent(value), !Task.isCancelled else { return }
        guard speechGranted else {
            onEvent(.failed("Dictation needs Speech Recognition access."))
            return
        }
        let microphoneGranted = await microphonePermission()
        guard isCurrent(value), !Task.isCancelled else { return }
        guard microphoneGranted else {
            onEvent(.failed("Dictation needs Microphone access."))
            return
        }
        do {
            #if os(iOS)
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
            #endif
            if modernEnabled, #available(iOS 26.0, macOS 26.0, *), let locale = await ModernCapture.supportedLocale(from: localeCandidates) {
                do {
                    let capture = try await ModernCapture.start(locale: locale, onEvent: { event in
                        if self.isCurrent(value) { onEvent(event) }
                    })
                    // Publish only if this start still owns the session; otherwise unwind it here.
                    guard !Task.isCancelled, adopt(generation: value, modern: capture) else {
                        capture.stop()
                        releaseAudioSessionIfIdle()
                        return
                    }
                    onEvent(.status(nil))
                    return
                } catch {
                    // Fall through to the legacy recognizer.
                }
            }
            guard isCurrent(value), !Task.isCancelled else { releaseAudioSessionIfIdle(); return }
            guard try beginLegacy(generation: value, onEvent: onEvent) else {
                releaseAudioSessionIfIdle()
                return
            }
            onEvent(.status(nil))
        } catch {
            // A superseded start owns nothing and must neither tear down its successor's capture
            // nor report a failure the current owner never asked for.
            guard isCurrent(value) else { releaseAudioSessionIfIdle(); return }
            teardown()
            onEvent(.failed("Couldn't start the microphone."))
        }
    }

    /// Builds the legacy capture and publishes it only while `value` is still the current
    /// generation. Returns `false` (after disposing everything it built) when a `stop()` or a newer
    /// `start()` arrived during setup.
    private func beginLegacy(generation value: Int, onEvent: @escaping @Sendable (Event) -> Void) throws -> Bool {
        let context = LegacyCaptureContext(
            localeCandidates: localeCandidates,
            isCurrent: { [weak self] in self?.isCurrent(value) ?? false },
            emit: onEvent,
            stop: { [weak self] in self?.stop() }
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
        let (legacy, modern) = lock.withLock { () -> ((any LegacyCaptureHandle)?, AnyObject?) in
            defer { self.legacy = nil; self.modern = nil }
            return (self.legacy, self.modern)
        }
        if #available(iOS 26.0, macOS 26.0, *) { (modern as? ModernCapture)?.stop() }
        legacy?.tearDown()
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
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
        // Local-only: pick only a recognizer that recognizes on-device, otherwise fail closed.
        let recognizer = try LegacySpeechPolicy.select(
            from: context.localeCandidates.compactMap { SFSpeechRecognizer(locale: $0) },
            isAvailable: { $0.isAvailable },
            supportsOnDevice: { $0.supportsOnDeviceRecognition }
        )
        let engine = AVAudioEngine()
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        request.taskHint = .dictation
        try LegacySpeechPolicy.configure(request, supportsOnDevice: recognizer.supportsOnDeviceRecognition)
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
