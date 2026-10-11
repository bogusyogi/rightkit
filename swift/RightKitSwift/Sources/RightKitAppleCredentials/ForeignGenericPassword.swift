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
///
/// ## What each public call does to Keychain UI interaction
///
/// | Call | Reads secret? | Process-wide interaction switch | Per-query UI controls |
/// | --- | --- | --- | --- |
/// | ``matches(service:account:)`` / ``matchesDetailed(service:account:)`` | no | **never touched** | `LAContext.interactionNotAllowed` + `kSecUseAuthenticationUISkip` |
/// | ``read(service:account:policy:)`` / ``read(persistentRef:policy:)`` / ``read(_:policy:)`` with ``Policy/noPrompt`` | yes | `SecKeychainSetUserInteractionAllowed(false)` for the data read only, serialised, restored | same two flags |
/// | the same calls with ``Policy/allowPrompt`` | yes | never touched; the read registers as "prompting" so no no-prompt window opens meanwhile | none |
///
/// Metadata-only queries (attributes and persistent references, never `kSecReturnData`) cannot trigger
/// the legacy ACL approval dialog, which guards secret data, so they do not need the process-wide
/// switch and no longer flip it. The one residual case is a *locked* keychain, where macOS may show its
/// unlock sheet for any query; the per-query flags are the only control there and the system may ignore them.
///
/// The switch is unavoidable for a no-prompt *data* read (see ``withPolicy(_:_:)``). While such a
/// window is open, other threads' Keychain calls in this process are refused without UI, so a prompting
/// read waits for the window to close (it is short: no UI can be waiting inside it), and a no-prompt data
/// read that finds a prompting read in flight returns ``ReadError/interactionRequired`` at once instead
/// of flipping the switch under the open dialog.
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

    /// A ``ReadError`` together with the raw `OSStatus` it was derived from, for callers that must tell
    /// apart statuses ``ReadError`` merges (`.accessDenied` covers `errSecAuthFailed`,
    /// `errSecUserCanceled` and `errSecNotAvailable`; `.interactionRequired` covers
    /// `errSecInteractionNotAllowed`, `errSecAuthFailed` and a skipped read). Returned by the
    /// `...Detailed` calls; the plain calls return ``error`` alone, unchanged.
    public struct ReadFailure: Error, Equatable, Sendable {
        public let error: ReadError
        /// The Security.framework status behind ``error``, or nil when none produced it
        /// (`.invalidArguments` from an empty argument, an empty enumeration, `.notFound` from no match).
        public let status: Int32?

        public init(error: ReadError, status: Int32?) {
            self.error = error
            self.status = status
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
        return matchesDetailed(service: service, account: account).mapError(\.error)
    }

    /// Reads exactly the item `persistentRef` names (from ``Match/persistentRef`` or
    /// ``ForeignGenericPassword/persistentRef``) with no further search, so a caller that chose one of
    /// several matches gets that item. A stale reference gives ``ReadError/notFound``; an empty one
    /// ``ReadError/invalidArguments``. ``service`` is empty when the item carries no service attribute.
    public static func read(persistentRef: Data, policy: Policy = .noPrompt) -> Result<ForeignGenericPassword, ReadError> {
        readDetailed(persistentRef: persistentRef, policy: policy).mapError(\.error)
    }

    /// Reads the item a ``Match`` describes by its persistent reference, keeping the match's attributes
    /// (no attribute lookup is repeated).
    public static func read(_ match: Match, policy: Policy = .noPrompt) -> Result<ForeignGenericPassword, ReadError> {
        readDetailed(match, policy: policy).mapError(\.error)
    }

    /// ``read(service:account:policy:)`` with the raw status kept in the failure.
    public static func readDetailed(
        service: String,
        account: String? = nil,
        policy: Policy = .noPrompt
    ) -> Result<ForeignGenericPassword, ReadFailure> {
        readDetailed(service: service, account: account, policy: policy, backend: SystemBackend())
    }

    /// ``read(persistentRef:policy:)`` with the raw status kept in the failure.
    public static func readDetailed(persistentRef: Data, policy: Policy = .noPrompt) -> Result<ForeignGenericPassword, ReadFailure> {
        readDetailed(persistentRef: persistentRef, policy: policy, backend: SystemBackend())
    }

    /// ``read(_:policy:)`` with the raw status kept in the failure.
    public static func readDetailed(_ match: Match, policy: Policy = .noPrompt) -> Result<ForeignGenericPassword, ReadFailure> {
        readDetailed(match, policy: policy, backend: SystemBackend())
    }

    /// ``matches(service:account:)`` with the raw status kept in the failure.
    public static func matchesDetailed(service: String, account: String? = nil) -> Result<[Match], ReadFailure> {
        guard validArguments(service: service, account: account) else {
            return .failure(ReadFailure(error: .invalidArguments, status: nil))
        }
        return SystemBackend().attributesDetailed(service: service, account: account, policy: .noPrompt).map { newestFirst($0) }
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

    static func failure(forStatus status: OSStatus, policy: Policy) -> ReadFailure {
        ReadFailure(error: error(forStatus: status, policy: policy), status: status)
    }

    // MARK: Backend seam

    /// Seam over the two Security.framework calls so selection and error mapping are testable.
    protocol Backend {
        func attributes(service: String, account: String?, policy: Policy) -> Result<[Match], ReadError>
        func data(forPersistentRef ref: Data, policy: Policy) -> (status: OSStatus, data: Data?)
        /// ``attributes(service:account:policy:)`` keeping the raw status. Defaults to wrapping it.
        func attributesDetailed(service: String, account: String?, policy: Policy) -> Result<[Match], ReadFailure>
        /// Attributes of the one item `ref` names (never its data). `nil` when the backend cannot say.
        func attributes(forPersistentRef ref: Data) -> Result<Match?, ReadFailure>
    }

    static func read(service: String, account: String?, policy: Policy, backend: some Backend) -> Result<ForeignGenericPassword, ReadError> {
        readDetailed(service: service, account: account, policy: policy, backend: backend).mapError(\.error)
    }

    static func readDetailed(service: String, account: String?, policy: Policy, backend: some Backend) -> Result<ForeignGenericPassword, ReadFailure> {
        guard validArguments(service: service, account: account) else {
            return .failure(ReadFailure(error: .invalidArguments, status: nil))
        }
        let found: [Match]
        switch backend.attributesDetailed(service: service, account: account, policy: policy) {
        case let .success(items): found = newestFirst(items)
        case let .failure(failure): return .failure(failure)
        }
        guard let winner = found.first else { return .failure(ReadFailure(error: .notFound, status: nil)) }
        return readData(of: winner, fallbackService: service, policy: policy, backend: backend)
    }

    static func read(persistentRef: Data, policy: Policy, backend: some Backend) -> Result<ForeignGenericPassword, ReadError> {
        readDetailed(persistentRef: persistentRef, policy: policy, backend: backend).mapError(\.error)
    }

    static func readDetailed(persistentRef: Data, policy: Policy, backend: some Backend) -> Result<ForeignGenericPassword, ReadFailure> {
        guard !persistentRef.isEmpty else { return .failure(ReadFailure(error: .invalidArguments, status: nil)) }
        // Attributes first: they never need approval, and they settle whether the item exists, so a
        // later "not found" on the data read can only mean it was skipped for want of approval.
        let known: Match?
        switch backend.attributes(forPersistentRef: persistentRef) {
        case let .success(match): known = match
        case let .failure(failure): return .failure(failure)
        }
        let match = known ?? Match(service: "", account: nil, modifiedAt: nil, persistentRef: persistentRef)
        return readData(of: match, fallbackService: "", policy: policy, backend: backend)
    }

    static func readDetailed(_ match: Match, policy: Policy, backend: some Backend) -> Result<ForeignGenericPassword, ReadFailure> {
        guard !match.persistentRef.isEmpty else { return .failure(ReadFailure(error: .invalidArguments, status: nil)) }
        return readData(of: match, fallbackService: "", policy: policy, backend: backend)
    }

    private static func readData(of winner: Match, fallbackService: String, policy: Policy, backend: some Backend) -> Result<ForeignGenericPassword, ReadFailure> {
        let (status, data) = backend.data(forPersistentRef: winner.persistentRef, policy: policy)
        guard status == errSecSuccess, let data else {
            // The item was enumerated a moment ago, so under the no-prompt policy a skipped or refused
            // data read means approval is needed, not that the item is absent.
            if policy == .noPrompt, status == errSecItemNotFound {
                return .failure(ReadFailure(error: .interactionRequired, status: status))
            }
            return .failure(failure(forStatus: status, policy: policy))
        }
        return .success(ForeignGenericPassword(
            service: winner.service.isEmpty ? fallbackService : winner.service,
            account: winner.account,
            modifiedAt: winner.modifiedAt,
            persistentRef: winner.persistentRef,
            data: data
        ))
    }

    // MARK: No-prompt enforcement

    /// Status returned when the no-prompt guarantee cannot be given on this system.
    static let noPromptUnavailableStatus = errSecUnimplemented

    /// Coordinates the process-wide interaction switch among this type's own data reads.
    ///
    /// - A no-prompt window flips the switch off, so windows are mutually exclusive, and no window may
    ///   open while a prompting read is in flight (the flip would make the system refuse the dialog
    ///   the user is looking at).
    /// - A prompting read does not touch the switch; it only waits for an open no-prompt window to
    ///   close (so it is not refused) and counts itself while it runs.
    final class InteractionGate: @unchecked Sendable {
        private let condition = NSCondition()
        private var suppressing = false
        private var prompting = 0

        /// Opens a no-prompt window, waiting for another one to close. Returns false, immediately, when
        /// a prompting read is in flight.
        func tryBeginSuppressing() -> Bool {
            condition.lock(); defer { condition.unlock() }
            while suppressing && prompting == 0 { condition.wait() }
            guard prompting == 0 else { return false }
            suppressing = true
            return true
        }

        func endSuppressing() {
            condition.lock()
            suppressing = false
            condition.broadcast()
            condition.unlock()
        }

        /// Registers a prompting read, waiting for an open no-prompt window to close.
        func beginPrompting() {
            condition.lock()
            while suppressing { condition.wait() }
            prompting += 1
            condition.unlock()
        }

        func endPrompting() {
            condition.lock()
            prompting -= 1
            condition.broadcast()
            condition.unlock()
        }
    }

    static let interactionGate = InteractionGate()
    private typealias SetInteractionAllowed = @convention(c) (DarwinBoolean) -> OSStatus
    private typealias GetInteractionAllowed = @convention(c) (UnsafeMutablePointer<DarwinBoolean>) -> OSStatus

    /// Resolves `SecKeychainGetUserInteractionAllowed` (test and diagnostics seam); nil if unavailable.
    static func processInteractionAllowed() -> Bool? {
        guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "SecKeychainGetUserInteractionAllowed") else { return nil }
        var value: DarwinBoolean = true
        guard unsafeBitCast(symbol, to: GetInteractionAllowed.self)(&value) == errSecSuccess else { return nil }
        return value.boolValue
    }

    /// Runs `body` (a data read) so that, under ``Policy/noPrompt``, no Keychain UI can appear.
    ///
    /// `kSecUseAuthenticationUISkip` and `LAContext.interactionNotAllowed` only govern
    /// data-protection items. Legacy file-Keychain ACL approval ("wants to access your confidential
    /// information") ignores them: measured on macOS 27, a no-trusted-application item read with
    /// those two set blocked ~10 s on the dialog and then succeeded. The only switch legacy items
    /// honour is `SecKeychainSetUserInteractionAllowed(false)`. It is process-wide, deprecated and no
    /// longer declared in the SDK headers, so it is resolved with `dlsym`. It is used **only** around
    /// secret-data reads (metadata queries never need it and never touch it). Windows are serialised
    /// behind ``interactionGate`` and the previous value is restored. Other threads' Keychain calls
    /// inside a window are refused without UI. If a ``Policy/allowPrompt`` read is in flight, a
    /// no-prompt read does not open a window (that would make the system refuse the open dialog) and
    /// returns `errSecInteractionNotAllowed`, i.e. ``ReadError/interactionRequired``, without waiting.
    /// A prompting read waits for an open window to close. If the symbol is missing, the guarantee is
    /// unavailable and the read fails closed with `errSecUnimplemented` instead of risking a dialog.
    static func withPolicy(_ policy: Policy, _ body: () -> OSStatus) -> OSStatus {
        guard policy == .noPrompt else {
            interactionGate.beginPrompting()
            defer { interactionGate.endPrompting() }
            return body()
        }
        guard let setSymbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "SecKeychainSetUserInteractionAllowed"),
              let getSymbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "SecKeychainGetUserInteractionAllowed")
        else { return noPromptUnavailableStatus }
        let setAllowed = unsafeBitCast(setSymbol, to: SetInteractionAllowed.self)
        let getAllowed = unsafeBitCast(getSymbol, to: GetInteractionAllowed.self)
        guard interactionGate.tryBeginSuppressing() else { return errSecInteractionNotAllowed }
        defer { interactionGate.endSuppressing() }
        var previous: DarwinBoolean = true
        _ = getAllowed(&previous)
        guard setAllowed(false) == errSecSuccess else { return noPromptUnavailableStatus }
        defer { _ = setAllowed(previous) }
        return body()
    }

    private static func validArguments(service: String, account: String?) -> Bool {
        !service.isEmpty && (account?.isEmpty != true)
    }

    /// The `SecItemCopyMatching` call, replaceable so tests can observe queries without a Keychain.
    typealias CopyMatching = (CFDictionary, UnsafeMutablePointer<CFTypeRef?>) -> OSStatus

    struct SystemBackend: Backend {
        var copyMatching: CopyMatching = { SecItemCopyMatching($0, $1) }

        /// Per-query "never show UI" controls; the only ones a metadata query needs or gets.
        private func addNoUI(to query: inout [String: Any]) {
            let context = LAContext()
            context.interactionNotAllowed = true
            query[kSecUseAuthenticationContext as String] = context
            query[kSecUseAuthenticationUI as String] = kSecUseAuthenticationUISkip
        }

        func attributes(service: String, account: String?, policy: Policy) -> Result<[Match], ReadError> {
            attributesDetailed(service: service, account: account, policy: policy).mapError(\.error)
        }

        /// Metadata only: never requests `kSecReturnData` and never touches the process-wide
        /// interaction switch, whatever `policy` is.
        func attributesDetailed(service: String, account: String?, policy: Policy) -> Result<[Match], ReadFailure> {
            var query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: service,
                kSecUseDataProtectionKeychain as String: false,
                kSecReturnAttributes as String: true,
                kSecReturnPersistentRef as String: true,
                kSecMatchLimit as String: kSecMatchLimitAll,
            ]
            if let account { query[kSecAttrAccount as String] = account }
            addNoUI(to: &query)

            var result: CFTypeRef?
            let status = copyMatching(query as CFDictionary, &result)
            if status == errSecItemNotFound { return .success([]) }
            guard status == errSecSuccess else { return .failure(ForeignGenericPassword.failure(forStatus: status, policy: .noPrompt)) }
            // One match still comes back as a single dictionary rather than an array of one.
            let items = (result as? [[String: Any]]) ?? (result as? [String: Any]).map { [$0] } ?? []
            return .success(ForeignGenericPassword.matches(fromAttributes: items))
        }

        /// Metadata only, like ``attributesDetailed(service:account:policy:)``.
        func attributes(forPersistentRef ref: Data) -> Result<Match?, ReadFailure> {
            var query: [String: Any] = [
                kSecValuePersistentRef as String: ref,
                kSecReturnAttributes as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
            ]
            addNoUI(to: &query)
            var result: CFTypeRef?
            let status = copyMatching(query as CFDictionary, &result)
            guard status == errSecSuccess else { return .failure(ForeignGenericPassword.failure(forStatus: status, policy: .noPrompt)) }
            let item = result as? [String: Any] ?? [:]
            return .success(Match(
                service: item[kSecAttrService as String] as? String ?? "",
                account: item[kSecAttrAccount as String] as? String,
                modifiedAt: item[kSecAttrModificationDate as String] as? Date,
                persistentRef: ref
            ))
        }

        func data(forPersistentRef ref: Data, policy: Policy) -> (status: OSStatus, data: Data?) {
            var query: [String: Any] = [
                kSecValuePersistentRef as String: ref,
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
            ]
            if policy == .noPrompt { addNoUI(to: &query) }
            var result: CFTypeRef?
            let status = ForeignGenericPassword.withPolicy(policy) { copyMatching(query as CFDictionary, &result) }
            return (status, result as? Data)
        }
    }
}

extension ForeignGenericPassword.Backend {
    func attributesDetailed(service: String, account: String?, policy: ForeignGenericPassword.Policy)
        -> Result<[ForeignGenericPassword.Match], ForeignGenericPassword.ReadFailure> {
        attributes(service: service, account: account, policy: policy).mapError { error in
            switch error {
            case let .keychainStatus(status): ForeignGenericPassword.ReadFailure(error: error, status: status)
            case .notFound: ForeignGenericPassword.ReadFailure(error: error, status: errSecItemNotFound)
            default: ForeignGenericPassword.ReadFailure(error: error, status: nil)
            }
        }
    }

    func attributes(forPersistentRef ref: Data) -> Result<ForeignGenericPassword.Match?, ForeignGenericPassword.ReadFailure> {
        .success(nil)
    }
}
#endif
