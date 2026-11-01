@preconcurrency import AVFoundation
import AudioToolbox
import Foundation
import RightKitSpeech

/// Warm in-memory microphone capture, the iOS counterpart to the desktop's
/// kept-warm CPAL stream. Audio flows from an `AVAudioEngine` input tap through
/// a resampling converter straight into an in-memory mono-16k `[Float]` buffer.
///
/// Why this replaces `AVAudioRecorder`-to-file: the recorder writes a capture
/// file whose data chunk Core Audio finalizes *asynchronously* after `stop()`,
/// so a fast stop/read races the flush and throws `avfaudio error -50`. Holding
/// samples in memory removes the file, the finalization step, and that entire
/// class of intermittent failures — the transcriber decodes directly from RAM.
///
/// The tap callback runs on a realtime audio thread, so all shared state is
/// guarded by a lock. This type is deliberately not `@MainActor`.
final class WarmAudioCapture {
    struct SpeechActivitySnapshot {
        let firstSpeechSample: Int?
        let lastSpeechAt: DispatchTime?

        var heardSpeech: Bool { firstSpeechSample != nil }
    }

    /// Raw INPUT-TAP health, measured BEFORE the converter, the conditioning
    /// and the ASR. The question it answers is "is Core Audio delivering
    /// anything at all", never "did the user speak".
    struct RawHealth {
        let callbackCount: Int
        let frameCount: Int
        let zeroSampleFraction: Double
        let maxAbs: Float
        let rms: Double
        /// Milliseconds from `start()` to the first tap callback, or -1.
        let firstCallbackMs: Int
        /// Milliseconds from `start()` to the first non-zero sample, or -1.
        let firstNonzeroMs: Int
        let inputSampleRate: Double
        let inputChannelCount: UInt32
        /// Milliseconds the tap has been running, so callback RATE is
        /// derivable. Amplitude alone can be ambiguous on a zombie tap
        /// (raw max 0.00055): the sharper signal was that it delivered one
        /// callback where a healthy tap delivers several per second. Logged
        /// only — the blocking verdict is deliberately NOT retuned on n=1.
        let elapsedMs: Int

        enum Verdict: String {
            case healthy
            /// Nonzero but far below any plausible microphone floor. Logged
            /// aggressively, never blocking, until its real distribution is
            /// known.
            case suspectLowSignal = "suspect_low_signal"
            /// No callbacks at all, or a digitally dead stream.
            case deadNoCallbacks = "dead_no_callbacks"
            case deadZeroSignal = "dead_zero_signal"

            var isDead: Bool {
                self == .deadNoCallbacks || self == .deadZeroSignal
            }
        }

        /// Deliberately conservative. Ordinary Bose captures peak around
        /// 0.20–0.27; the pathological ones measured 0.0000, 0.0006 and
        /// 0.0017 — roughly 53 dB down. There is enormous headroom between a
        /// quiet room and a dead capture path, so `dead` is reserved for
        /// digital silence and `suspect` only reports.
        var verdict: Verdict {
            if callbackCount == 0 || frameCount == 0 { return .deadNoCallbacks }
            if maxAbs <= 1e-6 || zeroSampleFraction >= 0.999 { return .deadZeroSignal }
            if maxAbs < 1e-3 { return .suspectLowSignal }
            return .healthy
        }

        var telemetryFields: [String: Any] {
            [
                "raw_callback_count": callbackCount,
                "raw_frame_count": frameCount,
                "raw_zero_sample_fraction": zeroSampleFraction,
                "raw_max_abs": Double(maxAbs),
                "raw_rms": rms,
                "raw_first_callback_ms": firstCallbackMs,
                "raw_first_nonzero_ms": firstNonzeroMs,
                "raw_input_sample_rate": inputSampleRate,
                "raw_input_channels": Int(inputChannelCount),
                "capture_health": verdict.rawValue,
            "raw_elapsed_ms": elapsedMs,
            "raw_callbacks_per_second": elapsedMs > 0
                ? Double(callbackCount) * 1000 / Double(elapsedMs)
                : 0,
            "raw_frames_per_second": elapsedMs > 0
                ? Double(frameCount) * 1000 / Double(elapsedMs)
                : 0,
            ]
        }
    }

