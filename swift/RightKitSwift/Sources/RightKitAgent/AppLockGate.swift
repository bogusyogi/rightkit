import Foundation
import LocalAuthentication
import Observation

/// App-level biometric gate: the enforcement point for "require Face ID
/// before using a stored API key," and the piece that gives the grace-window
/// UX a per-item Keychain ACL cannot (see `SecretStore` doc comment for why).
///
/// Pattern: authenticate once, remember `lastUnlockedAt`, and treat any call
/// within `graceInterval` of that as already-unlocked — no prompt. A call
/// after the grace window has elapsed re-prompts. This is the standard shape
/// used by password managers and banking apps for exactly this requirement:
/// "ask again the first time the app is opened after a while, not every time
/// I switch back and forth."
@MainActor
@Observable
public final class AppLockGate {
    private static let requireBiometricsKey = "app.coderight.requireBiometrics"

    /// Whether the gate is enforced at all. Settings toggles this. When
    /// false, `unlockIfNeeded` always succeeds immediately — secrets are
    /// still device-only Keychain storage (see `SecretStore`), just without
    /// the additional biometric step.
    public var requireBiometrics: Bool {
        didSet {
            UserDefaults.standard.set(requireBiometrics, forKey: Self.requireBiometricsKey)
            if !requireBiometrics { lastUnlockedAt = nil }
        }
    }

    /// How long a successful authentication stays valid. Five minutes matches
    /// the window most biometric-gated apps use for "still probably you" —
    /// long enough that switching to Safari to copy something and back does
    /// not re-prompt, short enough that a phone left on a table for a while
    /// re-locks before someone else picks it up.
    public var graceInterval: TimeInterval = 5 * 60

    private var lastUnlockedAt: Date?

    public init(defaults: UserDefaults = .standard) {
        self.requireBiometrics = defaults.object(forKey: Self.requireBiometricsKey) == nil
            ? true // default on — a phone is a more exposed surface than a laptop.
            : defaults.bool(forKey: Self.requireBiometricsKey)
    }

    /// True if a call right now would proceed without prompting.
    public var isCurrentlyUnlocked: Bool {
        guard requireBiometrics else { return true }
        guard let lastUnlockedAt else { return false }
        return Date().timeIntervalSince(lastUnlockedAt) < graceInterval
    }

    /// Ensure the gate is open, prompting only if the grace window has
    /// lapsed (or there has never been a successful authentication).
    ///
    /// `reason` is shown in the system prompt when one is needed, so it must
    /// say what the credential will be used for.
    public func unlockIfNeeded(reason: String) async throws {
        guard requireBiometrics else { return }
        if isCurrentlyUnlocked { return }

        let context = LAContext()
        context.localizedReason = reason

        let success: Bool
        do {
            success = try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)
        } catch let error as LAError {
            switch error.code {
            case .userCancel, .userFallback, .systemCancel, .appCancel:
                throw SecretStore.Failure.notAuthenticated
            case .biometryLockout:
                throw SecretStore.Failure.biometryLockedOut
            case .passcodeNotSet, .biometryNotAvailable, .biometryNotEnrolled:
                throw SecretStore.Failure.deviceNotSecured
            default:
                throw SecretStore.Failure.notAuthenticated
            }
        }
        guard success else { throw SecretStore.Failure.notAuthenticated }
        lastUnlockedAt = Date()
    }

    /// Force the next call to re-prompt regardless of the grace window.
    /// Called when the app returns from the background after being away long
    /// enough that "still probably you" no longer holds — see
    /// `CodeRightApp`'s scene-phase handling.
    public func lockIfStale(backgroundedAt: Date) {
        guard requireBiometrics, let lastUnlockedAt else { return }
        if backgroundedAt.timeIntervalSince(lastUnlockedAt) >= graceInterval {
            self.lastUnlockedAt = nil
        }
    }
}
