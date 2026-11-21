import Foundation
@preconcurrency import AVFoundation
#if os(iOS)
import AudioToolbox
#endif

/// Raw input-tap health, measured before conversion: it answers "is Core Audio delivering anything",
/// never "did the user speak".
public struct CaptureRawHealth: Sendable, Equatable {
    public let callbackCount: Int
    public let frameCount: Int
    public let zeroSampleFraction: Double
    public let maxAbs: Float
    public let rms: Double
    /// Milliseconds from `start()` to the first tap callback, or -1.
    public let firstCallbackMs: Int
    /// Milliseconds from `start()` to the first non-zero sample, or -1.
    public let firstNonzeroMs: Int
    public let inputSampleRate: Double
    public let inputChannelCount: UInt32
    /// Milliseconds the tap has been running, so callback rate is derivable.
    public let elapsedMs: Int

    public enum Verdict: String, Sendable {
        case healthy
        /// Nonzero but far below any plausible microphone floor. Report it; do not block on it.
        case suspectLowSignal = "suspect_low_signal"
        /// No callbacks at all.
        case deadNoCallbacks = "dead_no_callbacks"
        /// Callbacks arrive but every sample is zero.
        case deadZeroSignal = "dead_zero_signal"

        public var isDead: Bool { self == .deadNoCallbacks || self == .deadZeroSignal }
    }

    public init(
        callbackCount: Int, frameCount: Int, zeroSampleFraction: Double, maxAbs: Float, rms: Double,
        firstCallbackMs: Int, firstNonzeroMs: Int, inputSampleRate: Double, inputChannelCount: UInt32,
        elapsedMs: Int,
    ) {
        self.callbackCount = callbackCount
        self.frameCount = frameCount
        self.zeroSampleFraction = zeroSampleFraction
        self.maxAbs = maxAbs
        self.rms = rms
        self.firstCallbackMs = firstCallbackMs
        self.firstNonzeroMs = firstNonzeroMs
        self.inputSampleRate = inputSampleRate
        self.inputChannelCount = inputChannelCount
        self.elapsedMs = elapsedMs
    }

    public var telemetryFields: [String: Any] {
        [
            "raw_callback_count": callbackCount, "raw_frame_count": frameCount,
            "raw_zero_sample_fraction": zeroSampleFraction, "raw_max_abs": Double(maxAbs),
            "raw_rms": rms, "raw_first_callback_ms": firstCallbackMs,
            "raw_first_nonzero_ms": firstNonzeroMs, "raw_input_sample_rate": inputSampleRate,
            "raw_input_channels": Int(inputChannelCount), "capture_health": verdict.rawValue,
            "raw_elapsed_ms": elapsedMs,
            "raw_callbacks_per_second": elapsedMs > 0 ? Double(callbackCount) * 1000 / Double(elapsedMs) : 0,
            "raw_frames_per_second": elapsedMs > 0 ? Double(frameCount) * 1000 / Double(elapsedMs) : 0,
        ]
    }

    /// Deliberately conservative: `dead` is reserved for digital silence, `suspectLowSignal` only reports.
    /// Ordinary microphones peak around 0.2; measured dead paths peaked at 0.0000 to 0.0017.
    public var verdict: Verdict {
        if callbackCount == 0 || frameCount == 0 { return .deadNoCallbacks }
        if maxAbs <= 1e-6 || zeroSampleFraction >= 0.999 { return .deadZeroSignal }
        if maxAbs < 1e-3 { return .suspectLowSignal }
        return .healthy
    }
}

public enum WarmAudioCaptureError: Error, Sendable, Equatable, LocalizedError {
    /// The hardware input format is unusable (zero rate or channel count) or no converter exists.
    case unsupportedFormat
    /// `start()` while a capture is running. Never reported as success: a no-op start can produce a
    /// recording that is live in the UI while no samples arrive.
    case alreadyRunning
    /// The engine started but the tap delivers nothing usable. Not retryable on the same route.
    case capturePathDead(CaptureRawHealth)
    /// The graph was replaced by a newer lifecycle event; abandon this attempt.
    case lifecycleInvalidated
    case engineFailed(String)

