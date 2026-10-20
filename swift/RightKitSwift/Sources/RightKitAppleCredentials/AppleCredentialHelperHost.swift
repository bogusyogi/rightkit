import Foundation

// The credential helper wire protocol, moved out of HeardRight's `HeardRightAppleCredentials` CLI
// (PK16-2). A Rust or Tauri host cannot call the Security framework's synchronizable-item APIs, so a
// small signed helper (a nested login-item app that carries the keychain-access-groups
// entitlement) is spawned per request: one JSON request line on stdin, one JSON response line on
// stdout. This type owns the command set and its rules; the product keeps what is its own: the
// Keychain service and access group, the account allowlist and the wording of its messages.
//
// Request:  {"command":"status"}
//           {"command":"read","scope":"synced"|"device","account":"<allowlisted>"}
//           {"command":"write","scope":"synced"|"device","account":"<allowlisted>","value":"<string>"}
//           {"command":"delete_everywhere","account":"<allowlisted>"}
//           {"command":"copy_synced_to_device_only","account":"<allowlisted>"}
// `account` may be omitted, which selects the configured default (older callers owned one account).
//
// Response: {"ok":true,"status":"ready"} | {"ok":true,"status":"unavailable","detail":"..."}   (status)
//           {"ok":true,"value":"..."} | {"ok":true,"value":null}                                (read)
//           {"ok":true}                                                                          (write, delete_everywhere)
//           {"ok":true,"copied":true|false}                                                      (copy_synced_to_device_only)
//           {"ok":false,"error":"..."}                                                           (any failure)
//
// Every command is an explicit user or policy action decided by the host: the helper keeps no state
// between invocations. Only allowlisted accounts are ever read, written or deleted, so a caller of
// the helper cannot enumerate or touch any other item of the service.

/// One request line.
public struct AppleCredentialHelperRequest: Decodable, Equatable, Sendable {
    public let command: String
    public let scope: String?
    public let account: String?
    public let value: String?

    public init(command: String, scope: String? = nil, account: String? = nil, value: String? = nil) {
        self.command = command
        self.scope = scope
        self.account = account
        self.value = value
    }
}

/// One response line. Encodes only the fields that apply; a successful `read` always carries `value`
/// (`null` when the item is absent).
public struct AppleCredentialHelperResponse: Encodable, Equatable, Sendable {
    public let ok: Bool
    public var status: String?
    public var detail: String?
    public var error: String?
    public var value: String?
    public var copied: Bool?
    var carriesValue = false

    public static func failure(_ message: String) -> Self { Self(ok: false, error: message) }
    public static let success = Self(ok: true)

    private enum CodingKeys: String, CodingKey { case ok, status, detail, error, value, copied }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(ok, forKey: .ok)
        try container.encodeIfPresent(status, forKey: .status)
        try container.encodeIfPresent(detail, forKey: .detail)
        try container.encodeIfPresent(error, forKey: .error)
        if carriesValue { try container.encode(value, forKey: .value) } else { try container.encodeIfPresent(value, forKey: .value) }
        try container.encodeIfPresent(copied, forKey: .copied)
    }
}

/// Runs the helper commands over an ``AppleCredentialStore``.
public final class AppleCredentialHelperHost: Sendable {
    public struct Configuration: Sendable, Equatable {
        /// The only accounts the helper will read, write or delete. Must not be empty.
        public var accounts: [String]
        /// The account a request without `account` means. Must be one of ``accounts``.
        public var defaultAccount: String
        /// The `detail` of an `unavailable` status when the build lacks the keychain-access-groups
        /// entitlement (the product's own words).
        public var missingEntitlementDetail: String

        public init(
            accounts: [String], defaultAccount: String,
            missingEntitlementDetail: String = "This build is missing the keychain-access-groups entitlement."
        ) {
            precondition(!accounts.isEmpty && accounts.contains(defaultAccount), "the default account must be an allowlisted account")
            self.accounts = accounts
            self.defaultAccount = defaultAccount
            self.missingEntitlementDetail = missingEntitlementDetail
        }
    }

    public let configuration: Configuration
    private let store: AppleCredentialStore
    private let backend: any AppleCredentialBackend

    /// - Parameters:
    ///   - store: the store over the product's service and access group.
    ///   - backend: the backend `store` was built with (used for the broad synced read and the
    ///     any-scope deletion). Pass the same instance.
    public init(configuration: Configuration, store: AppleCredentialStore, backend: any AppleCredentialBackend) {
        self.configuration = configuration
        self.store = store
        self.backend = backend
    }

    /// The Security.framework host for `service` in `accessGroup`.
    public convenience init(service: String, accessGroup: String, configuration: Configuration) {
        let backend = SecurityAppleCredentialBackend(accessGroup: accessGroup)
        self.init(
            configuration: configuration,
            store: AppleCredentialStore(service: service, accessGroup: accessGroup, backend: backend),
            backend: backend
        )
    }

    // MARK: Lines

