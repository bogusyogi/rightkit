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

    private static let cancellationCodes: [String: Set<Int>] = [
        "kLSRErrorDomain": [209, 216],
        "kAFAssistantErrorDomain": [216],
    ]

    private let lock = NSLock()
    private let speechAuthorization: SpeechAuthorizationProvider
    private let microphonePermission: MicrophonePermissionProvider
    private let localeCandidates: [Locale]
    private var generation = 0
    private var engine: AVAudioEngine?
    private var tapInstalled = false
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var modern: AnyObject?
    private var startTask: Task<Void, Never>?

    public init(
        localeCandidates: [Locale] = Dictation.localeCandidates(),
        speechAuthorization: @escaping SpeechAuthorizationProvider = { await LiveDictationSession.requestSpeechAuthorization() },
        microphonePermission: @escaping MicrophonePermissionProvider = { await LiveDictationSession.requestMicrophonePermission() }
    ) {
        self.localeCandidates = localeCandidates
        self.speechAuthorization = speechAuthorization
        self.microphonePermission = microphonePermission
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
        lock.withLock { startTask = task }
    }

    public func stop() {
        let task: Task<Void, Never>? = lock.withLock { generation += 1; defer { startTask = nil }; return startTask }
        task?.cancel()
        teardown()
    }

    private func isCurrent(_ value: Int) -> Bool { lock.withLock { generation == value } }

    private func run(generation value: Int, onEvent: @escaping @Sendable (Event) -> Void) async {
        guard await speechAuthorization() else {
            onEvent(.failed("Dictation needs Speech Recognition access."))
            return
        }
        guard await microphonePermission() else {
            onEvent(.failed("Dictation needs Microphone access."))
            return
        }
        guard isCurrent(value), !Task.isCancelled else { return }
        do {
            #if os(iOS)
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
            #endif
            if #available(iOS 26.0, macOS 26.0, *), let locale = await ModernCapture.supportedLocale(from: localeCandidates) {
                do {
                    let capture = try await ModernCapture.start(locale: locale, onEvent: { event in
                        if self.isCurrent(value) { onEvent(event) }
                    })
                    guard isCurrent(value), !Task.isCancelled else { capture.stop(); return }
                    lock.withLock { modern = capture }
                    onEvent(.status(nil))
                    return
                } catch {
                    // Fall through to the legacy recognizer.
                }
            }
            guard isCurrent(value), !Task.isCancelled else { return }
            try beginLegacy(generation: value, onEvent: onEvent)
            onEvent(.status(nil))
        } catch {
            teardown()
            onEvent(.failed("Couldn't start the microphone."))
        }
    }

    private func beginLegacy(generation value: Int, onEvent: @escaping @Sendable (Event) -> Void) throws {
        guard let recognizer = localeCandidates.compactMap({ SFSpeechRecognizer(locale: $0) }).first(where: { $0.isAvailable }) else {
            throw SpeechError.engineUnavailable("Dictation isn't available for this language.")
        }
        let engine = AVAudioEngine()
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        request.taskHint = .dictation
        if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.channelCount > 0 else { throw SpeechError.engineUnavailable("no audio input") }
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in request.append(buffer) }
        engine.prepare()
        try engine.start()
        let recognition = recognizer.recognitionTask(with: request) { [weak self] result, error in
            guard let self, self.isCurrent(value) else { return }
            if let result {
                let text = result.bestTranscription.formattedString
                if result.isFinal { onEvent(.final(text)); self.stop(); return }
                onEvent(.partial(text))
            }
            if let error {
                let value = error as NSError
                if Self.cancellationCodes[value.domain]?.contains(value.code) == true { self.stop(); return }
                onEvent(.failed("Couldn't transcribe that."))
                self.stop()
            }
        }
        lock.withLock {
            self.engine = engine
            self.tapInstalled = true
            self.request = request
            self.task = recognition
        }
    }

    private func teardown() {
        let (engine, installed, request, task, modern) = lock.withLock { () -> (AVAudioEngine?, Bool, SFSpeechAudioBufferRecognitionRequest?, SFSpeechRecognitionTask?, AnyObject?) in
            defer { self.engine = nil; self.tapInstalled = false; self.request = nil; self.task = nil; self.modern = nil }
            return (self.engine, self.tapInstalled, self.request, self.task, self.modern)
        }
        if #available(iOS 26.0, macOS 26.0, *) { (modern as? ModernCapture)?.stop() }
        if let engine {
            if installed { engine.inputNode.removeTap(onBus: 0) }
            if engine.isRunning { engine.stop() }
        }
        task?.cancel()
        request?.endAudio()
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
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