    enum CaptureError: Error {
        case unsupportedFormat
        /// `AVAudioEngine.start()` returned success but the input tap is not
        /// delivering usable PCM. NOT retryable: late rungs are what produce
        /// these zombie capture paths in the first place, so consuming more
        /// of them cannot help.
        case capturePathDead(RawHealth)
        /// `start()` was called while a capture was already running. Never
        /// silently reported as success: a no-op "start" can manufacture a
        /// recording that is live in the UI while zero samples arrive.
        case alreadyRunning
        /// The audio session or engine graph was invalidated by a system reset.
        /// The owning lifecycle must abandon this attempt instead of retrying a
        /// stale graph.
        case lifecycleInvalidated
    }

    /// `var`, not `let`, and REPLACED on every `start()`.
    ///
    /// A failed start can leave this engine bound to a stale hardware format, and
    /// the next retry's `installTap` then trips an AVFoundation assertion — an
    /// Objective-C exception Swift cannot catch, so the process aborts.
    /// INVARIANT: no retry may reuse an AVAudioEngine graph that was bound to
    /// a failed or stale hardware format. A fresh graph per attempt is what
    /// enforces it; do not hoist this back to a `let`.
    private var engine = AVAudioEngine()
    private let outputFormat: AVAudioFormat
    private var converter: AVAudioConverter?
    /// Serializes every AVAudioEngine lifecycle mutation. The sample lock
    /// cannot serve this role because the realtime tap also takes it.
    private let lifecycleLock = NSLock()
    private let lock = NSLock()
    /// Serializes tap delivery with stop's seal point so a callback already in
    /// flight finishes before the returned snapshot, while later callbacks
    /// cannot notify downstream decoders.
    private let accumulationGate = NSLock()
    private let voiceActivityDetector = VoiceActivityDetector()
    private var samples: [Float] = []
    private var _latestLevel: Double = 0
    private var _latestVoiceActivity: VoiceActivityEvent = .silence
    /// The hardware input format this attempt's engine actually bound to.
    /// Reported as `hardware_input_rate` / `hardware_channels` telemetry so a
    /// route problem is diagnosable from trace alone.
    private var _boundInputSampleRate: Double = 0
    private var _boundInputChannelCount: UInt32 = 0
    /// Whole-capture level summary, accumulated on the realtime tap thread
    /// under `lock`. Content-free amplitude statistics only — never audio.
    /// Reported at stop so "the transcription was garbage" can be separated
    /// from "the microphone delivered almost nothing".
    private var _peakAmplitude: Float = 0
    private var _sumSquares: Double = 0
    /// RAW tap statistics, before conversion — see `RawHealth`.
    private var _rawCallbackCount = 0
    private var _rawFrameCount = 0
    private var _rawZeroSamples = 0
    private var _rawSamples = 0
    private var _rawMaxAbs: Float = 0
    private var _rawSumSquares: Double = 0
    /// Conversion health and speech coverage, so a level collapse can be
    /// located: raw tap → converter → conditioning → ASR.
    private var _converterErrorCount = 0
    private var _converterDroppedBufferCount = 0
    private var _voiceActiveFrames = 0
    private var _voiceTotalFrames = 0
    private var _firstSpeechSample: Int?
    private var _lastSpeechAt: DispatchTime?
    /// Tap generation mirrored under `lock`. Tap callbacks must not take
    /// `lifecycleLock` while holding `accumulationGate`: stop owns those locks
    /// in the opposite order while sealing the PCM snapshot.
    private var _activeTapGeneration: UInt64 = 0
    private var _startedAt: DispatchTime?
    private var _firstCallbackAt: DispatchTime?
    private var _firstNonzeroAt: DispatchTime?
    /// Monotonic graph identity. Every new engine graph gets a new value, and
    /// stop/configuration reset invalidates callbacks from older graphs.
    private var _engineGeneration: UInt64 = 0
    private var engineConfigurationObserver: NSObjectProtocol?
    private var isRunning = false
    /// Whether the tap's converted PCM is being KEPT. The WARM arm leaves the
    /// engine and tap running between dictations with this false, so buffers
    /// keep flowing and are discarded — the Bluetooth I/O path is never
    /// released, and a background start has no engine to acquire.
    private var accumulating = true
    /// Runs after conversion on AVAudioEngine's tap thread. Owner must enqueue
    /// work immediately; no model inference belongs on realtime audio thread.
    var pcmObserver: (@Sendable ([Float]) -> Void)?
    /// Called when Core Audio changes this graph's format while it is running.
    /// The generation lets the coordinator discard a late notification after a
    /// newer graph has already replaced this one.
    var engineConfigurationChangeHandler: (@Sendable (UInt64) -> Void)?

