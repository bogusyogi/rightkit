import CryptoKit
import Foundation
#if canImport(Combine)
import Combine
#endif
import Security
#if canImport(UIKit)
import UIKit
#endif

/// Entitlements returned beside a server-issued signed token.
public struct LicenseEntitlements: Codable, Equatable, Sendable {
    public let v: Int
    public let tier: String?
    public let features: [String]

    public init(v: Int = 1, tier: String? = nil, features: [String] = []) {
        self.v = v
        self.tier = tier
        self.features = features
    }

    public var isPro: Bool { tier == "pro" }
    public func hasFeature(_ feature: String) -> Bool { features.contains(feature) }
}

/// Raw activation response. Server response fields are optional because legacy endpoints omit
/// newer lifecycle metadata; authorization always comes from `signedToken` when present.
public struct LicenseServerResponse: Codable, Equatable, Sendable {
    public var ok: Bool?
    public var status: String?
    public var message: String?
    public var activationToken: String?
    public var licenseId: String?
    public var validUntil: String?
    public var signedToken: String?
    public var licenseKind: String?
    public var entitlementStage: String?
    public var licenseIssuedAt: String?
    public var confirmationDueAt: String?
    public var entitlements: LicenseEntitlements?
    public var lease: LicenseLeaseEnvelope?
    public var error: String?
    /// Optional elaboration some endpoints send beside `error`.
    public var detail: String?

    public init(
        ok: Bool? = nil, status: String? = nil, message: String? = nil, activationToken: String? = nil,
        licenseId: String? = nil, validUntil: String? = nil, signedToken: String? = nil,
        licenseKind: String? = nil, entitlementStage: String? = nil, licenseIssuedAt: String? = nil,
        confirmationDueAt: String? = nil, entitlements: LicenseEntitlements? = nil,
        lease: LicenseLeaseEnvelope? = nil, error: String? = nil, detail: String? = nil,
    ) {
        self.ok = ok; self.status = status; self.message = message; self.activationToken = activationToken
        self.licenseId = licenseId; self.validUntil = validUntil; self.signedToken = signedToken
        self.licenseKind = licenseKind; self.entitlementStage = entitlementStage
        self.licenseIssuedAt = licenseIssuedAt; self.confirmationDueAt = confirmationDueAt
        self.entitlements = entitlements; self.lease = lease; self.error = error; self.detail = detail
    }

    /// The server's `error` text with its `detail` appended when present (`"error: detail"`), or
    /// whichever of the two is non-empty; nil when neither is.
    public var errorText: String? { LicenseServerResponse.errorText(error: error, detail: detail) }

    static func errorText(error: String?, detail: String?) -> String? {
        let parts = [error, detail].compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: ": ")
    }
}

/// Device-local cached license. The raw key is retained only when the server did not issue an
/// activation token; once a token exists, `key` is cleared.
public struct StoredLicense: Codable, Equatable, Sendable {
    public var status: String
    public var deviceId: String
    public var key: String?
    public var activationToken: String?
    public var licenseId: String?
    public var validUntil: String?
    public var entitlements: LicenseEntitlements?
    public var message: String?
    public var activatedAt: String?
    public var lastRefreshedAt: String?
    public var signedToken: String?
    public var licenseKind: String?
    public var entitlementStage: String?
    public var licenseIssuedAt: String?
    public var confirmationDueAt: String?
    public var leaseId: String?
    public var leaseGeneration: Int?
    public var leaseExpiresAtUnix: Int64?
    public var leaseGraceSeconds: Int64?
    public var entitlementScope: String?

    public init(
        status: String = "none", deviceId: String, key: String? = nil,
        activationToken: String? = nil, licenseId: String? = nil, validUntil: String? = nil,
        entitlements: LicenseEntitlements? = nil, message: String? = nil,
        activatedAt: String? = nil, lastRefreshedAt: String? = nil, signedToken: String? = nil,
        licenseKind: String? = nil, entitlementStage: String? = nil,
        licenseIssuedAt: String? = nil, confirmationDueAt: String? = nil,
        leaseId: String? = nil, leaseGeneration: Int? = nil,
        leaseExpiresAtUnix: Int64? = nil, leaseGraceSeconds: Int64? = nil,
        entitlementScope: String? = nil,
    ) {
        self.status = status; self.deviceId = deviceId; self.key = key
        self.activationToken = activationToken; self.licenseId = licenseId
        self.validUntil = validUntil; self.entitlements = entitlements; self.message = message
        self.activatedAt = activatedAt; self.lastRefreshedAt = lastRefreshedAt
        self.signedToken = signedToken; self.licenseKind = licenseKind
        self.entitlementStage = entitlementStage; self.licenseIssuedAt = licenseIssuedAt
        self.confirmationDueAt = confirmationDueAt; self.leaseId = leaseId
        self.leaseGeneration = leaseGeneration; self.leaseExpiresAtUnix = leaseExpiresAtUnix
        self.leaseGraceSeconds = leaseGraceSeconds; self.entitlementScope = entitlementScope
    }

    /// Signature, app, device and expiry check of the stored token. Does NOT look at `status`; use
    /// ``authorizedClaims(publicKeyBase64url:app:deviceId:now:requireActiveStatus:acceptedTokenVersions:)``
    /// to gate features. `acceptedTokenVersions` is passed to ``LicenseVerifier/verify(token:app:deviceId:now:acceptedVersions:)``.
    public func verifiedClaims(
        publicKeyBase64url: String, app: String, now: Int = Int(Date().timeIntervalSince1970),
        acceptedTokenVersions: Set<Int> = LicenseVerifier.defaultAcceptedVersions
    ) -> LicenseClaims? {
        guard let signedToken else { return nil }
        let verificationDevice = leaseId == nil ? deviceId : LicenseActivationClient.deviceIDHash(deviceId)
        guard let claims = try? LicenseVerifier(publicKeyBase64url: publicKeyBase64url).verify(
            token: signedToken, app: app, deviceId: verificationDevice, now: now, acceptedVersions: acceptedTokenVersions
        ) else { return nil }
        guard entitlementScope == nil || entitlementScope == claims.scope else { return nil }
        return claims
    }

    public func isProCandidate(
        publicKeyBase64url: String, app: String, now: Int = Int(Date().timeIntervalSince1970),
        acceptedTokenVersions: Set<Int> = LicenseVerifier.defaultAcceptedVersions
    ) -> Bool {
        verifiedClaims(publicKeyBase64url: publicKeyBase64url, app: app, now: now, acceptedTokenVersions: acceptedTokenVersions)?.isPro == true
    }
}

/// Signed renewable device lease returned by lease endpoints.
public struct LicenseLeaseEnvelope: Codable, Equatable, Sendable {
    public let leaseId: String
    public let deviceIdHash: String
    public let pool: String
    public let issuedAt: String
    public let expiresAt: String
    public let graceSeconds: Int64
    public let generation: Int
    /// The signed entitlement token. Optional: a continuity-bridge response may carry an envelope
    /// without one. It is not part of the signed canonical JSON, and a record built from an envelope
    /// without it has no `signedToken`, so it never authorizes Pro (fail closed).
    public let entitlement: String?
    public let signature: String?
    public let entitlementScope: String?

