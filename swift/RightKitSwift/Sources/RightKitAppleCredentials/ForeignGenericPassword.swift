import Foundation

#if os(macOS)
import Security
import LocalAuthentication

/// A generic-password item that another app (or tool) filed in the **legacy file-based Keychain**.
///
/// `AppleCredentialStore` and `SecurityAppleCredentialBackend` own items this process created, in the
/// data-protection Keychain, under the caller's access group. They cannot see an item a different
/// program wrote to the login keychain, which is where tools such as the Claude Code CLI keep their
/// tokens. This type is the deliberately separate, **read-only** path for those foreign items:
///
/// - It queries only the legacy file Keychain (`kSecUseDataProtectionKeychain = false`) and sets no
///   access group, because file Keychains guard items with ACLs rather than groups.
/// - When several items match, it picks the newest by `kSecAttrModificationDate`. A tool that files a
///   new item on every token rotation, rather than updating in place, leaves several duplicates, and
///   `kSecMatchLimitOne` gives no ordering across them. Enumerating attributes is free of prompts, so
///   the newest is chosen by comparison, not luck.
/// - It reads the secret **by persistent reference** (`kSecValuePersistentRef`) of the chosen item, so
///   the data read targets exactly the item that won, not whatever a second search would return.
/// - By default (``Policy/noPrompt``) it never presents UI. If the item's ACL would require the user to
///   approve access, `read` returns ``ReadError/interactionRequired`` instead of prompting.
///
/// There is intentionally no write, update, or delete API here: this library must not alter another
/// app's items. To remove an item RightKit itself created in the legacy Keychain use
/// ``SecurityAppleCredentialBackend/deleteAnyScope(service:account:)``.
public struct ForeignGenericPassword: Sendable, Equatable {
    /// Whether a read may put up the system "wants to access your confidential information" dialog.
    public enum Policy: Sendable, Equatable {
        /// Never prompt. Uses `kSecUseAuthenticationUISkip` and an `LAContext` with
        /// `interactionNotAllowed`, and reports ``ReadError/interactionRequired`` when access would
        /// need approval. This is the default.
        case noPrompt
        /// Allow the system to ask the user. Use only for an action the user just initiated.
        case allowPrompt
    }

    /// Why a read produced no secret. Never carries secret bytes.
    public enum ReadError: Error, LocalizedError, Equatable, Sendable {
        /// No generic-password item matches the service (and account, if one was given).
        case notFound
        /// The item exists, but reading its data would require UI (ACL approval or an unlock), and the
        /// policy forbids prompting.
        case interactionRequired
        /// The user, or the item's ACL, denied access.
        case accessDenied
        /// `service` (or a supplied `account`) is empty.
        case invalidArguments
        /// Any other Security.framework failure, with its raw status.
        case keychainStatus(Int32)

        public var errorDescription: String? {
            switch self {
            case .notFound: return "No matching Keychain item."
            case .interactionRequired: return "Reading the Keychain item would require user approval."
            case .accessDenied: return "Access to the Keychain item was denied."
            case .invalidArguments: return "Keychain service or account is empty."
            case let .keychainStatus(status): return "Keychain operation failed (status \(status))."
            }
        }
    }

    /// Attributes of one matching item, without its secret.
    public struct Match: Sendable, Equatable {
        public let service: String
        public let account: String?
        public let modifiedAt: Date?
        /// Opaque handle that fetches this exact item's data without searching again.
        public let persistentRef: Data
    }

    public let service: String
    public let account: String?
    public let modifiedAt: Date?
    public let persistentRef: Data
    /// The secret bytes. Treat as sensitive: do not log or persist.
    public let data: Data

    /// UTF-8 interpretation of ``data``, or nil when it is not valid UTF-8.
    public var string: String? { String(data: data, encoding: .utf8) }