    /// 16 kHz mono float — the exact format the ASR runtimes expect.
    static let targetSampleRate: Double = 16_000

    init() {
        outputFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: Self.targetSampleRate,
            channels: 1,
            interleaved: false
        )!
    }

    deinit {
        if let observer = engineConfigurationObserver {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    /// Reusable across start/stop cycles: this is the ONE `WarmAudioCapture`
    /// the coordinator owns for the process lifetime. The `AVAudioEngine`
    /// inside it, however, is destroyed and rebuilt on EVERY attempt — see
    /// the `engine` declaration for the crash this prevents.
    ///
    /// A `start()` arriving while a capture is already running throws
    /// `alreadyRunning` rather than returning success. The previous silent
    /// `return` could report a started recording that had no engine of its
    /// own and delivered zero samples.
    func start(
        voiceProcessingEnabled: Bool = false,
        inputOnlyIO: Bool = false,
        preservingExistingSamples: Bool = false
    ) throws {
        lifecycleLock.lock()
        defer { lifecycleLock.unlock() }
        guard !isRunning else { throw CaptureError.alreadyRunning }
        lock.lock(); accumulating = true; lock.unlock()

        // Destroy the previous graph before building a new one. A prior
        // attempt may have left this engine bound to a hardware format that
        // no longer matches the route.
        removeEngineConfigurationObserverLocked()
        engine.stop()
        engine = AVAudioEngine()
        _engineGeneration &+= 1
        let engineGeneration = _engineGeneration
        lock.lock()
        _activeTapGeneration = engineGeneration
        lock.unlock()

        let input = engine.inputNode
        if inputOnlyIO {
            guard let audioUnit = input.audioUnit else {
                throw NSError(domain: NSOSStatusErrorDomain, code: Int(kAudioUnitErr_Uninitialized), userInfo: [
                    NSLocalizedDescriptionKey: "RemoteIO is unavailable for input-only configuration",
                ])
            }
            var before: UInt32 = 1
            var beforeSize = UInt32(MemoryLayout<UInt32>.size)
            let readBeforeStatus = AudioUnitGetProperty(
                audioUnit,
                kAudioOutputUnitProperty_EnableIO,
                kAudioUnitScope_Output,
                0,
                &before,
                &beforeSize
            )
            var disabled: UInt32 = 0
            let status = AudioUnitSetProperty(
                audioUnit,
                kAudioOutputUnitProperty_EnableIO,
                kAudioUnitScope_Output,
                0,
                &disabled,
                UInt32(MemoryLayout<UInt32>.size)
            )
            var after: UInt32 = 1
            var afterSize = UInt32(MemoryLayout<UInt32>.size)
            let readAfterStatus = AudioUnitGetProperty(
                audioUnit,
                kAudioOutputUnitProperty_EnableIO,
                kAudioUnitScope_Output,
                0,
                &after,
                &afterSize
            )
            DogfoodTraceStore.record("record_input_only_io_configured", fields: [
                "requested": true,
                "status": Int(status),
                "read_before_status": Int(readBeforeStatus),
                "output_enabled_before": Int(before),
                "read_after_status": Int(readAfterStatus),
                "output_enabled_after": Int(after),
                "configured": status == noErr && readAfterStatus == noErr && after == 0,
            ])
            guard status == noErr, readAfterStatus == noErr, after == 0 else {
                throw NSError(domain: NSOSStatusErrorDomain, code: Int(status), userInfo: [
                    NSLocalizedDescriptionKey: "Input-only RemoteIO configuration failed",
                ])
            }
        }
        if voiceProcessingEnabled {
            // Configure the stopped, fresh graph before reading its format.
            // A failure must surface; silently falling back would invalidate the arm.
            try input.setVoiceProcessingEnabled(true)
        }
        DogfoodTraceStore.record("record_voice_processing_configured", fields: [
            "requested": voiceProcessingEnabled,
            "enabled": input.isVoiceProcessingEnabled,
        ])
        let inputFormat = input.outputFormat(forBus: 0)
        // Validate that the hardware format is USABLE — never that it equals
        // the 16 kHz target. 48 kHz hardware feeding a 16 kHz converter is
        // normal and correct; a zeroed rate or channel count is the actual
        // "route not ready" signal, and is what `installTap` would otherwise
        // turn into an uncatchable assertion.
        guard inputFormat.sampleRate > 0, inputFormat.channelCount > 0 else {
            throw CaptureError.unsupportedFormat
        }
        guard let converter = AVAudioConverter(from: inputFormat, to: outputFormat) else {
            throw CaptureError.unsupportedFormat
        }
        self.converter = converter
        lock.lock()
        _boundInputSampleRate = inputFormat.sampleRate
        _boundInputChannelCount = inputFormat.channelCount
        if !preservingExistingSamples {
            samples.removeAll(keepingCapacity: true)
            _latestLevel = 0
            _latestVoiceActivity = .silence
            _firstSpeechSample = nil
            _lastSpeechAt = nil
            _peakAmplitude = 0
            _sumSquares = 0
        }
        lock.unlock()
        voiceActivityDetector.reset()

        lock.lock()
        resetRawHealthLocked()
        lock.unlock()

        installEngineConfigurationObserverLocked(for: engine, generation: engineGeneration)
        do {
            if #available(iOS 27.0, *) {
                try input.installAudioTap(onBus: 0, bufferSize: 4096, format: inputFormat) { [weak self] buffer, _ in
                    // The iOS 27 tap is read-only & Sendable. Copy into the
                    // existing mutable buffer path before touching converter
                    // state, retaining one production ingest implementation.
                    let mutable = AVAudioPCMBuffer(copying: buffer)
                    self?.consumeTapBuffer(mutable, generation: engineGeneration)
                }
            } else {
                input.installTap(onBus: 0, bufferSize: 4096, format: inputFormat) { [weak self] buffer, _ in
                    self?.consumeTapBuffer(buffer, generation: engineGeneration)
                }
            }
        } catch {
            removeEngineConfigurationObserverLocked()
            engine.stop()
            self.converter = nil
            throw error
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            // AVAudioEngine can reject a newly-set route. Leave no stale tap
            // behind so recovery can construct a genuinely fresh engine.
            input.removeTap(onBus: 0)
            removeEngineConfigurationObserverLocked()
            engine.stop()
            self.converter = nil
            throw error
        }
        isRunning = true
    }

    /// Rebuild the graph after AVAudioEngine stopped itself for a format
    /// change, retaining already captured PCM. The expected generation fence
    /// rejects a late notification after another lifecycle event replaced the
    /// graph.
    func recoverAfterConfigurationChange(
        expectedGeneration: UInt64,
        voiceProcessingEnabled: Bool,
        inputOnlyIO: Bool
    ) throws {
        lifecycleLock.lock()
        let valid = !isRunning && _engineGeneration == expectedGeneration
        lifecycleLock.unlock()
        guard valid else { throw CaptureError.lifecycleInvalidated }
        try start(
            voiceProcessingEnabled: voiceProcessingEnabled,
            inputOnlyIO: inputOnlyIO,
            preservingExistingSamples: true
        )
    }

    /// True while the engine is live, whether or not samples are being kept.
    var isMonitoring: Bool {
        lifecycleLock.lock(); defer { lifecycleLock.unlock() }
        return isRunning
    }

    /// Current graph identity. A changed value means an earlier tap or
    /// configuration notification is stale.
    var engineGeneration: UInt64 {
        lifecycleLock.lock(); defer { lifecycleLock.unlock() }
        return _engineGeneration
    }

    private func installEngineConfigurationObserverLocked(for graph: AVAudioEngine, generation: UInt64) {
        engineConfigurationObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange,
            object: graph,
            queue: nil
        ) { [weak self, weak graph] _ in
            self?.handleEngineConfigurationChange(graph: graph, generation: generation)
        }
    }

    private func removeEngineConfigurationObserverLocked() {
        if let observer = engineConfigurationObserver {
            NotificationCenter.default.removeObserver(observer)
            engineConfigurationObserver = nil
        }
    }

    private func handleEngineConfigurationChange(graph: AVAudioEngine?, generation: UInt64) {
        lifecycleLock.lock()
        guard isRunning, generation == _engineGeneration,
              let graph, graph === engine else {
            lifecycleLock.unlock()
            return
        }
        // A configuration change invalidates this graph. Stop it before the
        // coordinator decides whether to seal the current recording; this
        // prevents a stale tap from appending into a replacement graph.
        removeEngineConfigurationObserverLocked()
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        isRunning = false
        _engineGeneration &+= 1
        accumulationGate.lock()
        lock.lock()
        _activeTapGeneration = _engineGeneration
        lock.unlock()
        accumulationGate.unlock()
        let handler = engineConfigurationChangeHandler
        lifecycleLock.unlock()
        DogfoodTraceStore.record("record_engine_configuration_changed", fields: [
            "generation": generation,
            "next_generation": engineGeneration,
        ])
        handler?(generation)
    }

    /// Resets the raw-health counters for a fresh measurement window. Callers
    /// must already hold `lock`.
    private func resetRawHealthLocked() {
        _rawCallbackCount = 0
        _rawFrameCount = 0
        _rawZeroSamples = 0
        _rawSamples = 0
        _rawMaxAbs = 0
        _rawSumSquares = 0
        _startedAt = DispatchTime.now()
        _firstCallbackAt = nil
        _firstNonzeroAt = nil
    }

    /// Start keeping PCM on an already-running engine. Resets the buffer and
    /// the level statistics so the recording starts clean.
    func beginAccumulation() {
        lock.lock()
        samples.removeAll(keepingCapacity: true)
        _latestLevel = 0
        _latestVoiceActivity = .silence
        _peakAmplitude = 0
        _sumSquares = 0
        _converterErrorCount = 0
        _converterDroppedBufferCount = 0
        _voiceActiveFrames = 0
        _voiceTotalFrames = 0
        _firstSpeechSample = nil
        _lastSpeechAt = nil
        // Raw health must be measured for THIS recording, not carried over
        // from the warm idle period. Without this reset a warm reuse reported
        // the health of audio captured minutes earlier — which is exactly how
        // a backgrounded reuse that delivered near-silence (peak 0.0002)
        // still reported `capture_health: healthy` with a 3.85/s callback
        // rate inherited from the foreground recording before it.
        resetRawHealthLocked()
        accumulating = true
        lock.unlock()
        voiceActivityDetector.reset()
    }

    /// Stop keeping PCM and return what was captured, LEAVING the engine, the
    /// tap and the audio session alone.
    func endAccumulation() -> [Float] {
        lock.lock()
        accumulating = false
        let captured = samples
        lock.unlock()
        return captured
    }

    /// Stop capture and return everything captured so far.
    @discardableResult
    func stop() -> [Float] {
        lifecycleLock.lock()
        // Seal the capture at one lock-linearization point before stopping the
        // graph. A tap callback that arrives after this point may still finish
        // its raw health work, but it cannot append PCM or notify downstream
        // decoders against the snapshot being returned.
        accumulationGate.lock()
        lock.lock()
        accumulating = false
        // Invalidate callbacks queued from this graph before releasing the
        // seal. A later start gets a new generation and cannot inherit PCM
        // from this tap.
        _activeTapGeneration = _engineGeneration &+ 1
        let captured = samples
        lock.unlock()
        accumulationGate.unlock()
        if isRunning {
            removeEngineConfigurationObserverLocked()
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
            isRunning = false
        }
        _engineGeneration &+= 1
        removeEngineConfigurationObserverLocked()
        lifecycleLock.unlock()
        return captured
    }

    /// The hardware input format the running engine bound to, and the fixed
    /// conversion target. Content-free numbers only. Thread-safe.
    var boundInputSampleRate: Double {
        lock.lock(); defer { lock.unlock() }
        return _boundInputSampleRate
    }

    var boundInputChannelCount: UInt32 {
        lock.lock(); defer { lock.unlock() }
        return _boundInputChannelCount
    }

    var conversionTargetSampleRate: Double { Self.targetSampleRate }

    /// Latest normalized mic level (0…1) for meters. Thread-safe.
    var latestLevel: Double {
        lock.lock(); defer { lock.unlock() }
        return _latestLevel
    }

    var firstSpeechSample: Int? {
        lock.lock(); defer { lock.unlock() }
        return _firstSpeechSample
    }

    var speechActivity: SpeechActivitySnapshot {
        lock.lock(); defer { lock.unlock() }
        return SpeechActivitySnapshot(
            firstSpeechSample: _firstSpeechSample,
            lastSpeechAt: _lastSpeechAt
        )
    }

    var sampleCount: Int {
        lock.lock(); defer { lock.unlock() }
        return samples.count
    }

    /// Peak absolute amplitude and RMS across the whole capture (0…1), plus
    /// the sample count they were computed over. Thread-safe.
    var captureLevelSummary: (peak: Float, rms: Double, sampleCount: Int) {
        lock.lock(); defer { lock.unlock() }
        let count = samples.count
        let rms = count > 0 ? (_sumSquares / Double(count)).squareRoot() : 0
        return (_peakAmplitude, rms, count)
    }

    /// Where a signal collapse appears: the converter's own failures, and how
    /// much of the capture the VAD considered speech. Content-free counts.
    var conversionSummary: [String: Any] {
        lock.lock(); defer { lock.unlock() }
        return [
            "converter_error_count": _converterErrorCount,
            "converter_dropped_buffers": _converterDroppedBufferCount,
            "vad_active_fraction": _voiceTotalFrames > 0
                ? Double(_voiceActiveFrames) / Double(_voiceTotalFrames)
                : 0,
            "vad_frames": _voiceTotalFrames,
        ]
    }

    /// Snapshot only samples after an already-checkpointed cursor. Copying is
    /// bounded to one checkpoint interval and holds the lock briefly.
    func samples(from index: Int) -> [Float] {
        lock.lock(); defer { lock.unlock() }
        let start = min(max(index, 0), samples.count)
        return Array(samples[start...])
    }

    /// Measure the RAW tap buffer before anything touches it. Runs on the
    /// realtime thread, so it does only arithmetic under the lock.
    private func measureRaw(_ buffer: AVAudioPCMBuffer) {
        let now = DispatchTime.now()
        let frames = Int(buffer.frameLength)
        var maxAbs: Float = 0
        var zeros = 0
        var sumSquares: Double = 0
        var counted = 0
        if let channels = buffer.floatChannelData {
            for channel in 0..<Int(buffer.format.channelCount) {
                let samples = UnsafeBufferPointer(start: channels[channel], count: frames)
                for value in samples {
                    let magnitude = abs(value)
                    if magnitude > maxAbs { maxAbs = magnitude }
                    if value == 0 { zeros += 1 }
                    sumSquares += Double(value) * Double(value)
                    counted += 1
                }
            }
        }
        lock.lock()
        _rawCallbackCount += 1
        _rawFrameCount += frames
        _rawSamples += counted
        _rawZeroSamples += zeros
        _rawSumSquares += sumSquares
        if maxAbs > _rawMaxAbs { _rawMaxAbs = maxAbs }
        if _firstCallbackAt == nil { _firstCallbackAt = now }
        if _firstNonzeroAt == nil, maxAbs > 0 { _firstNonzeroAt = now }
        lock.unlock()
    }

    private func consumeTapBuffer(_ buffer: AVAudioPCMBuffer, generation: UInt64) {
        accumulationGate.lock()
        defer { accumulationGate.unlock() }
        lock.lock()
        let isCurrentGeneration = generation == _activeTapGeneration
        lock.unlock()
        guard isCurrentGeneration else { return }
        measureRaw(buffer)
        ingest(buffer)
    }

    /// Snapshot of the raw input path. Thread-safe.
    var rawHealth: RawHealth {
        lock.lock(); defer { lock.unlock() }
        func elapsedMs(_ instant: DispatchTime?) -> Int {
            guard let instant, let startedAt = _startedAt,
                  instant.uptimeNanoseconds >= startedAt.uptimeNanoseconds else { return -1 }
            return Int((instant.uptimeNanoseconds - startedAt.uptimeNanoseconds) / 1_000_000)
        }
        return RawHealth(
            callbackCount: _rawCallbackCount,
            frameCount: _rawFrameCount,
            zeroSampleFraction: _rawSamples > 0
                ? Double(_rawZeroSamples) / Double(_rawSamples)
                : 1,
            maxAbs: _rawMaxAbs,
            rms: _rawSamples > 0 ? (_rawSumSquares / Double(_rawSamples)).squareRoot() : 0,
            firstCallbackMs: elapsedMs(_firstCallbackAt),
            firstNonzeroMs: elapsedMs(_firstNonzeroAt),
            inputSampleRate: _boundInputSampleRate,
            inputChannelCount: _boundInputChannelCount,
            elapsedMs: elapsedMs(DispatchTime.now())
        )
    }

    private func ingest(_ input: AVAudioPCMBuffer) {
        guard let converter else { return }
        let ratio = outputFormat.sampleRate / input.format.sampleRate
        let capacity = AVAudioFrameCount(Double(input.frameLength) * ratio) + 1024
        guard let output = AVAudioPCMBuffer(pcmFormat: outputFormat, frameCapacity: capacity) else { return }

        var provided = false
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) { _, inStatus in
            if provided {
                // Keep the converter alive for the next tap buffer; do not end
                // the stream, or resampling state would reset every callback.
                inStatus.pointee = .noDataNow
                return nil
            }
            provided = true
            inStatus.pointee = .haveData
            return input
        }
        if status == .error {
            lock.lock(); _converterErrorCount += 1; lock.unlock()
            return
        }
        guard let channel = output.floatChannelData?.pointee,
              output.frameLength > 0 else {
            // Converted to nothing — a silent drop is exactly the kind of
            // collapse that would otherwise be invisible between the raw tap
            // and the ASR.
            lock.lock(); _converterDroppedBufferCount += 1; lock.unlock()
            return
        }

        let count = Int(output.frameLength)
        let frame = UnsafeBufferPointer(start: channel, count: count)

        // RMS → dBFS → 0…1, matching the previous meter's feel.
        var sumSquares: Float = 0
        var peak: Float = 0
        for value in frame {
            sumSquares += value * value
            peak = max(peak, abs(value))
        }
        let rms = (sumSquares / Float(count)).squareRoot()
        let db = rms > 0 ? 20 * log10(rms) : -60
        let level = Double(min(max((db + 60) / 60, 0), 1))
        let activity = voiceActivityDetector.ingest(rms: rms)
        let speechAt = DispatchTime.now()

        let copiedFrame = Array(frame)
        lock.lock()
        let keeping = accumulating
        if keeping {
            if activity == .speechStarted, _firstSpeechSample == nil {
                _firstSpeechSample = samples.count
            }
            if activity != .silence { _lastSpeechAt = speechAt }
            samples.append(contentsOf: copiedFrame)
            _peakAmplitude = max(_peakAmplitude, peak)
            _sumSquares += Double(sumSquares)
        }
        // Levels keep updating while idle so a warm engine can still be seen
        // to be alive; only the retained audio is gated.
        _latestLevel = level
        _latestVoiceActivity = activity
        if keeping {
            _voiceTotalFrames += 1
            if activity != .silence { _voiceActiveFrames += 1 }
        }
        lock.unlock()
        guard keeping else { return }
        pcmObserver?(copiedFrame)
    }
}

