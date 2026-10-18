import Foundation
#if canImport(LocalAuthentication)
import LocalAuthentication

/// What the user did with the system verification prompt.
public enum UserVerificationOutcome: Sendable, Equatable {
    case verified
    /// The user, the app or the system dismissed the prompt.
    case cancelled
    /// The prompt ended without verifying (wrong finger or password too often, lockout).
    case failed
    /// The policy cannot be evaluated here (no biometry enrolled, no passcode); the text is the
    /// system's description. The caller falls back to its own path (a PIN).
    case unavailable(String)
}

/// Proving the person at the keyboard is the account owner: Touch ID / Face ID, or the account
/// password when `biometricsOnly` is false. The Swift counterpart of `verify_user` in the Rust
/// `rightkit-credentials` crate (Windows Hello there); both answer verified, cancelled, or not.
public enum UserVerification {
    static func policy(biometricsOnly: Bool) -> LAPolicy {
        #if os(watchOS)
        // watchOS has no biometric-only policy; wrist detection plus passcode is its owner check.
        return .deviceOwnerAuthentication
        #else
        return biometricsOnly ? .deviceOwnerAuthenticationWithBiometrics : .deviceOwnerAuthentication
        #endif
    }

    /// Whether the policy can be evaluated now (hardware present and enrolled).
    public static func isAvailable(biometricsOnly: Bool = false) -> Bool {
        LAContext().canEvaluatePolicy(policy(biometricsOnly: biometricsOnly), error: nil)
    }

    /// Shows the system prompt with `reason`. Never throws: every end state is an outcome.
    public static func verify(reason: String, biometricsOnly: Bool = false) async -> UserVerificationOutcome {
        let context = LAContext()
        let chosen = policy(biometricsOnly: biometricsOnly)
        var evaluation: NSError?
        guard context.canEvaluatePolicy(chosen, error: &evaluation) else {
            return .unavailable(evaluation?.localizedDescription ?? "user verification is not available")
        }
        do {
            return try await context.evaluatePolicy(chosen, localizedReason: reason) ? .verified : .failed
        } catch {
            return outcome(for: error)
        }
    }

    /// The outcome for an error thrown by `evaluatePolicy`. Anything not a cancellation is a
    /// failure, never a pass.
    static func outcome(for error: Error) -> UserVerificationOutcome {
        guard let failure = error as? LAError else { return .failed }
        switch failure.code {
        case .userCancel, .appCancel, .systemCancel:
            return .cancelled
        case .passcodeNotSet:
            return .unavailable(failure.localizedDescription)
        #if !os(watchOS)
        case .biometryNotAvailable, .biometryNotEnrolled:
            return .unavailable(failure.localizedDescription)
        #endif
        default:
            return .failed
        }
    }
}
#endif