    public init(
        leaseId: String, deviceIdHash: String, pool: String, issuedAt: String, expiresAt: String,
        graceSeconds: Int64, generation: Int, entitlement: String?, signature: String? = nil,
        entitlementScope: String? = nil,
    ) {
        self.leaseId = leaseId; self.deviceIdHash = deviceIdHash; self.pool = pool
        self.issuedAt = issuedAt; self.expiresAt = expiresAt; self.graceSeconds = graceSeconds
        self.generation = generation; self.entitlement = entitlement; self.signature = signature
        self.entitlementScope = entitlementScope
    }

    public func canonicalJSON() -> String {
        func quote(_ value: String) -> String {
            var result = "\""
            for scalar in value.unicodeScalars {
                switch scalar {
                case "\"": result += "\\\""
                case "\\": result += "\\\\"
                case "\n": result += "\\n"
                case "\r": result += "\\r"
                case "\t": result += "\\t"
                default:
                    if scalar.value < 0x20 { result += String(format: "\\u%04x", scalar.value) }
                    else { result.unicodeScalars.append(scalar) }
                }
            }
            return result + "\""
        }
        var result = "{\"leaseId\":\(quote(leaseId)),\"deviceIdHash\":\(quote(deviceIdHash)),\"pool\":\(quote(pool)),\"issuedAt\":\(quote(issuedAt)),\"expiresAt\":\(quote(expiresAt)),\"graceSeconds\":\(graceSeconds),\"generation\":\(generation)"
        if let entitlementScope { result += ",\"entitlementScope\":\(quote(entitlementScope))" }
        return result + "}"
    }

    /// Fail-closed scope check: a missing scope (legacy full rights) passes; a present scope must be
    /// in `allowed`, otherwise ``LicenseScopeError/unsupportedEntitlementScope(_:)`` is thrown.
    public func validateEntitlementScope(allowed: Set<String> = LicenseEntitlementScope.mobileCompatible) throws {
        guard let entitlementScope else { return }
        guard allowed.contains(entitlementScope) else { throw LicenseScopeError.unsupportedEntitlementScope(entitlementScope) }
    }

    public func verifySignature(publicKey: Curve25519.Signing.PublicKey) throws {
        guard let signature, let bytes = Data(base64urlEncoded: signature) else {
            throw LicenseError.malformedLeaseSignature
        }
        guard publicKey.isValidSignature(bytes, for: Data(canonicalJSON().utf8)) else {
            throw LicenseError.badLeaseSignature
        }
    }
}

/// Entitlement scopes the server signs into tokens (`entitlement_scope`) and lease envelopes
/// (`entitlementScope`). Absent means legacy full rights.
public enum LicenseEntitlementScope {
    public static let full = "full"
    public static let mobile = "mobile"
    /// Scopes an iOS/iPadOS app may consume.
    public static let mobileCompatible: Set<String> = [full, mobile]
}

/// Raised by ``LicenseLeaseEnvelope/validateEntitlementScope(allowed:)``. A separate type from
/// ``LicenseError`` so adding it cannot break an exhaustive `switch` over ``LicenseError``.
public enum LicenseScopeError: Error, Equatable, Sendable, LocalizedError {
    case unsupportedEntitlementScope(String)

    public var errorDescription: String? {
        switch self {
        case .unsupportedEntitlementScope: return "The device lease has an unsupported entitlement scope."
        }
    }
}

/// Raised by ``LicenseActivationClient/activateLease(key:deviceLabel:)`` when a 2xx response lacks
/// its lease or activation token but the server explained why (`error`, plus `detail` when present).
/// Without that text the generic ``LicenseError/invalidResponse`` is thrown, as before. A separate
/// type so adding it cannot break an exhaustive `switch` over ``LicenseError``.
public enum LicenseResponseError: Error, Equatable, Sendable, LocalizedError {
    case incomplete(String)

    public var errorDescription: String? {
        switch self {
        case let .incomplete(text): return text
        }
    }
}

public enum LicenseStorageError: Error, Equatable, Sendable {
    case keychain(OSStatus)
    case encoding
}

/// Async storage seam; production apps use `KeychainLicenseStore`, tests inject memory storage.
public protocol LicenseStore: Sendable {
    func load() async -> StoredLicense?
    func save(_ license: StoredLicense) async throws
    func remove() async throws
}

public final class InMemoryLicenseStore: LicenseStore, @unchecked Sendable {
    private let lock = NSLock()
    private var value: StoredLicense?

    public init(_ value: StoredLicense? = nil) { self.value = value }
    public func load() async -> StoredLicense? { lock.withLock { value } }
    public func save(_ license: StoredLicense) async throws { lock.withLock { value = license } }
    public func remove() async throws { lock.withLock { value = nil } }
}

/// Device-only Keychain persistence. `service` is caller-scoped so app and extension records do
/// not collide; no synchronizable item is used for a license or device id.
public struct KeychainLicenseStore: LicenseStore, Sendable {
    public let service: String
    public let account: String

    public init(service: String = "org.rightkit.license", account: String = "activation") {
        self.service = service; self.account = account
    }

    /// Serialises every Keychain operation of every store in the process. The bodies are
    /// synchronous (no await), so holding the lock never spans a suspension point; this closes the
    /// read-then-rewrite window in which a concurrent `remove()` could be undone.
    private static let lock = NSLock()

    public func load() async -> StoredLicense? {
        let found: Data? = Self.lock.withLock {
            guard let data = read(service: service, account: account) else { return nil }
            migrateAccessibility(service: service, account: account)
            return data
        }
        guard let data = found else { return nil }
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try? decoder.decode(StoredLicense.self, from: data)
    }

    public func save(_ license: StoredLicense) async throws {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        guard let data = try? encoder.encode(license) else { throw LicenseStorageError.encoding }
        try Self.lock.withLock { try write(data, service: service, account: account) }
    }

    public func remove() async throws {
        try Self.lock.withLock { try delete(service: service, account: account) }
    }

    public func deviceID(account: String = "device-id") -> String {
        Self.lock.withLock { deviceIDLocked(account: account) }
    }

    private func deviceIDLocked(account: String) -> String {
        if let data = read(service: service, account: account), let value = String(data: data, encoding: .utf8), !value.isEmpty {
            migrateAccessibility(service: service, account: account)
            return value
        }
        let value = UUID().uuidString
        try? write(Data(value.utf8), service: service, account: account)
        return value
    }

    /// Rewrites only the accessibility attribute of an existing item (update-only, never add), so a
    /// record deleted in the meantime is never resurrected by the migration.
    private func migrateAccessibility(service: String, account: String) {
        let attributes: [String: Any] = [kSecAttrAccessible as String: Self.deviceOnlyAccessibility]
        _ = SecItemUpdate(identity(service: service, account: account) as CFDictionary, attributes as CFDictionary)
    }

