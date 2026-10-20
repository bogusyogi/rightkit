import Foundation
#if canImport(LocalAuthentication)
import LocalAuthentication
#endif
#if canImport(Observation)
import Observation
#endif

// The generic half of an app lock (PK16-2, from Pulse's iOS `AppLock`; PK08-7 names the same
// "credentials OS-auth primitive first" for the desktop shell). The lock state machine and the mapping of
// the system verification prompt to a decision live here; the product keeps what is its own: whether
// the lock is on (its setting and default), the prompt wording, the lock screen, and the rule for what a
// failure says to the user.

/// What the person at the device did with the system "prove you are the owner" prompt, with the one
/// distinction an app lock needs that ``UserVerificationOutcome`` folds away: a device that has no
/// passcode at all.
public enum OwnerAuthenticationResult: Sendable, Equatable {
    case verified
    /// The user, the app or the system dismissed the prompt. Not a failure: the lock simply stays.
    case cancelled
    /// The prompt ended without verifying (wrong passcode or finger too often, lockout).
    case failed
    /// The device has no passcode, so there is nothing to unlock with. An app lock lets the owner
    /// in rather than lock them out of their own device (the platform offers no stronger answer).
    case noPasscodeSet
    /// The policy cannot be evaluated for another reason (the text is the system's description). The
    /// lock stays: an unknown state is never a pass.
    case unavailable(String)
}

/// Asks for the owner check. The system implementation is ``SystemOwnerAuthentication``; tests inject a fake.
public protocol OwnerAuthenticating: Sendable {
    func authenticate(reason: String) async -> OwnerAuthenticationResult
}

/// The biometry the device offers, for button and setting labels.
public enum BiometryKind: Sendable, Equatable {
    case faceID, touchID, opticID
    /// No biometry (or not readable here): the passcode is the method.
    case none

    /// Apple's own names ("Face ID", "Touch ID", "Optic ID", "Passcode").
    public var systemName: String {
        switch self {
        case .faceID: return "Face ID"
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        case .none: return "Passcode"
        }
    }
}

/// Why the lock could not be opened. The app words it.
public enum AppLockFailure: Sendable, Equatable {
    /// The device cannot evaluate owner authentication right now.
    case unavailable
    /// The prompt ended without verifying.
    case couldNotVerify
}

#if canImport(LocalAuthentication)
/// ``OwnerAuthenticating`` over LocalAuthentication: Face ID, Touch ID or Optic ID with the device
/// passcode as the fallback (`deviceOwnerAuthentication`).
public struct SystemOwnerAuthentication: OwnerAuthenticating {
    public init() {}

    /// The biometry this device offers. A status read: it never shows a prompt. watchOS has no readable
    /// biometry type, so it reports ``BiometryKind/none``.
    public static var biometryKind: BiometryKind {
        #if os(watchOS)
        return .none
        #else
        let context = LAContext()
        _ = context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: nil)
        switch context.biometryType {
        case .faceID: return .faceID
        case .touchID: return .touchID
        #if os(iOS) || os(macOS) || os(visionOS)
        case .opticID: return .opticID
        #endif
        default: return .none
        }
        #endif
    }

    public func authenticate(reason: String) async -> OwnerAuthenticationResult {
        let context = LAContext()
        var evaluation: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &evaluation) else {
            return Self.result(forUnavailable: evaluation)
        }
        do {
            return try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason) ? .verified : .failed
        } catch {
            return Self.result(for: error)
        }
    }

    /// The result for the error `canEvaluatePolicy` reported. Only a missing passcode is
    /// ``OwnerAuthenticationResult/noPasscodeSet``.
    static func result(forUnavailable error: NSError?) -> OwnerAuthenticationResult {
        if let error, error.domain == LAError.errorDomain, error.code == LAError.Code.passcodeNotSet.rawValue { return .noPasscodeSet }
        return .unavailable(error?.localizedDescription ?? "owner authentication is not available")
    }

    /// The result for an error thrown by `evaluatePolicy`. Anything not a cancellation or a missing
    /// passcode is a failure, never a pass.
    static func result(for error: Error) -> OwnerAuthenticationResult {
        guard let failure = error as? LAError else { return .failed }
        switch failure.code {
        case .userCancel, .appCancel, .systemCancel: return .cancelled
        case .passcodeNotSet: return .noPasscodeSet
        default: return .failed
        }
    }
}
#endif

