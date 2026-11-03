#if canImport(AuthenticationServices) && os(iOS)
import AuthenticationServices
import Foundation

// RightKitCredentialProvider: the lifecycle of an `ASCredentialProviderViewController` extension
// and a bounded bridge into a Rust passkey core (`rightkit-passkeys`). The extension owns no
// passkey logic and no storage; the core does, behind `PasskeyCore`.
//
// Memory discipline (iOS credential-provider extensions run under a hard memory limit of a few
// tens of megabytes, and the system kills one that exceeds it):
// - No key derivation inside the extension. An Argon2id or other memory-hard KDF must not run
//   here. The vault key arrives already derived (a key the app stored in the shared keychain
//   access group, gated by biometry), or the request is handed to the app through
//   `PasskeyCore` returning `.needsApp`, which the controller turns into a cancel with a
//   message the system shows.
// - Requests and responses cross the bridge as bounded byte blobs (`BoundedPasskeyBridge`);
//   nothing larger than the caps is allocated on the extension's behalf.
// - One request is in flight at a time; a second one is refused rather than queued.

/// A passkey registration the extension asks the core to perform.
public struct PasskeyRegistrationInput: Sendable {
    public let relyingPartyID: String
    public let userName: String
    public let userHandle: Data
    public let clientDataHash: Data
    public let requireUserVerification: Bool
}

/// A passkey assertion the extension asks the core to perform.
public struct PasskeyAssertionInput: Sendable {
    public let relyingPartyID: String
    public let credentialID: Data?
    public let clientDataHash: Data
    public let requireUserVerification: Bool
    /// False for `provideCredentialWithoutUserInteraction`: the core must not prompt.
    public let allowUserInteraction: Bool
}

public struct PasskeyRegistrationOutput: Sendable {
    public let credentialID: Data
    public let attestationObject: Data
}

public struct PasskeyAssertionOutput: Sendable {
    public let userHandle: Data
    public let signature: Data
    public let authenticatorData: Data
    public let credentialID: Data
}

public enum PasskeyCoreError: Error, Equatable, Sendable {
    /// The user declined or cancelled.
    case userCancelled
    /// No credential matches.
    case noCredentials
    /// The request needs the app (locked vault, key not derived): the extension cannot do it.
    case needsApp
    /// The call took longer than the bridge's deadline.
    case timedOut
    /// A request or response was over the bridge's byte cap, or another call was in flight.
    case refused(String)
    case failed(String)
}

/// The seam into the Rust passkey core. Implemented over the C ABI of the app's Rust build.
public protocol PasskeyCore: Sendable {
    func register(_ input: PasskeyRegistrationInput) async throws -> PasskeyRegistrationOutput
    func assert(_ input: PasskeyAssertionInput) async throws -> PasskeyAssertionOutput
}

/// Bounds the calls into a `PasskeyCore`: byte caps on what crosses, one deadline per call, and
/// one call in flight.
public final class BoundedPasskeyBridge: @unchecked Sendable {
    public struct Limits: Sendable {
        public var maxInputBytes: Int
        public var maxOutputBytes: Int
        public var deadline: TimeInterval
        public init(maxInputBytes: Int = 16 * 1024, maxOutputBytes: Int = 64 * 1024, deadline: TimeInterval = 25) {
            self.maxInputBytes = maxInputBytes
            self.maxOutputBytes = maxOutputBytes
            self.deadline = deadline
        }
    }

    private let core: PasskeyCore
    private let limits: Limits
    private let lock = NSLock()
    private var busy = false

    public init(core: PasskeyCore, limits: Limits = Limits()) {
        self.core = core
        self.limits = limits
    }

    private func begin() throws {
        lock.lock(); defer { lock.unlock() }
        if busy { throw PasskeyCoreError.refused("another passkey request is in flight") }
        busy = true
    }

    private func end() {
        lock.lock(); busy = false; lock.unlock()
    }

    public func register(_ input: PasskeyRegistrationInput) async throws -> PasskeyRegistrationOutput {
        let size = input.relyingPartyID.utf8.count + input.userName.utf8.count + input.userHandle.count + input.clientDataHash.count
        guard size <= limits.maxInputBytes else { throw PasskeyCoreError.refused("request too large") }
        try begin(); defer { end() }
        let output = try await withDeadline { try await self.core.register(input) }
        guard output.credentialID.count + output.attestationObject.count <= limits.maxOutputBytes else {
            throw PasskeyCoreError.refused("response too large")
        }
        return output
    }