    /// Reads the newest generic-password item filed under `service` (and `account`, if given) in the
    /// legacy file Keychain. See the type documentation for selection, reference and prompt rules.
    public static func read(
        service: String,
        account: String? = nil,
        policy: Policy = .noPrompt
    ) -> Result<ForeignGenericPassword, ReadError> {
        read(service: service, account: account, policy: policy, backend: SystemBackend())
    }

    /// Lists matching items by attributes only (never the secret and never a prompt), newest first.
    public static func matches(service: String, account: String? = nil) -> Result<[Match], ReadError> {
        guard validArguments(service: service, account: account) else { return .failure(.invalidArguments) }
        return SystemBackend().attributes(service: service, account: account, policy: .noPrompt).map { newestFirst($0) }
    }

    // MARK: Selection (pure, unit-tested)

    /// Orders matches newest first. An item without a modification date ranks below any dated item but
    /// is kept, so a lone undated item is still readable.
    static func newestFirst(_ matches: [Match]) -> [Match] {
        matches.enumerated()
            .sorted {
                let (l, r) = ($0.element.modifiedAt ?? .distantPast, $1.element.modifiedAt ?? .distantPast)
                return l != r ? l > r : $0.offset < $1.offset
            }
            .map(\.element)
    }

    /// Builds matches from `SecItemCopyMatching` attribute dictionaries; entries without a persistent
    /// reference cannot be read back and are dropped.
    static func matches(fromAttributes items: [[String: Any]]) -> [Match] {
        items.compactMap { item in
            guard let ref = item[kSecValuePersistentRef as String] as? Data else { return nil }
            return Match(
                service: item[kSecAttrService as String] as? String ?? "",
                account: item[kSecAttrAccount as String] as? String,
                modifiedAt: item[kSecAttrModificationDate as String] as? Date,
                persistentRef: ref
            )
        }
    }

    static func error(forStatus status: OSStatus, policy: Policy) -> ReadError {
        switch status {
        case errSecItemNotFound:
            // After a successful attribute enumeration, "not found" on a data read means the item was
            // skipped (Skip policy) or vanished; the caller resolves which.
            return .notFound
        case errSecInteractionNotAllowed, errSecAuthFailed:
            return policy == .noPrompt ? .interactionRequired : .accessDenied
        case errSecUserCanceled, errSecNotAvailable:
            return .accessDenied
        case errSecParam:
            return .invalidArguments
        default:
            return .keychainStatus(status)
        }
    }

    // MARK: Backend seam

    /// Seam over the two Security.framework calls so selection and error mapping are testable.
    protocol Backend {
        func attributes(service: String, account: String?, policy: Policy) -> Result<[Match], ReadError>
        func data(forPersistentRef ref: Data, policy: Policy) -> (status: OSStatus, data: Data?)
    }

    static func read(service: String, account: String?, policy: Policy, backend: some Backend) -> Result<ForeignGenericPassword, ReadError> {
        guard validArguments(service: service, account: account) else { return .failure(.invalidArguments) }
        let found: [Match]
        switch backend.attributes(service: service, account: account, policy: policy) {
        case let .success(items): found = newestFirst(items)
        case let .failure(error): return .failure(error)
        }
        guard let winner = found.first else { return .failure(.notFound) }
        let (status, data) = backend.data(forPersistentRef: winner.persistentRef, policy: policy)
        guard status == errSecSuccess, let data else {
            // The item was enumerated a moment ago, so under the no-prompt policy a skipped or refused
            // data read means approval is needed, not that the item is absent.
            if policy == .noPrompt, status == errSecItemNotFound { return .failure(.interactionRequired) }
            return .failure(error(forStatus: status, policy: policy))
        }
        return .success(ForeignGenericPassword(
            service: winner.service.isEmpty ? service : winner.service,
            account: winner.account,
            modifiedAt: winner.modifiedAt,
            persistentRef: winner.persistentRef,
            data: data
        ))
    }

    // MARK: No-prompt enforcement

    /// Status returned when the no-prompt guarantee cannot be given on this system.
    static let noPromptUnavailableStatus = errSecUnimplemented