/// Writes mono-16k float samples to a complete, self-contained 16-bit PCM WAV
/// synchronously. Unlike `AVAudioRecorder`, `Data.write(to:)` returns only once
/// every byte is on disk, so a later read cannot race an async finalize. Used
/// only to persist a capture for the retry / insert-previous path.
enum PCMWavWriter {
    static func write(samples: [Float], sampleRate: Int, to url: URL) throws {
        let channels = 1
        let bitsPerSample = 16
        let byteRate = sampleRate * channels * bitsPerSample / 8
        let blockAlign = channels * bitsPerSample / 8
        let dataBytes = samples.count * 2

        var data = Data(capacity: 44 + dataBytes)
        func appendUInt32(_ value: UInt32) { var v = value.littleEndian; withUnsafeBytes(of: &v) { data.append(contentsOf: $0) } }
        func appendUInt16(_ value: UInt16) { var v = value.littleEndian; withUnsafeBytes(of: &v) { data.append(contentsOf: $0) } }

        data.append(contentsOf: Array("RIFF".utf8))
        appendUInt32(UInt32(36 + dataBytes))
        data.append(contentsOf: Array("WAVE".utf8))
        data.append(contentsOf: Array("fmt ".utf8))
        appendUInt32(16)                       // PCM fmt chunk size
        appendUInt16(1)                        // PCM
        appendUInt16(UInt16(channels))
        appendUInt32(UInt32(sampleRate))
        appendUInt32(UInt32(byteRate))
        appendUInt16(UInt16(blockAlign))
        appendUInt16(UInt16(bitsPerSample))
        data.append(contentsOf: Array("data".utf8))
        appendUInt32(UInt32(dataBytes))
        // Bulk conversion — a per-sample `data.append` costs ~9.6M calls on a
        // 5-minute capture; one map + one byte copy is a single pass.
        let pcm = samples.map { Int16(max(-1, min(1, $0)) * 32767).littleEndian }
        pcm.withUnsafeBytes { data.append(contentsOf: $0) }
        try data.write(to: url, options: .atomic)
    }
}