#if canImport(Observation)
/// Lock state for an app that asks for the owner when it opens and when it returns after a while in
/// the background. UI-free: a view observes it. The product persists `isEnabled` itself.
///
/// - Locked at creation when the lock is enabled (or as `startsLocked` says).
/// - Backgrounding starts a clock; coming back after at least ``graceSeconds`` locks and returns true
///   so the app asks at once. The system prompt itself makes the scene inactive, so backgrounding while
///   a prompt is up starts no clock.
/// - ``unlock()`` opens the lock only for a verified owner (or a device with no passcode). A cancelled
///   prompt leaves it locked with no failure; every other end leaves it locked with a failure.
@MainActor
@Observable
public final class AppLockCoordinator {
    public private(set) var isLocked: Bool
    /// True while the system prompt is up (the scene goes inactive meanwhile).
    public private(set) var isAsking = false
    public private(set) var failure: AppLockFailure?
    public private(set) var isEnabled: Bool
    public let graceSeconds: TimeInterval

    @ObservationIgnored private var backgroundedAt: Date?
    @ObservationIgnored private let authenticator: any OwnerAuthenticating
    @ObservationIgnored private let now: @Sendable () -> Date
    @ObservationIgnored private let unlockReason: String

    /// - Parameters:
    ///   - isEnabled: the product's own setting.
    ///   - startsLocked: overrides the initial state (a demo or a test); default `isEnabled`.
    ///   - graceSeconds: time in the background that locks on return (30 by default).
    ///   - unlockReason: the text of the unlock prompt (the product's).
    public init(
        isEnabled: Bool, startsLocked: Bool? = nil, graceSeconds: TimeInterval = 30, unlockReason: String,
        authenticator: any OwnerAuthenticating,
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.isEnabled = isEnabled
        self.isLocked = startsLocked ?? isEnabled
        self.graceSeconds = graceSeconds
        self.unlockReason = unlockReason
        self.authenticator = authenticator
        self.now = now
    }

    #if canImport(LocalAuthentication)
    /// The system owner check (Face ID, Touch ID or Optic ID, else the passcode).
    public convenience init(isEnabled: Bool, startsLocked: Bool? = nil, graceSeconds: TimeInterval = 30, unlockReason: String) {
        self.init(
            isEnabled: isEnabled, startsLocked: startsLocked, graceSeconds: graceSeconds, unlockReason: unlockReason,
            authenticator: SystemOwnerAuthentication()
        )
    }
    #endif

    public func didEnterBackground() {
        if !isAsking { backgroundedAt = now() }
    }

    /// Call when the app becomes active. Returns true when the app just locked and should ask at once.
    @discardableResult
    public func didBecomeActive() -> Bool {
        defer { backgroundedAt = nil }
        guard isEnabled, !isLocked, let since = backgroundedAt else { return false }
        guard now().timeIntervalSince(since) >= graceSeconds else { return false }
        isLocked = true
        failure = nil
        return true
    }

    /// Asks for the owner and opens the lock on success. Ignored while a prompt is already up.
    public func unlock() async {
        guard isEnabled else { isLocked = false; return }
        guard !isAsking else { return }
        isAsking = true
        defer { isAsking = false }
        switch await authenticator.authenticate(reason: unlockReason) {
        case .verified, .noPasscodeSet:
            isLocked = false
            failure = nil
        case .cancelled:
            failure = nil
        case .unavailable:
            failure = .unavailable
        case .failed:
            failure = .couldNotVerify
        }
    }

    /// Asks before something weakens the lock (turning it off), so someone holding an unlocked device
    /// cannot drop it silently. A device with no passcode at all passes without asking.
    public func confirmOwner(reason: String) async -> Bool {
        isAsking = true
        defer { isAsking = false }
        switch await authenticator.authenticate(reason: reason) {
        case .verified, .noPasscodeSet: return true
        case .cancelled, .failed, .unavailable: return false
        }
    }

    /// Records the product's setting. Turning the lock off unlocks.
    public func setEnabled(_ on: Bool) {
        isEnabled = on
        if !on { isLocked = false }
    }
}
#endif