    public var errorDescription: String? {
        switch self {
        case .unsupportedFormat: return "The microphone format is not usable."
        case .alreadyRunning: return "Capture is already running."
        case let .capturePathDead(health): return "The microphone tap is not delivering audio (\(health.verdict.rawValue))."
        case .lifecycleInvalidated: return "The capture graph was replaced."
        case let .engineFailed(reason): return "The audio engine failed: \(reason)"
        }
    }
}

/// What happened to the engine graph when Core Audio changed its configuration while it ran.
public enum WarmAudioCaptureConfigurationChange: Sendable, Equatable {
    /// The graph was stopped. Pass `generation` to
    /// ``WarmAudioCapture/recoverAfterConfigurationChange(expectedGeneration:)`` to rebuild it, or call `stop()`.
    case stopped(generation: UInt64)
    /// The graph was rebuilt automatically; `generation` is the new graph's.
    case rebuilt(generation: UInt64)
    case rebuildFailed(String)
}

/// A kept-warm `AVAudioEngine` microphone tap that converts every buffer to a caller-supplied format and
/// hands it to a callback on the tap thread.
///
/// - The engine is destroyed and rebuilt on every start. A failed start can leave an engine bound to a
///   stale hardware format, and the next `installTap` on it trips an uncatchable AVFoundation assertion.
/// - Every graph gets a generation. Tap buffers and configuration-change notifications from an older
///   graph are dropped, and `stop()` seals delivery: once it returns, the handler is never called again
///   for that graph.
/// - The session category, mode and options belong to the caller (``AudioSessionController``); this type
///   never touches the audio session.
/// - A "started but dead" tap is reported by ``verifyDelivering(timeout:)`` and, when
///   ``Configuration/deadTapTimeout`` is set, by `startFailureHandler`.
///
/// The buffer handler runs on a realtime audio thread: enqueue and return. Do not call `stop()` or
/// `start()` from inside it.
public final class WarmAudioCapture: @unchecked Sendable {
    public typealias RawHealth = CaptureRawHealth
    public typealias CaptureError = WarmAudioCaptureError

    public struct SpeechActivitySnapshot: Sendable {
        public let firstSpeechSample: Int?
        public let lastSpeechAt: DispatchTime?
        public var heardSpeech: Bool { firstSpeechSample != nil }

        public init(firstSpeechSample: Int?, lastSpeechAt: DispatchTime?) {
            self.firstSpeechSample = firstSpeechSample
            self.lastSpeechAt = lastSpeechAt
        }
    }

    public struct Configuration: @unchecked Sendable {
        /// Retain mono Float32 PCM for snapshots/checkpoints. Opt-in preserves callback-only capture.
        public var retainSamples: Bool
        /// The format handed to the buffer handler. Default: 16 kHz mono Float32, what the ASR runtimes expect.
        public var outputFormat: AVAudioFormat
        public var tapBufferSize: AVAudioFrameCount
        public var voiceProcessingEnabled: Bool
        /// Rebuild the graph automatically when Core Audio changes its configuration.
        public var rebuildOnConfigurationChange: Bool
        /// When set, report ``WarmAudioCaptureError/capturePathDead(_:)`` through `startFailureHandler`
        /// if no usable audio arrived this long after a start.
        public var deadTapTimeout: TimeInterval?
        /// Runs on the fresh, stopped graph's input node before its format is read (for example to
        /// configure RemoteIO). A throw fails the start.
        public var prepareInput: (@Sendable (AVAudioInputNode) throws -> Void)?

        /// Original initializer retained verbatim in signature for existing function references.
        public init(
            outputFormat: AVAudioFormat = WarmAudioCapture.mono16kFloat,
            tapBufferSize: AVAudioFrameCount = 4096,
            voiceProcessingEnabled: Bool = false,
            rebuildOnConfigurationChange: Bool = true,
            deadTapTimeout: TimeInterval? = nil,
            prepareInput: (@Sendable (AVAudioInputNode) throws -> Void)? = nil,
        ) {
            self.init(outputFormat: outputFormat, retainSamples: false, tapBufferSize: tapBufferSize,
                voiceProcessingEnabled: voiceProcessingEnabled,
                rebuildOnConfigurationChange: rebuildOnConfigurationChange,
                deadTapTimeout: deadTapTimeout, prepareInput: prepareInput)
        }

