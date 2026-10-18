import Foundation
@preconcurrency import AVFoundation

/// File format for ``AudioFileRecorder``. The default is what speech recognition wants: 16 kHz mono
/// 16-bit linear PCM.
public struct AudioFileRecorderSettings: Sendable, Equatable {
    public enum Encoding: Sendable, Equatable {
        /// 16-bit little-endian signed integer linear PCM.
        case linearPCM16
        /// MPEG-4 AAC at `bitRate` bits per second (small files, for example watch-to-phone transfer).
        case aac(bitRate: Int)
    }

    public var sampleRate: Double
    public var channels: Int
    public var encoding: Encoding

    public init(sampleRate: Double = 16_000, channels: Int = 1, encoding: Encoding = .linearPCM16) {
        self.sampleRate = sampleRate
        self.channels = channels
        self.encoding = encoding
    }

    /// The `AVAudioRecorder` settings dictionary for this format.
    public var avSettings: [String: Any] {
        switch encoding {
        case .linearPCM16:
            return [
                AVFormatIDKey: Int(kAudioFormatLinearPCM),
                AVLinearPCMBitDepthKey: 16,
                AVLinearPCMIsBigEndianKey: false,
                AVLinearPCMIsFloatKey: false,
                AVSampleRateKey: sampleRate,
                AVNumberOfChannelsKey: channels,
            ]
        case let .aac(bitRate):
            return [
                AVFormatIDKey: Int(kAudioFormatMPEG4AAC),
                AVSampleRateKey: sampleRate,
                AVNumberOfChannelsKey: channels,
                AVEncoderBitRateKey: bitRate,
                AVEncoderAudioQualityKey: AVAudioQuality.high.rawValue,
            ]
        }
    }
}

/// A finished recording on disk.
public struct AudioRecording: Sendable, Equatable {
    public let url: URL
    public let duration: TimeInterval
    public let byteCount: Int64

    public init(url: URL, duration: TimeInterval, byteCount: Int64) {
        self.url = url
        self.duration = duration
        self.byteCount = byteCount
    }
}

public enum AudioFileRecorderError: Error, Sendable, Equatable, LocalizedError {
    case permissionDenied
    case alreadyRecording
    case startFailed
    case sessionFailed(String)

    public var errorDescription: String? {
        switch self {
        case .permissionDenied: return "Microphone permission is required."
        case .alreadyRecording: return "A recording is already in progress."
        case .startFailed: return "Recording could not start."
        case let .sessionFailed(reason): return "The audio session could not be activated: \(reason)"
        }
    }
}

/// What a recording owner needs to hear about while a recording runs.
public enum AudioFileRecorderEvent: Sendable, Equatable {
    /// The system paused the recording (``AudioFileRecorder/Configuration/interruptionPolicy`` is `.pauseAndResume`).
    case paused
    /// The recording resumed after an interruption.
    case resumed
    /// The recording ended without a `stop()` call: an interruption under `.stopAndKeepFile`, a lost input
    /// route, a media-services reset, the duration cap, or an encoder error. `recording` is the file as
    /// finalized (nil if nothing usable remained). The recorder is idle again.
    case endedBySystem(reason: EndReason, recording: AudioRecording?)

    public enum EndReason: Sendable, Equatable {
        case interruption
        case inputRouteLost
        case mediaServicesReset
        case maximumDuration
        case encoderError
    }
}

/// Records microphone audio to a file with metering, session activation, record permission and
/// interruption handling, on iOS, watchOS and macOS (``AudioFileRecorderSettings`` default: 16 kHz mono
/// 16-bit PCM). Built on `AVAudioRecorder`, the one file recorder available on watchOS.
///
/// `stop()` returns only after the recorder reports the file finalized (bounded wait), so a caller that
/// reads the file right away does not race Core Audio's asynchronous header finalization.
public final class AudioFileRecorder: @unchecked Sendable {
    public enum State: Sendable, Equatable {
        case idle
        /// Permission and session activation are in flight.
        case starting
        case recording
        /// Paused by an interruption; waiting for the system to allow resuming.
        case paused
    }

    public enum InterruptionPolicy: Sendable, Equatable {
        /// An interruption ends the recording and keeps the file. The watch default: it cannot assume the
        /// user is still there when the call ends.
        case stopAndKeepFile
        /// An interruption pauses the recording; when the system recommends resuming, the same file continues.
        case pauseAndResume
    }