    /// Device-only accessibility: the item is excluded from backups and never migrates to another
    /// device, so a restored backup cannot carry a prior device binding.
    static var deviceOnlyAccessibility: CFString { kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly }

    func identity(service: String, account: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: account, kSecAttrSynchronizable as String: false]
    }

    private func read(service: String, account: String) -> Data? {
        var query = identity(service: service, account: account)
        query[kSecReturnData as String] = true; query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess else { return nil }
        return item as? Data
    }

    /// Update-or-add; on update the accessibility attribute is rewritten so existing records that
    /// predate device-only accessibility are migrated in place.
    private func write(_ data: Data, service: String, account: String) throws {
        let query = identity(service: service, account: account)
        let attributes: [String: Any] = [kSecValueData as String: data, kSecAttrAccessible as String: Self.deviceOnlyAccessibility]
        let status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if status == errSecItemNotFound {
            var item = query; item.merge(attributes) { $1 }
            let added = SecItemAdd(item as CFDictionary, nil)
            guard added == errSecSuccess else { throw LicenseStorageError.keychain(added) }
        } else if status != errSecSuccess { throw LicenseStorageError.keychain(status) }
    }

    private func delete(service: String, account: String) throws {
        let status = SecItemDelete(identity(service: service, account: account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw LicenseStorageError.keychain(status) }
    }
}

/// Where the renewable device-lease endpoints (`.../pro/<app>/devices/lease[/renew|/remove]`) live
/// relative to ``LicenseClientConfiguration/baseURL`` (the legacy `/activate`, `/refresh`,
/// `/deactivate` base, e.g. `https://host/v1/licenses`).
public enum LicenseLeasePathStyle: Equatable, Sendable {
    /// The parent of `baseURL`: `https://host/v1/licenses` → `https://host/v1/pro/<app>/devices/lease`.
    /// This is the resolved form of RightKit's original `baseURL/../pro/<app>/...` (the default).
    case siblingOfBase
    /// Rooted at `baseURL`'s origin under `apiPrefix`, ignoring `baseURL`'s path:
    /// `.originRooted(apiPrefix: "/v1")` → `https://host/v1/pro/<app>/devices/lease`, which is how
    /// ScrapeRight's in-app client spells its absolute lease URLs.
    case originRooted(apiPrefix: String)
}

/// What ``LicenseActivationClient/refresh()`` does when the refresh request fails.
public enum LicenseRefreshErrorPolicy: Equatable, Sendable {
    /// Default (RightKit's original behaviour): a 4xx status, transport failure or unreadable
    /// response throws; any other HTTP status (5xx, 3xx…) returns the current record unchanged.
    case keepRecordOnServerError
    /// Every failure throws, including 5xx, so the caller can show it.
    case surfaceAll
    /// Every failure returns the current record unchanged (ScrapeRight's in-app client behaviour).
    case keepRecordOnAnyError
}

public struct LicenseClientConfiguration: Sendable {
    public let app: String
    public let baseURL: URL
    public let appVersion: String
    public let deviceID: @Sendable () -> String
    public let session: URLSession
    public let publicKeyBase64url: String?
    /// Sent as `deviceClass` on `activate` when set (ScrapeRight and HeardRight send `"iphone"`).
    /// Default nil: the field is omitted, as before.
    public let deviceClass: String?
    /// Whether lease issue/renew/remove bodies carry `appKey`. Default true (RightKit's original
    /// body); ScrapeRight's client omits it because the app is already in the lease path.
    public let includeAppKeyInLeaseBody: Bool
    /// See ``LicenseLeasePathStyle``. Default ``LicenseLeasePathStyle/siblingOfBase``.
    public let leasePathStyle: LicenseLeasePathStyle
    /// See ``LicenseRefreshErrorPolicy``. Default ``LicenseRefreshErrorPolicy/keepRecordOnServerError``.
    public let refreshErrorPolicy: LicenseRefreshErrorPolicy
    /// Whether Pro also requires the stored record's `status == "active"`. Default true (desktop
    /// `license.rs` requires it too). False: a verifying signed token alone is enough, as in
    /// ScrapeRight's original client. Used by ``LicenseController`` and ``StoredLicenseResolver``.
    public let requireActiveStatus: Bool
    /// Token `v` claims accepted. Default `[2]`; ScrapeRight passes `[1, 2]`. See
    /// ``LicenseVerifier/verify(token:app:deviceId:now:acceptedVersions:)`` for what v1 lacks.
    public let acceptedTokenVersions: Set<Int>
    /// Explicit refresh statuses (compared case-insensitively) that clear the signed token,
    /// entitlements and `validUntil`. Default ``defaultRevokingStatuses`` (`removed`, `revoked`).
    /// Any other status, and a 2xx body with neither `status` nor `ok`, keeps the token.
    public let revokingStatuses: Set<String>
    /// Envelope `entitlementScope`s `activateLease`/`renewLease` accept; anything else throws
    /// ``LicenseScopeError`` before storage. Default ``LicenseEntitlementScope/mobileCompatible``.
    public let allowedEntitlementScopes: Set<String>

    /// The refresh statuses that revoke by default: what ScrapeRight's and HeardRight's shipped iOS
    /// clients and HeardRight's desktop `refresh_license_and_reanchor_inner` treat as explicit
    /// revocation. `invalid` is deliberately absent: none of them clears the token for it.
    public static let defaultRevokingStatuses: Set<String> = ["removed", "revoked"]

    public init(
        app: String, baseURL: URL, appVersion: String = "0.0.0", deviceID: @escaping @Sendable () -> String,
        session: URLSession = .shared, publicKeyBase64url: String? = nil,
        deviceClass: String? = nil, includeAppKeyInLeaseBody: Bool = true,
        leasePathStyle: LicenseLeasePathStyle = .siblingOfBase,
        refreshErrorPolicy: LicenseRefreshErrorPolicy = .keepRecordOnServerError,
        requireActiveStatus: Bool = true,
        acceptedTokenVersions: Set<Int> = LicenseVerifier.defaultAcceptedVersions,
        revokingStatuses: Set<String> = LicenseClientConfiguration.defaultRevokingStatuses,
        allowedEntitlementScopes: Set<String> = LicenseEntitlementScope.mobileCompatible
    ) {
        self.app = app; self.baseURL = baseURL; self.appVersion = appVersion; self.deviceID = deviceID; self.session = session; self.publicKeyBase64url = publicKeyBase64url
        self.deviceClass = deviceClass; self.includeAppKeyInLeaseBody = includeAppKeyInLeaseBody
        self.leasePathStyle = leasePathStyle; self.refreshErrorPolicy = refreshErrorPolicy
        self.requireActiveStatus = requireActiveStatus; self.acceptedTokenVersions = acceptedTokenVersions
        self.revokingStatuses = revokingStatuses; self.allowedEntitlementScopes = allowedEntitlementScopes
    }