        public init(
            outputFormat: AVAudioFormat = WarmAudioCapture.mono16kFloat,
            retainSamples: Bool,
            tapBufferSize: AVAudioFrameCount = 4096,
            voiceProcessingEnabled: Bool = false,
            rebuildOnConfigurationChange: Bool = true,
            deadTapTimeout: TimeInterval? = nil,
            prepareInput: (@Sendable (AVAudioInputNode) throws -> Void)? = nil,
        ) {
            self.retainSamples = retainSamples
            self.outputFormat = outputFormat
            self.tapBufferSize = tapBufferSize
            self.voiceProcessingEnabled = voiceProcessingEnabled
            self.rebuildOnConfigurationChange = rebuildOnConfigurationChange
            self.deadTapTimeout = deadTapTimeout
            self.prepareInput = prepareInput
        }
    }

    /// 16 kHz mono Float32, non-interleaved.
    nonisolated(unsafe) public static let mono16kFloat = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!

    public static let targetSampleRate: Double = 16_000

    public typealias BufferHandler = @Sendable (AVAudioPCMBuffer) -> Void

    public let configuration: Configuration

    /// Receives each converted buffer on the tap thread. Set before `start()`.
    public var bufferHandler: BufferHandler? {
        get { stateLock.withLock { _bufferHandler } }
        set { stateLock.withLock { _bufferHandler = newValue } }
    }

    /// Called when Core Audio reconfigures the running graph.
    public var configurationChangeHandler: (@Sendable (WarmAudioCaptureConfigurationChange) -> Void)? {
        get { stateLock.withLock { _configurationChangeHandler } }
        set { stateLock.withLock { _configurationChangeHandler = newValue } }
    }

    /// Called, once per start, when the dead-tap watchdog fires.
    public var startFailureHandler: (@Sendable (WarmAudioCaptureError) -> Void)? {
        get { stateLock.withLock { _startFailureHandler } }
        set { stateLock.withLock { _startFailureHandler = newValue } }
    }

    /// Serializes engine lifecycle mutation. The tap never takes it.
    private let lifecycleLock = NSLock()
    /// Guards the statistics and the active tap generation; the tap takes it briefly.
    private let stateLock = NSLock()
    /// Held by the tap for the whole of one delivery, and by `stop()` while it seals, so a callback in
    /// flight finishes before `stop()` returns and a later one is dropped.
    private let deliveryGate = NSLock()

    private var engine = AVAudioEngine()
    private var converter: AVAudioConverter?
    private var observer: NSObjectProtocol?
    private var running = false
    private var _engineGeneration: UInt64 = 0
    private var watchdog: Task<Void, Never>?

    private let voiceActivityDetector = VoiceActivityDetector()
    private var retainedSamples: [Float] = []
    private var accumulating = true
    private var peakAmplitude: Float = 0
    private var sumSquares: Double = 0
    private var voiceActiveFrames = 0
    private var voiceTotalFrames = 0
    private var _firstSpeechSample: Int?
    private var lastSpeechAt: DispatchTime?
    private var _pcmObserver: (@Sendable ([Float]) -> Void)?
    private var _engineConfigurationChangeHandler: (@Sendable (UInt64) -> Void)?
    private var attemptVoiceProcessingEnabled = false
    private var attemptInputOnlyIO = false
    private var preserveUnderlyingErrors = false

    /// Retained PCM delivery on tap thread. Enqueue & return; never call lifecycle or
    /// accumulation methods from this callback (they acquire deliveryGate).
    public var pcmObserver: (@Sendable ([Float]) -> Void)? {
        get { stateLock.withLock { _pcmObserver } }
        set { stateLock.withLock { _pcmObserver = newValue } }
    }

    /// Legacy generation notification: receives invalidated graph identity. Configure automatic
    /// rebuilding off when caller owns route-recovery policy.
    public var engineConfigurationChangeHandler: (@Sendable (UInt64) -> Void)? {
        get { stateLock.withLock { _engineConfigurationChangeHandler } }
        set { stateLock.withLock { _engineConfigurationChangeHandler = newValue } }
    }