    private static let interactionLock = NSLock()
    private typealias SetInteractionAllowed = @convention(c) (DarwinBoolean) -> OSStatus
    private typealias GetInteractionAllowed = @convention(c) (UnsafeMutablePointer<DarwinBoolean>) -> OSStatus

    /// Runs `body` so that, under ``Policy/noPrompt``, no Keychain UI can appear.
    ///
    /// `kSecUseAuthenticationUISkip` and `LAContext.interactionNotAllowed` only govern
    /// data-protection items. Legacy file-Keychain ACL approval ("wants to access your confidential
    /// information") ignores them: measured on macOS 27, a no-trusted-application item read with
    /// those two set blocked ~10 s on the dialog and then succeeded. The only switch legacy items
    /// honour is `SecKeychainSetUserInteractionAllowed(false)`. It is process-wide, deprecated and no
    /// longer declared in the SDK headers, so it is resolved with `dlsym`; calls are serialised and
    /// the previous value is restored. Other threads' Keychain calls inside the window are also
    /// refused without UI. If the symbol is missing, the guarantee is unavailable and the read
    /// fails closed with `errSecUnimplemented` instead of risking a dialog.
    static func withPolicy(_ policy: Policy, _ body: () -> OSStatus) -> OSStatus {
        guard policy == .noPrompt else { return body() }
        guard let setSymbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "SecKeychainSetUserInteractionAllowed"),
              let getSymbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "SecKeychainGetUserInteractionAllowed")
        else { return noPromptUnavailableStatus }
        let setAllowed = unsafeBitCast(setSymbol, to: SetInteractionAllowed.self)
        let getAllowed = unsafeBitCast(getSymbol, to: GetInteractionAllowed.self)
        interactionLock.lock()
        defer { interactionLock.unlock() }
        var previous: DarwinBoolean = true
        _ = getAllowed(&previous)
        guard setAllowed(false) == errSecSuccess else { return noPromptUnavailableStatus }
        defer { _ = setAllowed(previous) }
        return body()
    }

    private static func validArguments(service: String, account: String?) -> Bool {
        !service.isEmpty && (account?.isEmpty != true)
    }

    private struct SystemBackend: Backend {
        func attributes(service: String, account: String?, policy: Policy) -> Result<[Match], ReadError> {
            var query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: service,
                kSecUseDataProtectionKeychain as String: false,
                kSecReturnAttributes as String: true,
                kSecReturnPersistentRef as String: true,
                kSecMatchLimit as String: kSecMatchLimitAll,
            ]
            if let account { query[kSecAttrAccount as String] = account }
            let context = LAContext()
            context.interactionNotAllowed = true
            query[kSecUseAuthenticationContext as String] = context
            query[kSecUseAuthenticationUI as String] = kSecUseAuthenticationUISkip

            var result: CFTypeRef?
            let status = ForeignGenericPassword.withPolicy(policy) { SecItemCopyMatching(query as CFDictionary, &result) }
            if status == errSecItemNotFound { return .success([]) }
            guard status == errSecSuccess else { return .failure(ForeignGenericPassword.error(forStatus: status, policy: .noPrompt)) }
            // One match still comes back as a single dictionary rather than an array of one.
            let items = (result as? [[String: Any]]) ?? (result as? [String: Any]).map { [$0] } ?? []
            return .success(ForeignGenericPassword.matches(fromAttributes: items))
        }

        func data(forPersistentRef ref: Data, policy: Policy) -> (status: OSStatus, data: Data?) {
            var query: [String: Any] = [
                kSecValuePersistentRef as String: ref,
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
            ]
            if policy == .noPrompt {
                let context = LAContext()
                context.interactionNotAllowed = true
                query[kSecUseAuthenticationContext as String] = context
                query[kSecUseAuthenticationUI as String] = kSecUseAuthenticationUISkip
            }
            var result: CFTypeRef?
            let status = ForeignGenericPassword.withPolicy(policy) { SecItemCopyMatching(query as CFDictionary, &result) }
            return (status, result as? Data)
        }
    }
}
#endif
