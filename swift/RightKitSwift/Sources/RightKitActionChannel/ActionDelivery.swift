#if os(macOS)
import AppKit
import Foundation

/// Why an action could not be delivered within its deadline.
public enum ActionDeliveryError: Error, Sendable {
    /// The app answered and said no (for example it did not recognise this sender). Not retried.
    case refused(ActionChannelError)
    /// The deadline passed; `last` is the failure of the final attempt.
    case timedOut(last: ActionChannelError)

    /// The text a product shows. `appName` is the product's name.
    public func describe(appName: String) -> String {
        switch self {
        case let .refused(error): return error.describe(appName: appName)
        case let .timedOut(last): return "\(appName) did not accept the command in time (\(last.describe(appName: appName)))"
        }
    }
}

public struct ActionDeliveryConfiguration: Sendable {
    /// The stem the app listens on (`<per-user temp dir>/<socketName>.sock`).
    public var socketName: String
    /// Code requirement the listening process must satisfy (see ``actionChannelRequirement(teamID:identifier:)``).
    public var serverRequirement: String
    /// The app to launch (without activating it) when nothing is listening. `nil` never launches.
    public var launchBundleIdentifier: String?
    /// How long one delivery waits for the app to come up and accept it.
    public var deadline: TimeInterval
    /// Pause between attempts.
    public var retryInterval: TimeInterval

    public init(
        socketName: String,
        serverRequirement: String,
        launchBundleIdentifier: String? = nil,
        deadline: TimeInterval = 30,
        retryInterval: TimeInterval = 0.25
    ) {
        self.socketName = socketName
        self.serverRequirement = serverRequirement
        self.launchBundleIdentifier = launchBundleIdentifier
        self.deadline = deadline
        self.retryInterval = retryInterval
    }
}

/// Cold-start delivery for a short-lived sender (an App Intents helper): launch the app by its stable
/// bundle identifier if it is not running, keep retrying the connection in memory until the app's
/// listener accepts it or the deadline passes. Nothing is persisted: the command reaches the app only
/// through a connection the app itself verified, never through a file it must trust.
public enum ActionDelivery {
    /// Deliver `action` once. One frame id covers every retry, so the app can tell a resend from a new
    /// command. Throws ``ActionDeliveryError``; a cancelled task throws `CancellationError`.
    public static func submit(_ action: String, configuration: ActionDeliveryConfiguration) async throws {
        let connection = ActionChannelConnection(
            socketName: configuration.socketName,
            serverRequirement: configuration.serverRequirement
        )
        defer { connection.close() }
        let deadline = Date().addingTimeInterval(configuration.deadline)
        var last: ActionChannelError = .unavailable
        let intentID = UUID().uuidString.lowercased()
        while true {
            do {
                try await attempt(connection, action, id: intentID)
                return
            } catch let error as ActionChannelError {
                switch error {
                case .refused:
                    throw ActionDeliveryError.refused(error)
                case .unavailable:
                    if let bundleIdentifier = configuration.launchBundleIdentifier {
                        launchIfNeeded(bundleIdentifier: bundleIdentifier)
                    }
                case .serverNotVerified, .io:
                    break
                }
                last = error
            }
            if Date() >= deadline { throw ActionDeliveryError.timedOut(last: last) }
            try await Task.sleep(nanoseconds: UInt64(configuration.retryInterval * 1_000_000_000))
        }
    }

    /// Launch the app by its bundle identifier if it is not running, hidden and without activating it.
    /// The launched bundle is untrusted until it proves itself: the channel verifies the listener's
    /// signature before any command is sent, so an impostor bundle receives nothing.
    public static func launchIfNeeded(bundleIdentifier: String) {
        let running = NSRunningApplication.runningApplications(withBundleIdentifier: bundleIdentifier)
            .contains { !$0.isTerminated }
        guard !running,
              let appURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleIdentifier)
        else { return }
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = false
        configuration.hides = true
        NSWorkspace.shared.openApplication(at: appURL, configuration: configuration)
    }

    private static func attempt(_ connection: ActionChannelConnection, _ action: String, id: String) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    try connection.send(action, id: id)
                    continuation.resume()
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
}
#endif