    private var _bufferHandler: BufferHandler?
    private var _configurationChangeHandler: (@Sendable (WarmAudioCaptureConfigurationChange) -> Void)?
    private var _startFailureHandler: (@Sendable (WarmAudioCaptureError) -> Void)?
    private var activeTapGeneration: UInt64 = 0
    private var _latestLevel: Double = 0
    private var _boundInputSampleRate: Double = 0
    private var _boundInputChannelCount: UInt32 = 0
    private var startedAt: DispatchTime?
    private var firstCallbackAt: DispatchTime?
    private var firstNonzeroAt: DispatchTime?
    private var rawCallbackCount = 0
    private var rawFrameCount = 0
    private var rawSamples = 0
    private var rawZeroSamples = 0
    private var rawMaxAbs: Float = 0
    private var rawSumSquares: Double = 0
    private var converterErrorCount = 0
    private var droppedBufferCount = 0

    public init(configuration: Configuration = Configuration()) {
        self.configuration = configuration
    }

    deinit {
        if let observer { NotificationCenter.default.removeObserver(observer) }
        watchdog?.cancel()
    }

    // MARK: Lifecycle

    /// Builds a fresh engine graph, installs the tap and starts the engine. Throws
    /// ``WarmAudioCaptureError/alreadyRunning`` rather than silently succeeding when already running.
    public func start() throws {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        guard !running else { throw WarmAudioCaptureError.alreadyRunning }
        attemptVoiceProcessingEnabled = configuration.voiceProcessingEnabled
        attemptInputOnlyIO = false
        preserveUnderlyingErrors = false
        try startLocked(resetStatistics: true)
    }

    /// Starts with per-attempt hardware options; session/route policy remains caller-owned.
    public func start(
        voiceProcessingEnabled: Bool,
        inputOnlyIO: Bool = false,
        preservingExistingSamples: Bool = false
    ) throws {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        guard !running else { throw WarmAudioCaptureError.alreadyRunning }
        attemptVoiceProcessingEnabled = voiceProcessingEnabled
        attemptInputOnlyIO = inputOnlyIO
        preserveUnderlyingErrors = true
        try startLocked(resetStatistics: !preservingExistingSamples)
    }

    /// `start()`, then waits up to `timeout` for the tap to deliver usable audio. On a dead path it stops
    /// the capture and throws ``WarmAudioCaptureError/capturePathDead(_:)``.
    public func startVerified(timeout: TimeInterval = 1.0) async throws {
        try start()
        do {
            try await verifyDelivering(timeout: timeout)
        } catch {
            stop()
            throw error
        }
    }

    /// Polls the raw tap health until it is not dead or `timeout` passes. Throws
    /// ``WarmAudioCaptureError/capturePathDead(_:)`` with the last health on timeout.
    public func verifyDelivering(timeout: TimeInterval = 1.0) async throws {
        let deadline = DispatchTime.now().uptimeNanoseconds + UInt64(max(0, timeout) * 1_000_000_000)
        while true {
            let health = rawHealth
            if !health.verdict.isDead { return }
            if DispatchTime.now().uptimeNanoseconds >= deadline {
                throw WarmAudioCaptureError.capturePathDead(health)
            }
            try await Task.sleep(nanoseconds: 50_000_000)
        }
    }

    /// Stops the tap and engine. After it returns the buffer handler is not called again for this graph.
    public func stop() {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        _ = stopAndSnapshotLocked()
    }

    /// Seals PCM & stops at one lifecycle boundary. Existing `stop()` keeps its Void signature.
    public func stopAndReturnSamples() -> [Float] {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        return stopAndSnapshotLocked()
    }

    private func stopAndSnapshotLocked() -> [Float] {
        watchdog?.cancel()
        watchdog = nil
        deliveryGate.lock()
        let captured = stateLock.withLock {
            accumulating = false
            activeTapGeneration = _engineGeneration &+ 1
            return retainedSamples
        }
        deliveryGate.unlock()
        removeObserverLocked()
        if running {
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
            running = false
        }
        _engineGeneration &+= 1
        return captured
    }

    public var isMonitoring: Bool { isRunning }
    public var engineGeneration: UInt64 { generation }

    /// True while the engine is live.
    public var isRunning: Bool {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        return running
    }

    /// The identity of the current graph. A changed value means an earlier notification is stale.
    public var generation: UInt64 {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        return _engineGeneration
    }