    public func assert(_ input: PasskeyAssertionInput) async throws -> PasskeyAssertionOutput {
        let size = input.relyingPartyID.utf8.count + (input.credentialID?.count ?? 0) + input.clientDataHash.count
        guard size <= limits.maxInputBytes else { throw PasskeyCoreError.refused("request too large") }
        try begin(); defer { end() }
        let output = try await withDeadline { try await self.core.assert(input) }
        guard output.userHandle.count + output.signature.count + output.authenticatorData.count + output.credentialID.count <= limits.maxOutputBytes else {
            throw PasskeyCoreError.refused("response too large")
        }
        return output
    }

    private func withDeadline<T: Sendable>(_ work: @escaping @Sendable () async throws -> T) async throws -> T {
        let nanos = UInt64(max(limits.deadline, 0.1) * 1_000_000_000)
        return try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await work() }
            group.addTask {
                try await Task.sleep(nanoseconds: nanos)
                throw PasskeyCoreError.timedOut
            }
            defer { group.cancelAll() }
            guard let first = try await group.next() else { throw PasskeyCoreError.failed("no result") }
            return first
        }
    }
}

/// The extension's principal class. Subclass it in the app's extension target, set
/// `bridge` in `viewDidLoad` before calling `super`, and point the extension's Info.plist
/// `NSExtensionPrincipalClass` at the subclass.
@available(iOS 17.0, *)
open class RightKitCredentialProviderController: ASCredentialProviderViewController {
    /// The bounded bridge into the Rust core. Required before any request arrives.
    public var bridge: BoundedPasskeyBridge?

    /// Called when a request needs the app (`PasskeyCoreError.needsApp`): show your own message.
    open func presentNeedsApp() {}

    private func cancel(_ code: ASExtensionError.Code) {
        extensionContext.cancelRequest(withError: NSError(domain: ASExtensionErrorDomain, code: code.rawValue))
    }

    private func fail(_ error: Error) {
        switch error as? PasskeyCoreError {
        case .userCancelled?: cancel(.userCanceled)
        case .noCredentials?: cancel(.credentialIdentityNotFound)
        case .needsApp?:
            presentNeedsApp()
            cancel(.userInteractionRequired)
        default: cancel(.failed)
        }
    }

    open override func provideCredentialWithoutUserInteraction(for credentialRequest: any ASCredentialRequest) {
        guard let request = credentialRequest as? ASPasskeyCredentialRequest else {
            cancel(.failed)
            return
        }
        perform(request, allowUserInteraction: false)
    }

    open override func prepareInterfaceToProvideCredential(for credentialRequest: any ASCredentialRequest) {
        guard let request = credentialRequest as? ASPasskeyCredentialRequest else {
            cancel(.failed)
            return
        }
        perform(request, allowUserInteraction: true)
    }

    open override func prepareInterface(forPasskeyRegistration registrationRequest: any ASCredentialRequest) {
        guard let bridge, let request = registrationRequest as? ASPasskeyCredentialRequest,
              let identity = request.credentialIdentity as? ASPasskeyCredentialIdentity else {
            cancel(.failed)
            return
        }
        let input = PasskeyRegistrationInput(
            relyingPartyID: identity.relyingPartyIdentifier,
            userName: identity.userName,
            userHandle: identity.userHandle,
            clientDataHash: request.clientDataHash,
            requireUserVerification: request.userVerificationPreference == .required
        )
        Task { @MainActor in
            do {
                let output = try await bridge.register(input)
                let credential = ASPasskeyRegistrationCredential(
                    relyingParty: input.relyingPartyID,
                    clientDataHash: input.clientDataHash,
                    credentialID: output.credentialID,
                    attestationObject: output.attestationObject
                )
                await extensionContext.completeRegistrationRequest(using: credential)
            } catch {
                fail(error)
            }
        }
    }

    private func perform(_ request: ASPasskeyCredentialRequest, allowUserInteraction: Bool) {
        guard let bridge, let identity = request.credentialIdentity as? ASPasskeyCredentialIdentity else {
            cancel(.failed)
            return
        }
        let input = PasskeyAssertionInput(
            relyingPartyID: identity.relyingPartyIdentifier,
            credentialID: identity.credentialID,
            clientDataHash: request.clientDataHash,
            requireUserVerification: request.userVerificationPreference == .required,
            allowUserInteraction: allowUserInteraction
        )
        Task { @MainActor in
            do {
                let output = try await bridge.assert(input)
                let credential = ASPasskeyAssertionCredential(
                    userHandle: output.userHandle,
                    relyingParty: input.relyingPartyID,
                    signature: output.signature,
                    clientDataHash: input.clientDataHash,
                    authenticatorData: output.authenticatorData,
                    credentialID: output.credentialID
                )
                await extensionContext.completeAssertionRequest(using: credential)
            } catch {
                fail(error)
            }
        }
    }
}
#endif
