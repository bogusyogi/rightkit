import CoreFoundation
import Foundation
#if canImport(AVFAudio)
import AVFAudio
#endif

/// Product code owns recording policy; this protocol is the narrow bridge App Intents and
/// extensions use to reach the app's active recording session.
@MainActor
public protocol RightKitRecordingDriver: AnyObject {
    var processHandle: String { get }
    var isRecording: Bool { get }
    func start(source: String) async throws
    func stopAndCopy(source: String) async throws -> String
    func cancel(source: String) async throws
    func toggle(source: String) async throws
    /// Returns the latest transcript text for the caller to place on the pasteboard. Drivers that
    /// keep no transcript history inherit the default, which fails explicitly rather than succeed silently.
    func copyLatestTranscript(source: String) async throws -> String
}

public extension RightKitRecordingDriver {
    func copyLatestTranscript(source: String) async throws -> String {
        throw RightKitAppIntentError.actionUnsupported(RightKitRecordingAction.copyLatestTranscript.rawValue)
    }
}

public enum RightKitRecordingAction: String, Codable, Equatable, Sendable {
    case start, stopAndCopy, cancel, toggle, copyLatestTranscript, openApp
}

public enum RightKitAppIntentError: Error, Equatable, LocalizedError, Sendable {
    case driverUnavailable
    case commandRejected
    case microphonePermissionUndetermined
    case microphonePermissionDenied
    case notRecording
    case commandResultTimedOut
    case commandResultFailed(String)
    case commandResultCancelled
    case actionUnsupported(String)

    public var errorDescription: String? {
        switch self {
        case .driverUnavailable: return "Recording app is unavailable."
        case .commandRejected: return "Recording action could not be queued."
        case .microphonePermissionUndetermined: return "Microphone permission needs foreground confirmation."
        case .microphonePermissionDenied: return "Microphone permission is required for recording."
        case .notRecording: return "No recording is active."
        case .commandResultTimedOut: return "Recording app did not finish in time."
        case let .commandResultFailed(code): return "Recording action failed (\(code))."
        case .commandResultCancelled: return "Recording action was cancelled."
        case let .actionUnsupported(action): return "Recording action \(action) is not supported by this app."
        }
    }
}

/// Stable Darwin notification names used by an extension to wake its containing app after a
/// command is written to an app-group queue. Darwin notifications carry no user data.
public enum RightKitDarwinNotification {
    public static let recordingAction = "org.rightkit.recording-action"

    public static func post(_ name: String = recordingAction) {
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName(name as CFString), nil, nil, true
        )
    }
}

public enum RightKitMicrophonePermission: String, Equatable, Sendable {
    case undetermined, granted, denied
}

/// Permission is deliberately a handshake owned by the app process. An extension can request a
/// foreground continuation when iOS has not asked yet, but it cannot manufacture permission.
@MainActor
public enum RightKitMicrophonePermissionHandshake {
    public static func status() -> RightKitMicrophonePermission {
        #if canImport(AVFAudio)
        switch AVAudioApplication.shared.recordPermission {
        case .granted: return .granted
        case .denied: return .denied
        case .undetermined: return .undetermined
        @unknown default: return .denied
        }
        #else
        return .granted
        #endif
    }

    public static func request() async throws {
        switch status() {
        case .granted: return
        case .denied: throw RightKitAppIntentError.microphonePermissionDenied
        case .undetermined:
            #if canImport(AVFAudio)
            let granted = await withCheckedContinuation { continuation in
                AVAudioApplication.requestRecordPermission { continuation.resume(returning: $0) }
            }
            guard granted else { throw RightKitAppIntentError.microphonePermissionDenied }
            #else
            throw RightKitAppIntentError.microphonePermissionUndetermined
            #endif
        }
    }
}

/// Main-app composition root installs its driver exactly once. Extension processes leave `driver`
/// nil and enqueue through their own app-group adapter before posting `RightKitDarwinNotification`.
@MainActor
public enum RightKitRecordingBridge {
    public static var driver: (any RightKitRecordingDriver)?
    public static var currentProcessHandle: String {
        "\(ProcessInfo.processInfo.processName)#\(ProcessInfo.processInfo.processIdentifier)"
    }

    public static func dispatch(
        _ action: RightKitRecordingAction,
        source: String = "app-intent",
        enqueue: (() throws -> Void)? = nil,
    ) async throws -> String? {
        if let driver {
            switch action {
            case .start: try await driver.start(source: source)
            case .stopAndCopy: return try await driver.stopAndCopy(source: source)
            case .cancel: try await driver.cancel(source: source)
            case .toggle: try await driver.toggle(source: source)
            case .copyLatestTranscript: return try await driver.copyLatestTranscript(source: source)
            case .openApp: throw RightKitAppIntentError.actionUnsupported(action.rawValue)
            }
            return nil
        }
        guard let enqueue else { throw RightKitAppIntentError.driverUnavailable }
        try enqueue()
        RightKitDarwinNotification.post()
        return nil
    }
}