    /// Rebuilds the graph after a configuration change stopped it. `expectedGeneration` is the value from
    /// ``WarmAudioCaptureConfigurationChange/stopped(generation:)``; a mismatch (another lifecycle event
    /// replaced the graph meanwhile) or a running capture throws ``WarmAudioCaptureError/lifecycleInvalidated``.
    public func recoverAfterConfigurationChange(expectedGeneration: UInt64) throws {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        guard !running, _engineGeneration == expectedGeneration else {
            throw WarmAudioCaptureError.lifecycleInvalidated
        }
        try startLocked(resetStatistics: false)
    }

    public func recoverAfterConfigurationChange(
        expectedGeneration: UInt64, voiceProcessingEnabled: Bool, inputOnlyIO: Bool
    ) throws {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        guard !running, _engineGeneration == expectedGeneration else {
            throw WarmAudioCaptureError.lifecycleInvalidated
        }
        attemptVoiceProcessingEnabled = voiceProcessingEnabled
        attemptInputOnlyIO = inputOnlyIO
        preserveUnderlyingErrors = true
        try startLocked(resetStatistics: false)
    }

    /// Starts a fresh recording on warm engine, serialized with complete tap delivery.
    public func beginAccumulation() {
        deliveryGate.lock()
        defer { deliveryGate.unlock() }
        voiceActivityDetector.reset()
        stateLock.withLock {
            resetAccumulationLocked()
            resetRawHealth()
            accumulating = true
        }
    }

    /// Seals retained PCM while leaving engine/tap/session warm.
    public func endAccumulation() -> [Float] {
        deliveryGate.lock()
        defer { deliveryGate.unlock() }
        return stateLock.withLock {
            accumulating = false
            return retainedSamples
        }
    }

    public var boundInputSampleRate: Double { stateLock.withLock { _boundInputSampleRate } }
    public var boundInputChannelCount: UInt32 { stateLock.withLock { _boundInputChannelCount } }
    public var conversionTargetSampleRate: Double { configuration.outputFormat.sampleRate }
    public var sampleCount: Int { stateLock.withLock { retainedSamples.count } }
    public var firstSpeechSample: Int? { stateLock.withLock { _firstSpeechSample } }
    public var speechActivity: SpeechActivitySnapshot {
        stateLock.withLock { SpeechActivitySnapshot(firstSpeechSample: _firstSpeechSample, lastSpeechAt: lastSpeechAt) }
    }
    public var captureLevelSummary: (peak: Float, rms: Double, sampleCount: Int) {
        stateLock.withLock {
            let count = retainedSamples.count
            return (peakAmplitude, count > 0 ? (sumSquares / Double(count)).squareRoot() : 0, count)
        }
    }
    public var conversionSummary: [String: Any] {
        stateLock.withLock {
            ["converter_error_count": converterErrorCount, "converter_dropped_buffers": droppedBufferCount,
             "vad_active_fraction": voiceTotalFrames > 0 ? Double(voiceActiveFrames) / Double(voiceTotalFrames) : 0,
             "vad_frames": voiceTotalFrames]
        }
    }
    public func samples(from index: Int) -> [Float] {
        stateLock.withLock {
            let start = min(max(index, 0), retainedSamples.count)
            return Array(retainedSamples[start...])
        }
    }

    /// Caller holds stateLock; resets one retained recording's statistics.
    private func resetAccumulationLocked() {
        retainedSamples.removeAll(keepingCapacity: true)
        _latestLevel = 0
        peakAmplitude = 0
        sumSquares = 0
        converterErrorCount = 0
        droppedBufferCount = 0
        voiceActiveFrames = 0
        voiceTotalFrames = 0
        _firstSpeechSample = nil
        lastSpeechAt = nil
    }

    // MARK: Observation

    /// Latest normalized microphone level (0...1) for meters. It keeps updating while the handler is idle.
    public var latestLevel: Double { stateLock.withLock { _latestLevel } }