    public struct Configuration: Sendable {
        public var settings: AudioFileRecorderSettings
        /// The session to activate before recording; nil leaves the session to the caller.
        public var session: AudioSessionConfiguration?
        public var interruptionPolicy: InterruptionPolicy
        /// Ends the recording on its own after this long.
        public var maximumDuration: TimeInterval?
        /// Ask for record permission in `start(to:)` when it is undetermined.
        public var requestsPermission: Bool
        /// How long `stop()` waits for the file to finalize.
        public var finalizeTimeout: TimeInterval

        public init(
            settings: AudioFileRecorderSettings = AudioFileRecorderSettings(),
            session: AudioSessionConfiguration? = AudioSessionConfiguration(category: .record),
            interruptionPolicy: InterruptionPolicy = .stopAndKeepFile,
            maximumDuration: TimeInterval? = nil,
            requestsPermission: Bool = true,
            finalizeTimeout: TimeInterval = 1.0,
        ) {
            self.settings = settings
            self.session = session
            self.interruptionPolicy = interruptionPolicy
            self.maximumDuration = maximumDuration
            self.requestsPermission = requestsPermission
            self.finalizeTimeout = finalizeTimeout
        }
    }

    // MARK: Seams (internal; tests substitute them)

    typealias BackendFactory = @Sendable (URL, [String: Any]) throws -> any RecorderBackend
    typealias PermissionRequest = @Sendable () async -> Bool
    typealias SessionActivation = @Sendable (AudioSessionConfiguration) async throws -> Void
    typealias SessionRelease = @Sendable (Bool) async -> Void

    public let configuration: Configuration

    private let makeBackend: BackendFactory
    private let requestPermission: PermissionRequest
    private let activateSession: SessionActivation
    private let releaseSession: SessionRelease
    private let makeMonitor: @Sendable (@escaping @Sendable (AudioSessionEvent) -> Void) -> AudioSessionMonitor

    private let lock = NSLock()
    private var _state: State = .idle
    private var backend: (any RecorderBackend)?
    private var finish: FinishSignal?
    private var monitor: AudioSessionMonitor?
    private var sessionPump: Task<Void, Never>?
    /// Bumped by every start and every end, so a late event or finish for an older recording is ignored.
    private var token = 0

    private let eventContinuation: AsyncStream<AudioFileRecorderEvent>.Continuation
    /// Lifecycle events. Single consumer; buffered until read.
    public let events: AsyncStream<AudioFileRecorderEvent>

    public convenience init(configuration: Configuration = Configuration()) {
        self.init(
            configuration: configuration,
            makeBackend: { url, settings in try AVAudioRecorderBackend(url: url, settings: settings) },
            requestPermission: { await MicrophonePermission.request() },
            activateSession: { try await AudioSessionController.activate($0) },
            releaseSession: { await AudioSessionController.deactivate(notifyOthers: $0) },
            makeMonitor: { AudioSessionMonitor(handler: $0) },
        )
    }

    init(
        configuration: Configuration,
        makeBackend: @escaping BackendFactory,
        requestPermission: @escaping PermissionRequest,
        activateSession: @escaping SessionActivation,
        releaseSession: @escaping SessionRelease,
        makeMonitor: @escaping @Sendable (@escaping @Sendable (AudioSessionEvent) -> Void) -> AudioSessionMonitor,
    ) {
        self.configuration = configuration
        self.makeBackend = makeBackend
        self.requestPermission = requestPermission
        self.activateSession = activateSession
        self.releaseSession = releaseSession
        self.makeMonitor = makeMonitor
        let (stream, continuation) = AsyncStream.makeStream(of: AudioFileRecorderEvent.self)
        events = stream
        eventContinuation = continuation
    }

    deinit {
        eventContinuation.finish()
        sessionPump?.cancel()
        monitor?.stop()
    }

    // MARK: Observation

    public var state: State { lock.withLock { _state } }
    public var isRecording: Bool { state == .recording }

    /// Seconds recorded so far (0 when idle).
    public var currentTime: TimeInterval { lock.withLock { backend?.currentTime ?? 0 } }

    /// Current input level, 0...1 (-60 dBFS or lower is 0). Poll it while recording; 0 when idle.
    public var meterLevel: Double {
        guard let power = lock.withLock({ backend?.averagePowerDB() }) else { return 0 }
        return Self.level(powerDB: power)
    }