    /// The lease endpoint URL for `suffix` (`[]`, `["renew"]` or `["remove"]`), built from URL
    /// components. For ``LicenseLeasePathStyle/siblingOfBase`` it equals the standardized form of
    /// the original `baseURL.appendingPathComponent("../pro/<app>/devices/lease/...")`, except that
    /// it is sent without the `..` segment on the wire.
    func leaseURL(_ suffix: [String]) -> URL {
        var components = URLComponents(url: baseURL, resolvingAgainstBaseURL: true) ?? URLComponents()
        let prefix: [String]
        switch leasePathStyle {
        case .siblingOfBase:
            prefix = Array(components.path.split(separator: "/").map(String.init).dropLast())
        case let .originRooted(apiPrefix):
            prefix = apiPrefix.split(separator: "/").map(String.init)
        }
        components.path = "/" + (prefix + ["pro", app, "devices", "lease"] + suffix).joined(separator: "/")
        components.fragment = nil
        return components.url ?? baseURL
    }
}

public extension LicenseActivationClient {
    typealias Configuration = LicenseClientConfiguration
}

/// Shared license client for activation, refresh, renewable leases and removal. It is transport
/// only: entitlement authority remains `LicenseVerifier` + `AccessResolver` at app composition.
public struct LicenseActivationClient: Sendable {
    public let configuration: LicenseClientConfiguration
    public let store: any LicenseStore

    public init(configuration: LicenseClientConfiguration, store: any LicenseStore) {
        self.configuration = configuration; self.store = store
    }

    public func load() async -> StoredLicense? { await store.load() }

    public func activate(key rawKey: String) async throws -> StoredLicense {
        let key = rawKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !key.isEmpty else { throw LicenseError.transport("Enter a license key first.") }
        var payload = ["app": configuration.app, "licenseKey": key, "deviceId": configuration.deviceID(), "version": configuration.appVersion]
        if let deviceClass = configuration.deviceClass { payload["deviceClass"] = deviceClass }
        let body: LicenseServerResponse = try await post(path: "activate", payload: payload)
        let license = Self.license(from: body, deviceId: configuration.deviceID(), key: key)
        try await store.save(license)
        return license
    }

    /// Refreshes a legacy (activation-token) record. Failures follow
    /// ``LicenseClientConfiguration/refreshErrorPolicy``. Lease-bound records renew through
    /// ``renewLease()`` instead (``LicenseController/refresh(silent:)`` routes them).
    public func refresh() async throws -> StoredLicense? {
        guard let current = await store.load(), let token = current.activationToken, !token.isEmpty else { return await store.load() }
        let body: LicenseServerResponse
        do {
            body = try await post(path: "refresh", payload: ["app": configuration.app, "activationToken": token, "deviceId": current.deviceId])
        } catch {
            switch configuration.refreshErrorPolicy {
            case .surfaceAll: throw error
            case .keepRecordOnAnyError: return current
            case .keepRecordOnServerError:
                guard let error = error as? LicenseError else { return current }
                if case .serverStatus(let status, _) = error, !(400..<500).contains(status) { return current }
                throw error
            }
        }
        let updated = Self.merged(
            current: current, body: body,
            refreshed: Self.license(from: body, deviceId: current.deviceId, key: current.key, activatedAt: current.activatedAt),
            revokingStatuses: configuration.revokingStatuses
        )
        try await store.save(updated)
        return updated
    }

    public func deactivate() async throws {
        if let current = await store.load(), let token = current.activationToken {
            _ = try? await post(path: "deactivate", payload: ["app": configuration.app, "activationToken": token]) as LicenseServerResponse
        }
        try await store.remove()
    }

    /// Deactivates a legacy activation by its original token, for a durable pending-removal queue.
    /// Never loads or touches the stored license. Returns only after the server accepted the
    /// call; any failure throws, so the caller acknowledges its queue entry on success alone.
    public func deactivate(activationToken: String) async throws {
        _ = try await post(path: "deactivate", payload: ["app": configuration.app, "activationToken": activationToken]) as LicenseServerResponse
    }

    public func activateLease(key: String, deviceLabel: String? = nil) async throws -> StoredLicense {
        let deviceHash = Self.deviceIDHash(configuration.deviceID())
        var payload = leaseBody(["licenseKey": key.trimmingCharacters(in: .whitespacesAndNewlines), "deviceIdHash": deviceHash, "platform": Self.platform()])
        if let deviceLabel { payload["deviceLabel"] = deviceLabel }
        let body: LicenseServerResponse = try await post(url: configuration.leaseURL([]), payload: payload)
        guard let lease = body.lease, let token = body.activationToken else {
            if let text = body.errorText { throw LicenseResponseError.incomplete(text) }
            throw LicenseError.invalidResponse
        }
        // Signature, then scope (fail closed), both before anything is stored.
        let license = try verifiedLicense(from: lease, activationToken: token, deviceId: configuration.deviceID())
        try await store.save(license)
        return license
    }

    /// Renews the stored lease. Signature and scope are checked before anything is stored: an
    /// unsupported `entitlementScope` throws ``LicenseScopeError`` and leaves storage untouched; any
    /// other failure except 404/410 returns the current record unchanged; 404/410 stores the record
    /// as `revoked` with its token cleared.
    public func renewLease() async throws -> StoredLicense? {
        try await renewLeaseOutcome().license
    }

    /// ``renewLease()`` plus whether the server answered 404/410 (this device's lease is gone).
    func renewLeaseOutcome() async throws -> (license: StoredLicense?, deviceRemoved: Bool) {
        guard let current = await store.load(), let leaseId = current.leaseId else { return (await store.load(), false) }
        let payload = leaseBody(["deviceIdHash": Self.deviceIDHash(current.deviceId), "leaseId": leaseId])
        do {
            let body: LicenseServerResponse = try await post(url: configuration.leaseURL(["renew"]), payload: payload)
            guard let lease = body.lease else { return (current, false) }
            let renewed = try verifiedLicense(from: lease, activationToken: current.activationToken ?? "", deviceId: current.deviceId, activatedAt: current.activatedAt)
            try await store.save(renewed); return (renewed, false)
        } catch let error as LicenseScopeError {
            throw error
        } catch let error as LicenseError {
            if case .serverStatus(let status, _) = error, status == 404 || status == 410 {
                var revoked = current; revoked.status = "revoked"; revoked.signedToken = nil; revoked.entitlements = nil; revoked.validUntil = nil
                try await store.save(revoked); return (revoked, true)
            }
            return (current, false)
        } catch { return (current, false) }
    }

    /// Verifies the envelope signature against the configured key, then its scope against
    /// ``LicenseClientConfiguration/allowedEntitlementScopes``, then builds the record. Stores nothing.
    private func verifiedLicense(from lease: LicenseLeaseEnvelope, activationToken: String, deviceId: String, activatedAt: String? = nil) throws -> StoredLicense {
        try lease.verifySignature(publicKey: try leaseKey())
        try lease.validateEntitlementScope(allowed: configuration.allowedEntitlementScopes)
        return Self.license(from: lease, activationToken: activationToken, deviceId: deviceId, activatedAt: activatedAt)
    }