    /// Snapshot of the raw input path.
    public var rawHealth: CaptureRawHealth {
        stateLock.withLock {
            let started = startedAt
            func elapsed(_ instant: DispatchTime?) -> Int {
                guard let instant, let started, instant.uptimeNanoseconds >= started.uptimeNanoseconds else { return -1 }
                return Int((instant.uptimeNanoseconds - started.uptimeNanoseconds) / 1_000_000)
            }
            return CaptureRawHealth(
                callbackCount: rawCallbackCount,
                frameCount: rawFrameCount,
                zeroSampleFraction: rawSamples > 0 ? Double(rawZeroSamples) / Double(rawSamples) : 1,
                maxAbs: rawMaxAbs,
                rms: rawSamples > 0 ? (rawSumSquares / Double(rawSamples)).squareRoot() : 0,
                firstCallbackMs: elapsed(firstCallbackAt),
                firstNonzeroMs: elapsed(firstNonzeroAt),
                inputSampleRate: _boundInputSampleRate,
                inputChannelCount: _boundInputChannelCount,
                elapsedMs: elapsed(DispatchTime.now()),
            )
        }
    }

    /// Converter failures and buffers that converted to nothing, since the last fresh start.
    public var conversionCounters: (errors: Int, droppedBuffers: Int) {
        stateLock.withLock { (converterErrorCount, droppedBufferCount) }
    }

    // MARK: Graph construction

    private func startLocked(resetStatistics: Bool) throws {
        removeObserverLocked()
        engine.stop()
        let graph = AVAudioEngine()
        engine = graph
        _engineGeneration &+= 1
        let generation = _engineGeneration
        deliveryGate.lock()
        stateLock.withLock { activeTapGeneration = generation }
        deliveryGate.unlock()

        let input = graph.inputNode
        do {
            if attemptInputOnlyIO { try Self.configureInputOnly(input) }
            #if !os(watchOS)
            if attemptVoiceProcessingEnabled { try input.setVoiceProcessingEnabled(true) }
            #else
            guard !attemptVoiceProcessingEnabled else { throw WarmAudioCaptureError.unsupportedFormat }
            #endif
            try configuration.prepareInput?(input)
        } catch {
            if preserveUnderlyingErrors { throw error }
            throw WarmAudioCaptureError.engineFailed(error.localizedDescription)
        }
        let inputFormat = input.outputFormat(forBus: 0)
        if configuration.retainSamples {
            guard configuration.outputFormat.commonFormat == .pcmFormatFloat32,
                  configuration.outputFormat.channelCount == 1,
                  configuration.outputFormat.sampleRate == Self.targetSampleRate else {
                throw WarmAudioCaptureError.unsupportedFormat
            }
        }
        // Validate that the format is usable, never that it equals the target: 48 kHz hardware feeding a
        // 16 kHz converter is normal; a zero rate or channel count is the "route not ready" signal that
        // would otherwise reach installTap as an uncatchable assertion.
        guard inputFormat.sampleRate > 0, inputFormat.channelCount > 0,
              let newConverter = AVAudioConverter(from: inputFormat, to: configuration.outputFormat)
        else { throw WarmAudioCaptureError.unsupportedFormat }

        deliveryGate.lock()
        voiceActivityDetector.reset()
        stateLock.withLock {
            converter = newConverter
            _boundInputSampleRate = inputFormat.sampleRate
            _boundInputChannelCount = inputFormat.channelCount
            if resetStatistics {
                resetAccumulationLocked()
            }
            resetRawHealth()
            accumulating = true
        }
        deliveryGate.unlock()

        installObserverLocked(for: graph, generation: generation)
        input.installTap(onBus: 0, bufferSize: configuration.tapBufferSize, format: inputFormat) { [weak self] buffer, _ in
            self?.consume(buffer, generation: generation)
        }
        graph.prepare()
        do {
            try graph.start()
        } catch {
            // The route can be rejected here. Leave no tap behind so the next attempt builds a fresh graph.
            input.removeTap(onBus: 0)
            removeObserverLocked()
            graph.stop()
            deliveryGate.lock()
            stateLock.withLock {
                activeTapGeneration = generation &+ 1
                converter = nil
            }
            deliveryGate.unlock()
            if preserveUnderlyingErrors { throw error }
            throw WarmAudioCaptureError.engineFailed(error.localizedDescription)
        }
        running = true
        armWatchdogLocked(generation: generation)
    }