    /// Maps an average power in dBFS to a 0...1 meter value.
    public static func level(powerDB: Float) -> Double {
        let clamped = max(-60, min(0, Double(powerDB)))
        return (clamped + 60) / 60
    }

    // MARK: Recording

    /// Requests permission if needed, activates the session, and starts recording to `url`. Any failure
    /// leaves the recorder idle with the session released.
    public func start(to url: URL) async throws {
        let myToken = try lock.withLock { () -> Int in
            guard _state == .idle else { throw AudioFileRecorderError.alreadyRecording }
            _state = .starting
            token += 1
            return token
        }
        do {
            if configuration.requestsPermission {
                guard await requestPermission() else { throw AudioFileRecorderError.permissionDenied }
            }
            if let session = configuration.session {
                do { try await activateSession(session) } catch {
                    throw AudioFileRecorderError.sessionFailed(error.localizedDescription)
                }
            }
            let created = try makeBackend(url, configuration.settings.avSettings)
            guard created.prepare(), created.record(forDuration: configuration.maximumDuration) else {
                throw AudioFileRecorderError.startFailed
            }
            let signal = FinishSignal()
            created.onFinish = { [weak self, signal] success in
                signal.fire()
                self?.recorderFinished(token: myToken, success: success)
            }
            let (stream, continuation) = AsyncStream.makeStream(of: AudioSessionEvent.self)
            let sessionMonitor = makeMonitor { continuation.yield($0) }
            sessionMonitor.start()
            let pump = Task { [weak self] in
                for await event in stream {
                    await self?.handle(event, token: myToken)
                }
            }
            let adopted = lock.withLock { () -> Bool in
                guard token == myToken, _state == .starting else { return false }
                backend = created
                finish = signal
                monitor = sessionMonitor
                sessionPump = pump
                _state = .recording
                return true
            }
            if !adopted {
                pump.cancel()
                continuation.finish()
                sessionMonitor.stop()
                created.onFinish = nil
                created.stop()
                throw CancellationError()
            }
        } catch {
            let owned = lock.withLock { () -> Bool in
                guard token == myToken else { return false }
                _state = .idle
                return true
            }
            if owned, configuration.session != nil { await releaseSession(configuration.session?.notifyOthersOnDeactivation ?? true) }
            throw error
        }
    }

    /// Stops the recording and returns the finalized file, or nil when nothing was recording (or the file is empty).
    @discardableResult
    public func stop() async -> AudioRecording? {
        await end(discard: false)
    }

    /// Stops the recording and deletes the file.
    public func cancel() async {
        _ = await end(discard: true)
    }

    // MARK: Internals