    public func removeLease() async throws {
        guard let current = await store.load(), let leaseId = current.leaseId else {
            try await deactivate()
            return
        }
        let payload = leaseBody(["deviceIdHash": Self.deviceIDHash(current.deviceId), "leaseId": leaseId])
        _ = try await post(url: configuration.leaseURL(["remove"]), payload: payload) as LicenseServerResponse
        try await store.remove()
    }

    /// Removes a lease by its original identity, for a durable pending-removal queue whose entry
    /// must survive device-identity or account changes. Sends exactly `deviceIdHash` and `leaseId`
    /// to the configured app's lease-removal endpoint: it never derives a hash from the current
    /// device, never loads or substitutes the stored license, and never touches the local
    /// activation. Returns only after the server accepted the removal; any failure throws, so the
    /// caller acknowledges its queue entry on success alone.
    public func removeLease(deviceIdHash: String, leaseId: String) async throws {
        let payload = leaseBody(["deviceIdHash": deviceIdHash, "leaseId": leaseId])
        _ = try await post(url: configuration.leaseURL(["remove"]), payload: payload) as LicenseServerResponse
    }

    /// Adds `appKey` to a lease body unless the configuration opted out.
    private func leaseBody(_ fields: [String: String]) -> [String: String] {
        guard configuration.includeAppKeyInLeaseBody else { return fields }
        var body = fields; body["appKey"] = configuration.app
        return body
    }

    public static func license(from body: LicenseServerResponse, deviceId: String, key: String?, activatedAt: String? = nil) -> StoredLicense {
        StoredLicense(status: body.status ?? (body.ok == true ? "active" : "invalid"), deviceId: deviceId, key: body.activationToken == nil ? key : nil, activationToken: body.activationToken, licenseId: body.licenseId, validUntil: body.validUntil, entitlements: body.entitlements, message: body.message, activatedAt: activatedAt ?? ISO8601DateFormatter().string(from: Date()), lastRefreshedAt: ISO8601DateFormatter().string(from: Date()), signedToken: body.signedToken, licenseKind: body.licenseKind, entitlementStage: body.entitlementStage, licenseIssuedAt: body.licenseIssuedAt, confirmationDueAt: body.confirmationDueAt)
    }

    public static func deviceIDHash(_ deviceID: String) -> String {
        SHA256.hash(data: Data(deviceID.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    private static func platform() -> String {
        #if canImport(UIKit)
        return UIDevice.current.userInterfaceIdiom == .pad ? "ipados" : "ios"
        #else
        return "ios"
        #endif
    }

    private func leaseKey() throws -> Curve25519.Signing.PublicKey {
        guard let key = configuration.publicKeyBase64url else { throw LicenseError.malformedPublicKey }
        return try Curve25519.Signing.PublicKey(rawRepresentation: Data(base64urlEncoded: key) ?? Data())
    }

    private func post<T: Decodable>(path: String, payload: [String: String]) async throws -> T {
        try await post(url: configuration.baseURL.appendingPathComponent(path), payload: payload)
    }

    private func post<T: Decodable>(url: URL, payload: [String: String]) async throws -> T {
        var request = URLRequest(url: url); request.httpMethod = "POST"; request.timeoutInterval = 18
        request.setValue("application/json", forHTTPHeaderField: "Accept"); request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: payload)
        let (data, response): (Data, URLResponse)
        do { (data, response) = try await configuration.session.data(for: request) } catch { throw LicenseError.transport(error.localizedDescription) }
        guard let http = response as? HTTPURLResponse else { throw LicenseError.invalidResponse }
        guard (200..<300).contains(http.statusCode) else { throw LicenseError.serverStatus(http.statusCode, Self.errorMessage(from: data)) }
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let decoded = try? decoder.decode(T.self, from: data) else { throw LicenseError.invalidResponse }
        return decoded
    }

    private static func errorMessage(from data: Data) -> String {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return "" }
        if let message = object["message"] as? String { return message }
        return LicenseServerResponse.errorText(error: object["error"] as? String, detail: object["detail"] as? String) ?? ""
    }

    /// Refresh merge. Only an explicit status in `revokingStatuses` clears the signed token,
    /// entitlements and `validUntil`. A 2xx body with neither `status` nor `ok` says nothing about
    /// the license, so it keeps the token AND the current status (it never downgrades the record);
    /// every other status is stored as sent, keeping the token.
    static func merged(
        current: StoredLicense, body: LicenseServerResponse, refreshed: StoredLicense, revokingStatuses: Set<String>
    ) -> StoredLicense {
        let silent = body.status == nil && body.ok == nil
        let revoked = !silent && Set(revokingStatuses.map { $0.lowercased() }).contains(refreshed.status.lowercased())
        var value = current
        value.status = silent ? current.status : refreshed.status
        value.message = refreshed.message; value.lastRefreshedAt = refreshed.lastRefreshedAt
        if !revoked { value.validUntil = refreshed.validUntil ?? current.validUntil; value.entitlements = refreshed.entitlements ?? current.entitlements; value.signedToken = refreshed.signedToken ?? current.signedToken }
        else { value.validUntil = nil; value.entitlements = nil; value.signedToken = nil }
        value.activationToken = refreshed.activationToken ?? current.activationToken
        value.licenseId = refreshed.licenseId ?? current.licenseId
        value.licenseKind = refreshed.licenseKind ?? current.licenseKind; value.entitlementStage = refreshed.entitlementStage ?? current.entitlementStage
        value.licenseIssuedAt = refreshed.licenseIssuedAt ?? current.licenseIssuedAt; value.confirmationDueAt = refreshed.confirmationDueAt ?? current.confirmationDueAt
        return value
    }

    /// Builds the stored record for a signed lease envelope — the same builder ``activateLease(key:deviceLabel:)``
    /// and ``renewLease()`` use — for app bridge and lease flows that receive an envelope elsewhere.
    ///
    /// Fails closed, in this order: the envelope signature must verify against `publicKeyBase64url`
    /// (``LicenseLeaseEnvelope/verifySignature(publicKey:)``), then its `entitlementScope` must pass
    /// ``LicenseLeaseEnvelope/validateEntitlementScope(allowed:)``. Nothing is persisted.
    public static func license(
        fromLease lease: LicenseLeaseEnvelope, activationToken: String, deviceId: String,
        publicKeyBase64url: String, key: String? = nil, activatedAt: String? = nil,
        allowedScopes: Set<String> = LicenseEntitlementScope.mobileCompatible
    ) throws -> StoredLicense {
        guard let raw = Data(base64urlEncoded: publicKeyBase64url),
              let publicKey = try? Curve25519.Signing.PublicKey(rawRepresentation: raw)
        else { throw LicenseError.malformedPublicKey }
        try lease.verifySignature(publicKey: publicKey)
        try lease.validateEntitlementScope(allowed: allowedScopes)
        return license(from: lease, activationToken: activationToken, deviceId: deviceId, key: key, activatedAt: activatedAt)
    }

    private static func license(from lease: LicenseLeaseEnvelope, activationToken: String, deviceId: String, key: String? = nil, activatedAt: String? = nil) -> StoredLicense {
        StoredLicense(status: "active", deviceId: deviceId, key: key, activationToken: activationToken, validUntil: lease.expiresAt, activatedAt: activatedAt ?? ISO8601DateFormatter().string(from: Date()), lastRefreshedAt: ISO8601DateFormatter().string(from: Date()), signedToken: lease.entitlement, leaseId: lease.leaseId, leaseGeneration: lease.generation, leaseExpiresAtUnix: ISO8601DateFormatter().date(from: lease.expiresAt).map { Int64($0.timeIntervalSince1970) }, leaseGraceSeconds: lease.graceSeconds, entitlementScope: lease.entitlementScope)
    }
}

public extension LicenseActivationClient {
    /// DISPLAY ONLY. Reads the mutable cached status/`validUntil`; it is not proof of entitlement
    /// and must never gate a feature. Use ``StoredLicense/authorizedClaims(publicKeyBase64url:app:deviceId:now:)``.
    static func displayIsLicensed(_ license: StoredLicense?, graceDays: Int = 7, now: Date = Date()) -> Bool {
        guard let license, license.status == "active" else { return false }
        guard let validUntil = license.validUntil else { return true }
        guard let expiry = ISO8601DateFormatter().date(from: validUntil) else { return license.licenseKind != "trial" && license.entitlementStage != "provisional" }
        let exact = license.licenseKind == "trial" || license.entitlementStage == "provisional"
        return now < expiry.addingTimeInterval(Double(exact ? 0 : graceDays) * 86_400)
    }
}

public struct LicenseRequirement: Equatable, Sendable {
    public var tier: String?
    public var feature: String?
    public var allFeatures: [String]
    public var anyFeatures: [String]