    private static func configureInputOnly(_ input: AVAudioInputNode) throws {
        #if os(iOS)
        guard let audioUnit = input.audioUnit else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(kAudioUnitErr_Uninitialized))
        }
        var disabled: UInt32 = 0
        let status = AudioUnitSetProperty(audioUnit, kAudioOutputUnitProperty_EnableIO,
            kAudioUnitScope_Output, 0, &disabled, UInt32(MemoryLayout<UInt32>.size))
        var after: UInt32 = 1
        var size = UInt32(MemoryLayout<UInt32>.size)
        let readStatus = AudioUnitGetProperty(audioUnit, kAudioOutputUnitProperty_EnableIO,
            kAudioUnitScope_Output, 0, &after, &size)
        guard status == noErr, readStatus == noErr, after == 0 else {
            let code = status != noErr ? status : (readStatus != noErr ? readStatus : kAudioUnitErr_FailedInitialization)
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(code))
        }
        #else
        throw WarmAudioCaptureError.unsupportedFormat
        #endif
    }

    private func armWatchdogLocked(generation: UInt64) {
        watchdog?.cancel()
        watchdog = nil
        guard let timeout = configuration.deadTapTimeout else { return }
        watchdog = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(max(0, timeout) * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.watchdogFired(generation: generation)
        }
    }

    private func watchdogFired(generation: UInt64) {
        lifecycleLock.lock()
        let current = running && _engineGeneration == generation
        lifecycleLock.unlock()
        guard current else { return }
        let health = rawHealth
        guard health.verdict.isDead else { return }
        startFailureHandler?(.capturePathDead(health))
    }

    private func installObserverLocked(for graph: AVAudioEngine, generation: UInt64) {
        observer = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: graph, queue: nil,
        ) { [weak self, weak graph] _ in
            self?.handleConfigurationChange(graph: graph, generation: generation)
        }
    }

    private func removeObserverLocked() {
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = nil
    }

    private func handleConfigurationChange(graph: AVAudioEngine?, generation: UInt64) {
        lifecycleLock.lock()
        guard running, generation == _engineGeneration, let graph, graph === engine else {
            lifecycleLock.unlock()
            return
        }
        // The change invalidates this graph. Stop it first so a stale tap cannot deliver into a
        // replacement graph, and move the tap generation on so in-flight buffers are dropped.
        watchdog?.cancel()
        watchdog = nil
        removeObserverLocked()
        graph.inputNode.removeTap(onBus: 0)
        graph.stop()
        running = false
        _engineGeneration &+= 1
        deliveryGate.lock()
        stateLock.withLock { activeTapGeneration = _engineGeneration }
        deliveryGate.unlock()
        var outcome: WarmAudioCaptureConfigurationChange = .stopped(generation: _engineGeneration)
        if configuration.rebuildOnConfigurationChange {
            do {
                try startLocked(resetStatistics: false)
                outcome = .rebuilt(generation: _engineGeneration)
            } catch {
                outcome = .rebuildFailed(error.localizedDescription)
            }
        }
        lifecycleLock.unlock()
        configurationChangeHandler?(outcome)
        engineConfigurationChangeHandler?(generation)
    }

    // MARK: Tap path

    private func consume(_ buffer: AVAudioPCMBuffer, generation: UInt64) {
        deliveryGate.lock()
        defer { deliveryGate.unlock() }
        let (isCurrent, activeConverter, handler) = stateLock.withLock {
            (generation == activeTapGeneration, converter, _bufferHandler)
        }
        guard isCurrent else { return }
        measureRaw(buffer)
        guard let activeConverter else { return }
        guard let converted = Self.convert(buffer, using: activeConverter, to: configuration.outputFormat) else {
            stateLock.withLock { converterErrorCount += 1 }
            return
        }
        guard converted.frameLength > 0 else {
            stateLock.withLock { droppedBufferCount += 1 }
            return
        }
        let level = Self.level(of: converted)
        stateLock.withLock { _latestLevel = level }
        if configuration.retainSamples, let channel = converted.floatChannelData?.pointee {
            let frame = UnsafeBufferPointer(start: channel, count: Int(converted.frameLength))
            var frameSum: Float = 0
            var peak: Float = 0
            for value in frame {
                frameSum += value * value
                peak = max(peak, abs(value))
            }
            let activity = voiceActivityDetector.ingest(rms: (frameSum / Float(frame.count)).squareRoot())
            let copied = Array(frame)
            let now = DispatchTime.now()
            let observer = stateLock.withLock { () -> (@Sendable ([Float]) -> Void)? in
                guard accumulating else { return nil }
                if activity == .speechStarted, _firstSpeechSample == nil { _firstSpeechSample = retainedSamples.count }
                if activity != .silence { lastSpeechAt = now }
                retainedSamples.append(contentsOf: copied)
                peakAmplitude = max(peakAmplitude, peak)
                sumSquares += Double(frameSum)
                voiceTotalFrames += 1
                if activity != .silence { voiceActiveFrames += 1 }
                return _pcmObserver
            }
            observer?(copied)
        }
        handler?(converted)
    }

    /// Converts one tap buffer. Returns nil when the converter reports an error. The converter is kept
    /// alive across calls (`.noDataNow`, not end-of-stream) so resampling state does not reset per buffer.
    static func convert(_ input: AVAudioPCMBuffer, using converter: AVAudioConverter, to outputFormat: AVAudioFormat) -> AVAudioPCMBuffer? {
        let ratio = outputFormat.sampleRate / input.format.sampleRate
        let capacity = AVAudioFrameCount(Double(input.frameLength) * ratio) + 1024
        guard let output = AVAudioPCMBuffer(pcmFormat: outputFormat, frameCapacity: capacity) else { return nil }
        var provided = false
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) { _, inStatus in
            if provided {
                inStatus.pointee = .noDataNow
                return nil
            }
            provided = true
            inStatus.pointee = .haveData
            return input
        }
        return status == .error ? nil : output
    }

    /// RMS to dBFS to 0...1, with a -60 dB floor.
    static func level(of buffer: AVAudioPCMBuffer) -> Double {
        guard let channel = buffer.floatChannelData?.pointee, buffer.frameLength > 0 else { return 0 }
        let count = Int(buffer.frameLength)
        var sumSquares: Float = 0
        for value in UnsafeBufferPointer(start: channel, count: count) { sumSquares += value * value }
        return meterLevel(rms: (sumSquares / Float(count)).squareRoot())
    }

    /// Maps linear RMS to a 0...1 meter value: -60 dBFS or lower is 0, 0 dBFS is 1.
    public static func meterLevel(rms: Float) -> Double {
        let db = rms > 0 ? 20 * log10(rms) : -60
        return Double(min(max((db + 60) / 60, 0), 1))
    }

    private func measureRaw(_ buffer: AVAudioPCMBuffer) {
        let now = DispatchTime.now()
        let frames = Int(buffer.frameLength)
        var maxAbs: Float = 0
        var zeros = 0
        var counted = 0
        var sumSquares: Double = 0
        if let channels = buffer.floatChannelData {
            let channelCount = Int(buffer.format.channelCount)
            let buffers = buffer.format.isInterleaved ? 1 : channelCount
            let valuesPerBuffer = buffer.format.isInterleaved ? frames * channelCount : frames
            for channel in 0..<buffers {
                for value in UnsafeBufferPointer(start: channels[channel], count: valuesPerBuffer) {
                    let magnitude = abs(value)
                    if magnitude > maxAbs { maxAbs = magnitude }
                    if value == 0 { zeros += 1 }
                    sumSquares += Double(value) * Double(value)
                    counted += 1
                }
            }
        }
        stateLock.withLock {
            rawCallbackCount += 1
            rawFrameCount += frames
            rawSamples += counted
            rawZeroSamples += zeros
            rawSumSquares += sumSquares
            if maxAbs > rawMaxAbs { rawMaxAbs = maxAbs }
            if firstCallbackAt == nil { firstCallbackAt = now }
            if firstNonzeroAt == nil, maxAbs > 0 { firstNonzeroAt = now }
        }
    }

    /// Callers hold `stateLock`.
    private func resetRawHealth() {
        rawCallbackCount = 0
        rawFrameCount = 0
        rawSamples = 0
        rawZeroSamples = 0
        rawMaxAbs = 0
        rawSumSquares = 0
        startedAt = DispatchTime.now()
        firstCallbackAt = nil
        firstNonzeroAt = nil
    }
}