    /// One request line in, one response line out, and the exit code a spawn-per-request helper
    /// should use: 0 for a handled request (including a failed command, which is `ok:false`), 1 for a
    /// missing or unreadable request line.
    public func respond(toLine line: String?) -> (line: String, exitCode: Int32) {
        guard let line, !line.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return (Self.encode(.failure("no request on stdin")), 1)
        }
        guard let request = try? JSONDecoder().decode(AppleCredentialHelperRequest.self, from: Data(line.utf8)) else {
            return (Self.encode(.failure("invalid request JSON")), 1)
        }
        return (Self.encode(handle(request)), 0)
    }

    static func encode(_ response: AppleCredentialHelperResponse) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(response), let line = String(data: data, encoding: .utf8) else {
            return #"{"error":"failed to encode response","ok":false}"#
        }
        return line.replacingOccurrences(of: "\n", with: "")
    }

    // MARK: Commands

    public func handle(_ request: AppleCredentialHelperRequest) -> AppleCredentialHelperResponse {
        switch request.command {
        case "status": return status()
        case "read":
            guard let scope = parseScope(request.scope) else { return .failure("missing or invalid scope") }
            guard let account = allowlisted(request.account) else { return .failure("missing or invalid account") }
            return read(scope: scope, account: account)
        case "write":
            guard let scope = parseScope(request.scope) else { return .failure("missing or invalid scope") }
            guard let value = request.value, !value.isEmpty else { return .failure("missing value") }
            guard let account = allowlisted(request.account) else { return .failure("missing or invalid account") }
            return write(value, scope: scope, account: account)
        case "delete_everywhere":
            guard let account = allowlisted(request.account) else { return .failure("missing or invalid account") }
            return deleteEverywhere(account: account)
        case "copy_synced_to_device_only":
            guard let account = allowlisted(request.account) else { return .failure("missing or invalid account") }
            return copySyncedToDeviceOnly(account: account)
        default:
            return .failure("unknown command: \(request.command)")
        }
    }

    private func parseScope(_ wire: String?) -> AppleCredentialScope? { wire.flatMap(AppleCredentialScope.init(rawValue:)) }

    /// Absent selects the default; present must be allowlisted.
    private func allowlisted(_ wire: String?) -> String? {
        let name = wire ?? configuration.defaultAccount
        return configuration.accounts.contains(name) ? name : nil
    }

    /// Whether this process can use the shared Keychain item at all, independent of whether a credential
    /// exists. A missing item still proves the access group and entitlement are wired.
    private func status() -> AppleCredentialHelperResponse {
        switch store.status() {
        case let .success((ready, detail)):
            return AppleCredentialHelperResponse(ok: true, status: ready ? "ready" : "unavailable", detail: ready ? nil : detail)
        case .failure(.missingEntitlement):
            return AppleCredentialHelperResponse(ok: true, status: "unavailable", detail: configuration.missingEntitlementDetail)
        case let .failure(error):
            return AppleCredentialHelperResponse(ok: true, status: "unavailable", detail: error.errorDescription)
        }
    }

    /// A failed read is an error, never "absent": a host must not treat an unreadable item as no item.
    /// The synced scope uses the broad synchronizable lookup (an item another device minted a moment
    /// ago is found before this device's sync attribute settles) and still never returns a
    /// device-only copy.
    private func read(scope: AppleCredentialScope, account: String) -> AppleCredentialHelperResponse {
        switch store.read(account, scope: scope, broadSynced: true) {
        case let .success(value):
            var response = AppleCredentialHelperResponse(ok: true, value: value)
            response.carriesValue = true
            return response
        case let .failure(error):
            return .failure(Self.message(error))
        }
    }

    private func write(_ value: String, scope: AppleCredentialScope, account: String) -> AppleCredentialHelperResponse {
        switch store.write(value, account: account, scope: scope) {
        case .success: return .success
        case let .failure(error): return .failure(Self.message(error))
        }
    }

    /// Removes both copies. Apple propagates a synchronizable item's deletion to every device on the
    /// Apple Account, so only an explicit "forget this credential everywhere" action may call this,
    /// never a routine sync-off.
    private func deleteEverywhere(account: String) -> AppleCredentialHelperResponse {
        switch backend.deleteEverywhere(service: store.service, account: account) {
        case .success: return .success
        case let .failure(error): return .failure(Self.message(error))
        }
    }

    /// Turning sync OFF must never delete the synced item: copy what is synced into a device-only
    /// item and leave the synced one alone. `copied:false` means nothing was synced, which is not an
    /// error; a copy that could not be written IS one (`copied:true` is never reported for a copy that
    /// was not made).
    private func copySyncedToDeviceOnly(account: String) -> AppleCredentialHelperResponse {
        switch store.read(account, scope: .synced, broadSynced: true) {
        case let .failure(error):
            return .failure(Self.message(error))
        case let .success(value):
            guard let value else {
                var none = AppleCredentialHelperResponse(ok: true)
                none.copied = false
                return none
            }
            switch store.write(value, account: account, scope: .device) {
            case let .failure(error): return .failure(Self.message(error))
            case .success:
                var copied = AppleCredentialHelperResponse(ok: true)
                copied.copied = true
                return copied
            }
        }
    }

    private static func message(_ error: AppleCredentialError) -> String {
        error.errorDescription ?? "credential operation failed"
    }
}