    public init(tier: String? = nil, feature: String? = nil, allFeatures: [String] = [], anyFeatures: [String] = []) {
        self.tier = tier; self.feature = feature; self.allFeatures = allFeatures; self.anyFeatures = anyFeatures
    }

    public static var pro: Self { Self(tier: "pro") }
}

// MARK: Display-only raw helpers (cached, unsigned, mutable data: never authorization)

public func displayIsLicensed(_ license: StoredLicense?, graceDays: Int = 7, now: Date = Date()) -> Bool {
    LicenseActivationClient.displayIsLicensed(license, graceDays: graceDays, now: now)
}
public func displayTier(_ license: StoredLicense?) -> String? { license?.entitlements?.tier }
public func displayHasFeature(_ license: StoredLicense?, _ feature: String) -> Bool { license?.entitlements?.hasFeature(feature) == true }
public func displayIsPro(_ license: StoredLicense?, graceDays: Int = 7, now: Date = Date()) -> Bool {
    displayIsLicensed(license, graceDays: graceDays, now: now) && displayTier(license) == "pro"
}

// MARK: Authorization (derived only from signed, verified claims)

public extension StoredLicense {
    /// The only authorization source: a signed token that verifies against `publicKeyBase64url`
    /// and is bound to `app`, to this record's device (and to the live `deviceId` the caller must supply
    /// (the cached record device ID alone is never trusted), and to an unexpired `exp`. The record must also still be
    /// `active`; a missing, unsigned, forged, wrong-app, wrong-device or expired token yields nil.
    ///
    /// - Parameters:
    ///   - requireActiveStatus: default true, the strict gate (desktop `license.rs` requires
    ///     `status == "active"` too). False drops only the status check: a verifying token is
    ///     enough, as in ScrapeRight's original client. Signature, app, device and expiry still apply.
    ///   - acceptedTokenVersions: token `v` claims accepted; default `[2]`.
    func authorizedClaims(
        publicKeyBase64url: String, app: String, deviceId expectedDeviceId: String,
        now: Int = Int(Date().timeIntervalSince1970),
        requireActiveStatus: Bool = true,
        acceptedTokenVersions: Set<Int> = LicenseVerifier.defaultAcceptedVersions
    ) -> LicenseClaims? {
        if requireActiveStatus { guard status == "active" else { return nil } }
        guard !expectedDeviceId.isEmpty, expectedDeviceId == deviceId else { return nil }
        return verifiedClaims(publicKeyBase64url: publicKeyBase64url, app: app, now: now, acceptedTokenVersions: acceptedTokenVersions)
    }

    /// ``authorizedClaims(publicKeyBase64url:app:deviceId:now:requireActiveStatus:acceptedTokenVersions:)``
    /// with the key, app, live device id and knobs taken from `configuration`. Nil without a key.
    func authorizedClaims(
        configuration: LicenseClientConfiguration, now: Int = Int(Date().timeIntervalSince1970)
    ) -> LicenseClaims? {
        guard let key = configuration.publicKeyBase64url else { return nil }
        return authorizedClaims(
            publicKeyBase64url: key, app: configuration.app, deviceId: configuration.deviceID(), now: now,
            requireActiveStatus: configuration.requireActiveStatus, acceptedTokenVersions: configuration.acceptedTokenVersions
        )
    }
}

public extension LicenseClaims {
    /// Fail closed: an empty requirement, an unknown tier name, or a missing feature is denied.
    func satisfies(_ requirement: LicenseRequirement) -> Bool {
        if let requiredTier = requirement.tier {
            let rank: [String: Int] = ["free": 0, "plus": 1, "pro": 2]
            guard let actual = rank[tier], let needed = rank[requiredTier], actual >= needed else { return false }
        }
        if let feature = requirement.feature, !hasFeature(feature) { return false }
        if !requirement.allFeatures.allSatisfy({ hasFeature($0) }) { return false }
        if !requirement.anyFeatures.isEmpty && !requirement.anyFeatures.contains(where: { hasFeature($0) }) { return false }
        return requirement.tier != nil || requirement.feature != nil || !requirement.allFeatures.isEmpty || !requirement.anyFeatures.isEmpty
    }
}

public func isPro(
    _ license: StoredLicense?, publicKeyBase64url: String, app: String, deviceId: String, now: Int = Int(Date().timeIntervalSince1970),
    requireActiveStatus: Bool = true, acceptedTokenVersions: Set<Int> = LicenseVerifier.defaultAcceptedVersions
) -> Bool {
    license?.authorizedClaims(
        publicKeyBase64url: publicKeyBase64url, app: app, deviceId: deviceId, now: now,
        requireActiveStatus: requireActiveStatus, acceptedTokenVersions: acceptedTokenVersions
    )?.isPro == true
}

public func satisfiesRequirement(
    _ license: StoredLicense?, _ requirement: LicenseRequirement, publicKeyBase64url: String, app: String, deviceId: String,
    now: Int = Int(Date().timeIntervalSince1970),
    requireActiveStatus: Bool = true, acceptedTokenVersions: Set<Int> = LicenseVerifier.defaultAcceptedVersions
) -> Bool {
    license?.authorizedClaims(
        publicKeyBase64url: publicKeyBase64url, app: app, deviceId: deviceId, now: now,
        requireActiveStatus: requireActiveStatus, acceptedTokenVersions: acceptedTokenVersions
    )?.satisfies(requirement) == true
}

/// Small resolver seam apps can inject beside `RightKitPurchases.AccessResolver`.
public protocol LicenseEntitlementResolving: Sendable {
    func resolve() async -> LicenseClaims?
}

public struct StoredLicenseResolver: LicenseEntitlementResolving, Sendable {
    private let client: LicenseActivationClient
    private let publicKeyBase64url: String
    private let app: String
    private let now: @Sendable () -> Int

