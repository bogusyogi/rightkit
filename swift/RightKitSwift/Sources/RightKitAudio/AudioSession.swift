import Foundation
@preconcurrency import AVFoundation

/// The category, mode and options a caller wants the shared `AVAudioSession` in. A plain value, so
/// callers choose them and the capture and recorder types never hard-code a session policy. On macOS,
/// which has no audio session, applying a configuration does nothing.
public struct AudioSessionConfiguration: Sendable, Equatable {
    public enum Category: Sendable, Equatable {
        case record
        case playAndRecord
        case playback
        case ambient
    }

    public enum Mode: Sendable, Equatable {
        case `default`
        case measurement
        case spokenAudio
        case voiceChat
        case videoRecording
    }

    public struct Options: OptionSet, Sendable, Equatable {
        public let rawValue: Int
        public init(rawValue: Int) { self.rawValue = rawValue }

        public static let mixWithOthers = Options(rawValue: 1 << 0)
        public static let duckOthers = Options(rawValue: 1 << 1)
        /// iOS only.
        public static let defaultToSpeaker = Options(rawValue: 1 << 2)
        /// Bluetooth hands-free profile input. iOS only.
        public static let allowBluetoothHFP = Options(rawValue: 1 << 3)
        /// iOS only.
        public static let allowBluetoothA2DP = Options(rawValue: 1 << 4)
        /// iOS only.
        public static let interruptSpokenAudioAndMixWithOthers = Options(rawValue: 1 << 5)
    }

    public var category: Category
    public var mode: Mode
    public var options: Options
    /// Best-effort hint; failures to apply it are ignored. iOS only.
    public var preferredSampleRate: Double?
    /// Best-effort hint; failures to apply it are ignored. iOS only.
    public var preferredInputChannels: Int?
    /// Passed as `.notifyOthersOnDeactivation` when the session is released.
    public var notifyOthersOnDeactivation: Bool

    public init(
        category: Category,
        mode: Mode = .default,
        options: Options = [],
        preferredSampleRate: Double? = nil,
        preferredInputChannels: Int? = nil,
        notifyOthersOnDeactivation: Bool = true,
    ) {
        self.category = category
        self.mode = mode
        self.options = options
        self.preferredSampleRate = preferredSampleRate
        self.preferredInputChannels = preferredInputChannels
        self.notifyOthersOnDeactivation = notifyOthersOnDeactivation
    }

    /// Record-only, measurement mode: what speech recognition wants.
    public static let recordOnly = AudioSessionConfiguration(category: .record, mode: .measurement)
    /// Spoken-audio playback that ducks other audio: what text-to-speech wants.
    public static let spokenPlayback = AudioSessionConfiguration(
        category: .playback, mode: .spokenAudio, options: [.duckOthers])
}

public enum AudioSessionError: Error, Equatable, Sendable, LocalizedError {
    /// The system answered the activation request with "not activated".
    case activationRefused
    case configurationFailed(String)

    public var errorDescription: String? {
        switch self {
        case .activationRefused: return "The system did not activate the audio session."
        case let .configurationFailed(reason): return "Could not configure the audio session: \(reason)"
        }
    }
}

/// Configures, activates and releases the shared audio session with the API each platform wants:
/// `setActive` on iOS, the asynchronous `activate`/`deactivate` on watchOS 27 and later (`setActive`
/// before that). A no-op on macOS.
public enum AudioSessionController {
    #if os(iOS) || os(watchOS) || os(tvOS)
    private static func apply(_ configuration: AudioSessionConfiguration, to session: AVAudioSession) throws {
        let category: AVAudioSession.Category
        switch configuration.category {
        case .record: category = .record
        case .playAndRecord: category = .playAndRecord
        case .playback: category = .playback
        case .ambient: category = .ambient
        }
        let mode: AVAudioSession.Mode
        switch configuration.mode {
        case .default: mode = .default
        case .measurement: mode = .measurement
        case .spokenAudio: mode = .spokenAudio
        case .voiceChat: mode = .voiceChat
        case .videoRecording: mode = .videoRecording
        }
        var options: AVAudioSession.CategoryOptions = []
        if configuration.options.contains(.mixWithOthers) { options.insert(.mixWithOthers) }
        if configuration.options.contains(.duckOthers) { options.insert(.duckOthers) }
        #if os(iOS)
        if configuration.options.contains(.defaultToSpeaker) { options.insert(.defaultToSpeaker) }
        if configuration.options.contains(.allowBluetoothHFP) { options.insert(.allowBluetoothHFP) }
        if configuration.options.contains(.allowBluetoothA2DP) { options.insert(.allowBluetoothA2DP) }
        if configuration.options.contains(.interruptSpokenAudioAndMixWithOthers) {
            options.insert(.interruptSpokenAudioAndMixWithOthers)
        }
        #endif
        try session.setCategory(category, mode: mode, options: options)
        #if os(iOS)
        if let rate = configuration.preferredSampleRate { try? session.setPreferredSampleRate(rate) }
        if let channels = configuration.preferredInputChannels { try? session.setPreferredInputNumberOfChannels(channels) }
        #endif
    }
    #endif

    /// Sets the category and activates the session.
    public static func activate(_ configuration: AudioSessionConfiguration) async throws {
        #if os(iOS) || os(tvOS)
        let session = AVAudioSession.sharedInstance()
        try apply(configuration, to: session)
        try session.setActive(true)
        #elseif os(watchOS)
        let session = AVAudioSession.sharedInstance()
        try apply(configuration, to: session)
        if #available(watchOS 27.0, *) {
            guard try await session.activate(options: []) else { throw AudioSessionError.activationRefused }
        } else {
            try session.setActive(true)
        }
        #endif
    }

    /// Releases the session. Failures are swallowed: a session that cannot be released has nothing a
    /// caller can do about it, and cleanup must not mask the error that caused it.
    public static func deactivate(notifyOthers: Bool = true) async {
        #if os(iOS) || os(tvOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: notifyOthers ? [.notifyOthersOnDeactivation] : [])
        #elseif os(watchOS)
        let session = AVAudioSession.sharedInstance()
        if #available(watchOS 27.0, *) {
            _ = try? await session.deactivate(options: notifyOthers ? [.notifyOthersOnDeactivation] : [])
        } else {
            try? session.setActive(false, options: notifyOthers ? [.notifyOthersOnDeactivation] : [])
        }
        #endif
    }
}

/// Record permission for the microphone, one API on iOS, watchOS and macOS.
public enum MicrophonePermission {
    public enum State: Sendable, Equatable {
        case undetermined
        case granted
        case denied
    }

    /// The current state; never prompts.
    public static var current: State {
        #if os(iOS) || os(watchOS) || os(tvOS)
        switch AVAudioApplication.shared.recordPermission {
        case .granted: return .granted
        case .denied: return .denied
        case .undetermined: return .undetermined
        @unknown default: return .denied
        }
        #elseif os(macOS)
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: return .granted
        case .notDetermined: return .undetermined
        default: return .denied
        }
        #else
        return .denied
        #endif
    }

    /// Prompts only when the state is undetermined; returns whether access is granted.
    public static func request() async -> Bool {
        switch current {
        case .granted: return true
        case .denied: return false
        case .undetermined:
            #if os(iOS) || os(watchOS) || os(tvOS)
            return await AVAudioApplication.requestRecordPermission()
            #elseif os(macOS)
            return await AVCaptureDevice.requestAccess(for: .audio)
            #else
            return false
            #endif
        }
    }
}