    /// Ends the current recording (if any) and returns its finalized file.
    private func end(discard: Bool) async -> AudioRecording? {
        let taken = lock.withLock { () -> (any RecorderBackend, FinishSignal?, AudioSessionMonitor?, Task<Void, Never>?)? in
            guard let backend, _state == .recording || _state == .paused else { return nil }
            defer {
                self.backend = nil
                finish = nil
                monitor = nil
                sessionPump = nil
                _state = .idle
                token += 1
            }
            return (backend, finish, monitor, sessionPump)
        }
        guard let (recorder, signal, sessionMonitor, pump) = taken else { return nil }
        pump?.cancel()
        sessionMonitor?.stop()
        let duration = recorder.currentTime
        let url = recorder.url
        recorder.stop()
        if let signal { await signal.wait(timeout: configuration.finalizeTimeout) }
        recorder.onFinish = nil
        if configuration.session != nil { await releaseSession(configuration.session?.notifyOthersOnDeactivation ?? true) }
        if discard {
            try? FileManager.default.removeItem(at: url)
            return nil
        }
        let size = ((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)?.int64Value ?? 0
        guard size > 0 else { return nil }
        return AudioRecording(url: url, duration: duration, byteCount: size)
    }

    private func recorderFinished(token finishedToken: Int, success: Bool) {
        // A stop() the caller asked for cleared the token already; only unrequested ends reach here.
        let current = lock.withLock { token == finishedToken && (_state == .recording) }
        guard current else { return }
        Task { [weak self] in
            guard let self else { return }
            let reason: AudioFileRecorderEvent.EndReason = success ? .maximumDuration : .encoderError
            let recording = await self.end(discard: false)
            self.eventContinuation.yield(.endedBySystem(reason: reason, recording: recording))
        }
    }

    func handle(_ event: AudioSessionEvent, token eventToken: Int) async {
        guard lock.withLock({ token == eventToken }) else { return }
        switch event {
        case .interruptionBegan:
            switch configuration.interruptionPolicy {
            case .stopAndKeepFile:
                await endBySystem(.interruption)
            case .pauseAndResume:
                let paused = lock.withLock { () -> Bool in
                    guard _state == .recording else { return false }
                    backend?.pause()
                    _state = .paused
                    return true
                }
                if paused { eventContinuation.yield(.paused) }
            }
        case let .interruptionEnded(shouldResume):
            let isPaused = lock.withLock { _state == .paused }
            guard isPaused else { return }
            guard shouldResume else {
                await endBySystem(.interruption)
                return
            }
            if let session = configuration.session {
                do { try await activateSession(session) } catch {
                    await endBySystem(.interruption)
                    return
                }
            }
            let resumed = lock.withLock { () -> Bool in
                guard token == eventToken, _state == .paused, let backend else { return false }
                guard backend.resume() else { return false }
                _state = .recording
                return true
            }
            if resumed { eventContinuation.yield(.resumed) } else { await endBySystem(.interruption) }
        case let .routeChanged(reason):
            if reason.losesActiveInput { await endBySystem(.inputRouteLost) }
        case .mediaServicesLost, .mediaServicesReset:
            await endBySystem(.mediaServicesReset)
        }
    }

    private func endBySystem(_ reason: AudioFileRecorderEvent.EndReason) async {
        guard lock.withLock({ _state == .recording || _state == .paused }) else { return }
        let recording = await end(discard: false)
        eventContinuation.yield(.endedBySystem(reason: reason, recording: recording))
    }
}

// MARK: - Backend

/// The slice of `AVAudioRecorder` the recorder uses. Internal so tests can drive the state machine
/// without a microphone.
protocol RecorderBackend: AnyObject, Sendable {
    var url: URL { get }
    var currentTime: TimeInterval { get }
    /// Called once with `success` when the recorder finishes on its own or after `stop()`.
    var onFinish: (@Sendable (Bool) -> Void)? { get set }
    func prepare() -> Bool
    func record(forDuration: TimeInterval?) -> Bool
    func pause()
    func resume() -> Bool
    func stop()
    func averagePowerDB() -> Float
}

final class AVAudioRecorderBackend: NSObject, RecorderBackend, AVAudioRecorderDelegate, @unchecked Sendable {
    private let recorder: AVAudioRecorder
    private let lock = NSLock()
    private var _onFinish: (@Sendable (Bool) -> Void)?

    init(url: URL, settings: [String: Any]) throws {
        recorder = try AVAudioRecorder(url: url, settings: settings)
        super.init()
        recorder.delegate = self
        recorder.isMeteringEnabled = true
    }

    var url: URL { recorder.url }
    var currentTime: TimeInterval { recorder.currentTime }
    var onFinish: (@Sendable (Bool) -> Void)? {
        get { lock.withLock { _onFinish } }
        set { lock.withLock { _onFinish = newValue } }
    }

    func prepare() -> Bool { recorder.prepareToRecord() }

    func record(forDuration duration: TimeInterval?) -> Bool {
        if let duration { return recorder.record(forDuration: duration) }
        return recorder.record()
    }

    func pause() { recorder.pause() }
    func resume() -> Bool { recorder.record() }
    func stop() { recorder.stop() }

    func averagePowerDB() -> Float {
        recorder.updateMeters()
        return recorder.averagePower(forChannel: 0)
    }

    func audioRecorderDidFinishRecording(_ recorder: AVAudioRecorder, successfully flag: Bool) {
        onFinish?(flag)
    }

    func audioRecorderEncodeErrorDidOccur(_ recorder: AVAudioRecorder, error: Error?) {
        onFinish?(false)
    }
}

/// A one-shot flag the recorder's finish callback sets and `stop()` waits on, with a bound.
final class FinishSignal: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false

    func fire() { lock.withLock { done = true } }

    func wait(timeout: TimeInterval) async {
        let deadline = DispatchTime.now().uptimeNanoseconds + UInt64(max(0, timeout) * 1_000_000_000)
        while !lock.withLock({ done }), DispatchTime.now().uptimeNanoseconds < deadline {
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
    }
}