    public init(client: LicenseActivationClient, publicKeyBase64url: String, app: String, now: @escaping @Sendable () -> Int = { Int(Date().timeIntervalSince1970) }) {
        self.client = client; self.publicKeyBase64url = publicKeyBase64url; self.app = app; self.now = now
    }

    public func resolve() async -> LicenseClaims? {
        let license: StoredLicense?
        if let refreshed = try? await client.refresh() {
            license = refreshed
        } else {
            license = await client.load()
        }
        return license?.authorizedClaims(
            publicKeyBase64url: publicKeyBase64url, app: app, deviceId: client.configuration.deviceID(), now: now(),
            requireActiveStatus: client.configuration.requireActiveStatus,
            acceptedTokenVersions: client.configuration.acceptedTokenVersions
        )
    }
}

/// User-facing strings ``LicenseController`` sets. Defaults are RightKit's original strings; apps
/// override for tone or localisation. Errors still show their `localizedDescription`.
public struct LicenseMessages: Equatable, Sendable {
    /// After `activate`/`activateLease`, when the server sent no message (and `activatedPro` does not apply).
    public var activated = "License activated."
    /// When set, replaces the activation message whenever the new record verifies as Pro
    /// (e.g. ScrapeRight's "Pro entitlement restored."). Default nil: no Pro-specific message.
    public var activatedPro: String? = nil
    /// After a non-silent refresh, when the server sent no message (and `refreshedPro` does not apply).
    public var refreshed = "License refreshed."
    /// When set, replaces the refresh message whenever the refreshed record verifies as Pro.
    public var refreshedPro: String? = nil
    /// After `remove()`.
    public var removed = "License removed."
    /// When set, shown after a non-silent refresh whose lease renewal or legacy refresh got HTTP
    /// 404/410 (the server no longer knows this device). Default nil: the original message (the
    /// record's message or `refreshed` for a renewal, the error text for a legacy refresh).
    public var deviceRemoved: String? = nil
    public init() {}
}

#if canImport(Combine)
/// Main-actor license state for app UI.
///
/// Every operation that reads or writes the store or publishes `license` (the initial load,
/// `activate`, `activateLease`, `persist`, `refresh`, `remove`, `retryPendingRemovals`) runs on
/// one serial chain: each starts only after the previous one finished, in call order. A call
/// therefore sees every earlier call's effects, and none of them can publish over it. In
/// particular, once `await remove()` returns, no earlier operation is still running and every
/// later one reads the already-emptied store, so no removed license is ever republished. Waiting
/// on the chain suspends; it never blocks the main actor.
@MainActor
public final class LicenseController: ObservableObject {
    @Published public private(set) var license: StoredLicense?
    @Published public private(set) var isBusy = false
    @Published public private(set) var message = ""

    public let client: LicenseActivationClient
    /// Strings this controller sets; see ``LicenseMessages``.
    public let messages: LicenseMessages
    /// Durable server-removal queue; nil for controllers built with `init(client:)`.
    public let pendingRemovals: PendingLicenseRemovalQueue?
    /// Bridge re-consumption gate; nil for controllers built with `init(client:)`.
    public let bridgeConsumption: BridgeConsumptionGate?
    /// Last operation on the serial chain; the next operation awaits it before it starts.
    private var operationTail: Task<Void, Never>?

    public init(client: LicenseActivationClient, messages: LicenseMessages = LicenseMessages()) {
        self.client = client
        self.messages = messages
        self.pendingRemovals = nil
        self.bridgeConsumption = nil
        operationTail = Task { [weak self] in
            let loaded = await client.load()
            self?.license = loaded
        }
    }

    /// Controller with device-side removal housekeeping: `remove()` suppresses bridge consumption
    /// and queues the server removal durably before any network call, and queued removals are
    /// retried at init, after each successful refresh, and on `retryPendingRemovals()`.
    /// - Parameters:
    ///   - deviceState: device-only storage (never synced).
    ///   - namespace: key prefix; defaults to the client's app id.
    ///   - messages: user-facing strings; defaults are the original ones.
    public init(
        client: LicenseActivationClient, deviceState: any LicenseDeviceStateStoring, namespace: String? = nil,
        messages: LicenseMessages = LicenseMessages()
    ) {
        let scope = namespace ?? client.configuration.app
        self.client = client
        self.messages = messages
        self.pendingRemovals = PendingLicenseRemovalQueue(storage: deviceState, namespace: scope)
        self.bridgeConsumption = BridgeConsumptionGate(storage: deviceState, namespace: scope)
        operationTail = Task { [weak self] in
            let loaded = await client.load()
            guard let self else { return }
            self.license = loaded
            await self.deliverPendingRemovals()
        }
    }

    /// Runs `operation` after every operation enqueued before it, and returns its result. The
    /// operation runs in its own task, so a cancelled caller never leaves it half done.
    private func serialized<T: Sendable>(_ operation: @escaping @MainActor @Sendable () async -> T) async -> T {
        let previous = operationTail
        let task = Task { @MainActor in
            await previous?.value
            return await operation()
        }
        operationTail = Task { @MainActor in _ = await task.value }
        return await task.value
    }

    private func serializedThrowing<T: Sendable>(_ operation: @escaping @MainActor @Sendable () async throws -> T) async throws -> T {
        let previous = operationTail
        let task = Task { @MainActor in
            await previous?.value
            return try await operation()
        }
        operationTail = Task { @MainActor in _ = try? await task.value }
        return try await task.value
    }

    /// Waits until every operation enqueued so far has finished (tests).
    func waitForPendingOperations() async {
        while let tail = operationTail {
            await tail.value
            if operationTail == tail { return }
        }
    }

    /// See `BridgeConsumptionGate`. Always false for controllers built with `init(client:)`.
    public var isBridgeConsumptionSuppressed: Bool { bridgeConsumption?.isSuppressed ?? false }

    /// This device holds a record that is not Pro and has no activation token to recover with;
    /// only an explicit re-activation repairs it. See `StoredLicense.needsReconnect`.
    public var needsReconnect: Bool {
        guard let license else { return false }
        guard let key = client.configuration.publicKeyBase64url else {
            // Without a verification key nothing can be proven Pro: a token-less record is stuck.
            return license.activationToken?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true
        }
        return license.needsReconnect(
            publicKeyBase64url: key, app: client.configuration.app, deviceId: client.configuration.deviceID(),
            requireActiveStatus: client.configuration.requireActiveStatus,
            acceptedTokenVersions: client.configuration.acceptedTokenVersions
        )
    }

