import Foundation
@preconcurrency import AVFoundation

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
    public struct Configuration: @unchecked Sendable {
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

        public init(
            outputFormat: AVAudioFormat = WarmAudioCapture.mono16kFloat,
            tapBufferSize: AVAudioFrameCount = 4096,
            voiceProcessingEnabled: Bool = false,
            rebuildOnConfigurationChange: Bool = true,
            deadTapTimeout: TimeInterval? = nil,
            prepareInput: (@Sendable (AVAudioInputNode) throws -> Void)? = nil,
        ) {
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
    private var engineGeneration: UInt64 = 0
    private var watchdog: Task<Void, Never>?

    private var _bufferHandler: BufferHandler?
    private var _configurationChangeHandler: (@Sendable (WarmAudioCaptureConfigurationChange) -> Void)?
    private var _startFailureHandler: (@Sendable (WarmAudioCaptureError) -> Void)?
    private var activeTapGeneration: UInt64 = 0
    private var _latestLevel: Double = 0
    private var boundInputSampleRate: Double = 0
    private var boundInputChannelCount: UInt32 = 0
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
        try startLocked(resetStatistics: true)
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
        watchdog?.cancel()
        watchdog = nil
        deliveryGate.lock()
        stateLock.withLock { activeTapGeneration = engineGeneration &+ 1 }
        deliveryGate.unlock()
        removeObserverLocked()
        if running {
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
            running = false
        }
        engineGeneration &+= 1
    }

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
        return engineGeneration
    }

    /// Rebuilds the graph after a configuration change stopped it. `expectedGeneration` is the value from
    /// ``WarmAudioCaptureConfigurationChange/stopped(generation:)``; a mismatch (another lifecycle event
    /// replaced the graph meanwhile) or a running capture throws ``WarmAudioCaptureError/lifecycleInvalidated``.
    public func recoverAfterConfigurationChange(expectedGeneration: UInt64) throws {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        guard !running, engineGeneration == expectedGeneration else {
            throw WarmAudioCaptureError.lifecycleInvalidated
        }
        try startLocked(resetStatistics: false)
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
                inputSampleRate: boundInputSampleRate,
                inputChannelCount: boundInputChannelCount,
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
        engineGeneration &+= 1
        let generation = engineGeneration
        stateLock.withLock { activeTapGeneration = generation }

        let input = graph.inputNode
        do {
            if configuration.voiceProcessingEnabled { try input.setVoiceProcessingEnabled(true) }
            try configuration.prepareInput?(input)
        } catch {
            throw WarmAudioCaptureError.engineFailed(error.localizedDescription)
        }
        let inputFormat = input.outputFormat(forBus: 0)
        // Validate that the format is usable, never that it equals the target: 48 kHz hardware feeding a
        // 16 kHz converter is normal; a zero rate or channel count is the "route not ready" signal that
        // would otherwise reach installTap as an uncatchable assertion.
        guard inputFormat.sampleRate > 0, inputFormat.channelCount > 0,
              let newConverter = AVAudioConverter(from: inputFormat, to: configuration.outputFormat)
        else { throw WarmAudioCaptureError.unsupportedFormat }

        stateLock.withLock {
            converter = newConverter
            boundInputSampleRate = inputFormat.sampleRate
            boundInputChannelCount = inputFormat.channelCount
            if resetStatistics {
                _latestLevel = 0
                converterErrorCount = 0
                droppedBufferCount = 0
            }
            resetRawHealth()
        }

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
            stateLock.withLock { converter = nil }
            throw WarmAudioCaptureError.engineFailed(error.localizedDescription)
        }
        running = true
        armWatchdogLocked(generation: generation)
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
        let current = running && engineGeneration == generation
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
        guard running, generation == engineGeneration, let graph, graph === engine else {
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
        engineGeneration &+= 1
        deliveryGate.lock()
        stateLock.withLock { activeTapGeneration = engineGeneration }
        deliveryGate.unlock()
        var outcome: WarmAudioCaptureConfigurationChange = .stopped(generation: engineGeneration)
        if configuration.rebuildOnConfigurationChange {
            do {
                try startLocked(resetStatistics: false)
                outcome = .rebuilt(generation: engineGeneration)
            } catch {
                outcome = .rebuildFailed(error.localizedDescription)
            }
        }
        lifecycleLock.unlock()
        configurationChangeHandler?(outcome)
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
            for channel in 0..<Int(buffer.format.channelCount) {
                for value in UnsafeBufferPointer(start: channels[channel], count: frames) {
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