    /// Delivers queued server removals. No-op for controllers built with `init(client:)`.
    @discardableResult
    public func retryPendingRemovals() async -> [PendingLicenseRemoval] {
        guard pendingRemovals != nil else { return [] }
        return await serialized { await self.deliverPendingRemovals() }
    }

    /// Unserialized delivery, for use inside an operation already on the chain.
    @discardableResult
    private func deliverPendingRemovals() async -> [PendingLicenseRemoval] {
        guard let pendingRemovals else { return [] }
        return await pendingRemovals.retry(using: client)
    }

    /// Activation through the renewable device-lease endpoint. Clears bridge suppression on
    /// success: an explicit activation is the user's way back after a removal.
    public func activateLease(_ key: String, deviceLabel: String? = nil) async {
        await serialized {
            self.isBusy = true; defer { self.isBusy = false }
            do {
                self.license = try await self.client.activateLease(key: key, deviceLabel: deviceLabel)
                self.bridgeConsumption?.allow()
                self.message = self.outcomeMessage(for: self.license, pro: self.messages.activatedPro, fallback: self.messages.activated)
            } catch { self.message = error.localizedDescription }
        }
    }

    /// Store an already-verified license obtained elsewhere (e.g. a continuity-bridge exchange
    /// the app performed) through the same device-only store as an activation. Does not clear
    /// bridge suppression.
    public func persist(_ verified: StoredLicense) async throws {
        try await serializedThrowing {
            try await self.client.store.save(verified)
            self.license = verified
        }
    }

    public var isProCandidate: Bool { verifiesPro(license) }

    public func activate(_ key: String) async {
        await serialized {
            self.isBusy = true; defer { self.isBusy = false }
            do {
                self.license = try await self.client.activate(key: key)
                self.bridgeConsumption?.allow()
                self.message = self.outcomeMessage(for: self.license, pro: self.messages.activatedPro, fallback: self.messages.activated)
            }
            catch { self.message = error.localizedDescription }
        }
    }

    /// Refreshes the stored record. A lease-bound record (`leaseId != nil`) renews through
    /// ``LicenseActivationClient/renewLease()``; any other record goes through
    /// ``LicenseActivationClient/refresh()``.
    ///
    /// - Parameter silent: for background refreshes (launch, foreground). Leaves `message` and
    ///   `isBusy` untouched (success or failure) and republishes `license` only when it changed.
    public func refresh(silent: Bool = false) async {
        await serialized { await self.performRefresh(silent: silent) }
    }

    private func performRefresh(silent: Bool) async {
        if !silent { isBusy = true }
        defer { if !silent { isBusy = false } }
        do {
            let (fetched, removed) = try await routedRefresh()
            let refreshed = await stampDeviceRemoved(fetched, removed: removed)
            publish(refreshed, silent: silent)
            if !silent { message = refreshMessage(for: refreshed, deviceRemoved: removed) }
            // A completed round trip proves connectivity: flush owed removals now.
            await deliverPendingRemovals()
        } catch {
            if Self.isDeviceRemoved(error), let stamped = await stampDeviceRemoved(license, removed: true) { license = stamped }
            if !silent { message = failureMessage(for: error) }
        }
    }

    /// On a 404/410 the stored record's own `message` carries ``LicenseMessages/deviceRemoved`` too,
    /// so apps that render the record (not only the controller) show the removed-device text.
    private func stampDeviceRemoved(_ record: StoredLicense?, removed: Bool) async -> StoredLicense? {
        guard removed, let text = messages.deviceRemoved, var stamped = record, stamped.message != text else { return record }
        stamped.message = text
        try? await client.store.save(stamped)
        return stamped
    }

    private static func isDeviceRemoved(_ error: Error) -> Bool {
        if case let .serverStatus(status, _)? = error as? LicenseError { return status == 404 || status == 410 }
        return false
    }

    private func refreshMessage(for record: StoredLicense?, deviceRemoved: Bool) -> String {
        if deviceRemoved, let text = messages.deviceRemoved { return text }
        return outcomeMessage(for: record, pro: messages.refreshedPro, fallback: messages.refreshed)
    }

    private func failureMessage(for error: any Error) -> String {
        if let text = messages.deviceRemoved, case let .serverStatus(status, _)? = error as? LicenseError, status == 404 || status == 410 {
            return text
        }
        return error.localizedDescription
    }

    /// Removes the license from this device. With `init(client:)` this is the original
    /// best-effort deactivate. With device state configured, the order is: suppress bridge
    /// consumption, queue the owed server removal durably, delete the local record (Pro ends
    /// immediately, even offline), then attempt delivery; an undelivered removal stays queued.
    public func remove() async {
        await serialized { await self.performRemove() }
    }

    private func performRemove() async {
        isBusy = true; defer { isBusy = false }
        guard let pendingRemovals else {
            try? await client.deactivate(); license = nil; message = messages.removed
            return
        }
        bridgeConsumption?.suppress()
        let current = await client.load()
        if let current, let owed = PendingLicenseRemoval.owed(by: current) { await pendingRemovals.enqueue(owed) }
        try? await client.store.remove()
        license = nil
        message = messages.removed
        await deliverPendingRemovals()
    }

    private func routedRefresh() async throws -> (StoredLicense?, deviceRemoved: Bool) {
        if await client.load()?.leaseId != nil { return try await client.renewLeaseOutcome() }
        return (try await client.refresh(), false)
    }

    private func publish(_ refreshed: StoredLicense?, silent: Bool) {
        if silent, refreshed == license { return }
        license = refreshed
    }

    private func verifiesPro(_ record: StoredLicense?) -> Bool {
        record?.authorizedClaims(configuration: client.configuration)?.isPro == true
    }

    /// `pro` (when set and the record verifies as Pro), else the server message, else `fallback` —
    /// with `pro == nil` this is exactly the original `record?.message ?? fallback`.
    private func outcomeMessage(for record: StoredLicense?, pro: String?, fallback: String) -> String {
        if let pro, verifiesPro(record) { return pro }
        return record?.message ?? fallback
    }
}
#endif

/// CodeRight's display-only token adapter. It stores only opaque signed token bytes and a stable
/// device id; callers still verify through `LicenseVerifier` before exposing claims.
public struct CodeRightLicenseTokenStore: Sendable {
    private let store: KeychainLicenseStore
    public init(service: String = "app.coderight.license") { store = KeychainLicenseStore(service: service, account: "license-token") }
    public func loadToken() async -> String? { (await store.load())?.signedToken }
    public func saveToken(_ token: String) async throws { try await store.save(StoredLicense(status: "active", deviceId: deviceID(), signedToken: token.trimmingCharacters(in: .whitespacesAndNewlines))) }
    public func clear() async throws { try await store.remove() }
    public func deviceID() -> String { store.deviceID() }
}
