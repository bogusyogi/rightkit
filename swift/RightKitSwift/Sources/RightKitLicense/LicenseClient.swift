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

    public init(
        ok: Bool? = nil, status: String? = nil, message: String? = nil, activationToken: String? = nil,
        licenseId: String? = nil, validUntil: String? = nil, signedToken: String? = nil,
        licenseKind: String? = nil, entitlementStage: String? = nil, licenseIssuedAt: String? = nil,
        confirmationDueAt: String? = nil, entitlements: LicenseEntitlements? = nil,
        lease: LicenseLeaseEnvelope? = nil, error: String? = nil,
    ) {
        self.ok = ok; self.status = status; self.message = message; self.activationToken = activationToken
        self.licenseId = licenseId; self.validUntil = validUntil; self.signedToken = signedToken
        self.licenseKind = licenseKind; self.entitlementStage = entitlementStage
        self.licenseIssuedAt = licenseIssuedAt; self.confirmationDueAt = confirmationDueAt
        self.entitlements = entitlements; self.lease = lease; self.error = error
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

    public func verifiedClaims(
        publicKeyBase64url: String, app: String, now: Int = Int(Date().timeIntervalSince1970)
    ) -> LicenseClaims? {
        guard let signedToken else { return nil }
        let verificationDevice = leaseId == nil ? deviceId : LicenseActivationClient.deviceIDHash(deviceId)
        guard let claims = try? LicenseVerifier(publicKeyBase64url: publicKeyBase64url).verify(
            token: signedToken, app: app, deviceId: verificationDevice, now: now
        ) else { return nil }
        guard entitlementScope == nil || entitlementScope == claims.scope else { return nil }
        return claims
    }

    public func isProCandidate(
        publicKeyBase64url: String, app: String, now: Int = Int(Date().timeIntervalSince1970)
    ) -> Bool {
        verifiedClaims(publicKeyBase64url: publicKeyBase64url, app: app, now: now)?.isPro == true
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
    public let entitlement: String
    public let signature: String?
    public let entitlementScope: String?

    public init(
        leaseId: String, deviceIdHash: String, pool: String, issuedAt: String, expiresAt: String,
        graceSeconds: Int64, generation: Int, entitlement: String, signature: String? = nil,
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

    public func verifySignature(publicKey: Curve25519.Signing.PublicKey) throws {
        guard let signature, let bytes = Data(base64urlEncoded: signature) else {
            throw LicenseError.malformedLeaseSignature
        }
        guard publicKey.isValidSignature(bytes, for: Data(canonicalJSON().utf8)) else {
            throw LicenseError.badLeaseSignature
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

    public func load() async -> StoredLicense? {
        guard let data = read(service: service, account: account) else { return nil }
        try? write(data, service: service, account: account) // migrate legacy accessibility
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try? decoder.decode(StoredLicense.self, from: data)
    }

    public func save(_ license: StoredLicense) async throws {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        guard let data = try? encoder.encode(license) else { throw LicenseStorageError.encoding }
        try write(data, service: service, account: account)
    }

    public func remove() async throws { try delete(service: service, account: account) }

    public func deviceID(account: String = "device-id") -> String {
        if let data = read(service: service, account: account), let value = String(data: data, encoding: .utf8), !value.isEmpty {
            // Migrate a record written before device-only accessibility was enforced.
            try? write(data, service: service, account: account)
            return value
        }
        let value = UUID().uuidString
        try? write(Data(value.utf8), service: service, account: account)
        return value
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

public struct LicenseClientConfiguration: Sendable {
    public let app: String
    public let baseURL: URL
    public let appVersion: String
    public let deviceID: @Sendable () -> String
    public let session: URLSession
    public let publicKeyBase64url: String?

    public init(app: String, baseURL: URL, appVersion: String = "0.0.0", deviceID: @escaping @Sendable () -> String, session: URLSession = .shared, publicKeyBase64url: String? = nil) {
        self.app = app; self.baseURL = baseURL; self.appVersion = appVersion; self.deviceID = deviceID; self.session = session; self.publicKeyBase64url = publicKeyBase64url
    }
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
        let body: LicenseServerResponse = try await post(path: "activate", payload: ["app": configuration.app, "licenseKey": key, "deviceId": configuration.deviceID(), "version": configuration.appVersion])
        let license = Self.license(from: body, deviceId: configuration.deviceID(), key: key)
        try await store.save(license)
        return license
    }

    public func refresh() async throws -> StoredLicense? {
        guard let current = await store.load(), let token = current.activationToken, !token.isEmpty else { return await store.load() }
        let body: LicenseServerResponse
        do {
            body = try await post(path: "refresh", payload: ["app": configuration.app, "activationToken": token, "deviceId": current.deviceId])
        } catch let error as LicenseError {
            if case .serverStatus(let status, _) = error, !(400..<500).contains(status) { return current }
            throw error
        } catch { return current }
        let updated = Self.merged(current: current, refreshed: Self.license(from: body, deviceId: current.deviceId, key: current.key, activatedAt: current.activatedAt))
        try await store.save(updated)
        return updated
    }

    public func deactivate() async throws {
        if let current = await store.load(), let token = current.activationToken {
            _ = try? await post(path: "deactivate", payload: ["app": configuration.app, "activationToken": token]) as LicenseServerResponse
        }
        try await store.remove()
    }

    public func activateLease(key: String, deviceLabel: String? = nil) async throws -> StoredLicense {
        let deviceHash = Self.deviceIDHash(configuration.deviceID())
        var payload = ["appKey": configuration.app, "licenseKey": key.trimmingCharacters(in: .whitespacesAndNewlines), "deviceIdHash": deviceHash, "platform": Self.platform()]
        if let deviceLabel { payload["deviceLabel"] = deviceLabel }
        let body: LicenseServerResponse = try await post(path: "../pro/\(configuration.app)/devices/lease", payload: payload)
        guard let lease = body.lease, let token = body.activationToken else { throw LicenseError.invalidResponse }
        try lease.verifySignature(publicKey: try leaseKey())
        let license = Self.license(from: lease, activationToken: token, deviceId: configuration.deviceID())
        try await store.save(license)
        return license
    }

    public func renewLease() async throws -> StoredLicense? {
        guard let current = await store.load(), let leaseId = current.leaseId else { return await store.load() }
        let payload = ["appKey": configuration.app, "deviceIdHash": Self.deviceIDHash(current.deviceId), "leaseId": leaseId]
        do {
            let body: LicenseServerResponse = try await post(path: "../pro/\(configuration.app)/devices/lease/renew", payload: payload)
            guard let lease = body.lease else { return current }
            try lease.verifySignature(publicKey: try leaseKey())
            let renewed = Self.license(from: lease, activationToken: current.activationToken ?? "", deviceId: current.deviceId, activatedAt: current.activatedAt)
            try await store.save(renewed); return renewed
        } catch let error as LicenseError {
            if case .serverStatus(let status, _) = error, status == 404 || status == 410 {
                var revoked = current; revoked.status = "revoked"; revoked.signedToken = nil; revoked.entitlements = nil; revoked.validUntil = nil
                try await store.save(revoked); return revoked
            }
            return current
        } catch { return current }
    }

    public func removeLease() async throws {
        guard let current = await store.load(), let leaseId = current.leaseId else {
            try await deactivate()
            return
        }
        let payload = ["appKey": configuration.app, "deviceIdHash": Self.deviceIDHash(current.deviceId), "leaseId": leaseId]
        _ = try await post(path: "../pro/\(configuration.app)/devices/lease/remove", payload: payload) as LicenseServerResponse
        try await store.remove()
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
        let url = configuration.baseURL.appendingPathComponent(path)
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
        return (object["message"] as? String) ?? (object["error"] as? String) ?? ""
    }

    private static func merged(current: StoredLicense, refreshed: StoredLicense) -> StoredLicense {
        let revoked = ["removed", "revoked", "invalid"].contains(refreshed.status.lowercased())
        var value = current
        value.status = refreshed.status; value.message = refreshed.message; value.lastRefreshedAt = refreshed.lastRefreshedAt
        if !revoked { value.validUntil = refreshed.validUntil ?? current.validUntil; value.entitlements = refreshed.entitlements ?? current.entitlements; value.signedToken = refreshed.signedToken ?? current.signedToken }
        else { value.validUntil = nil; value.entitlements = nil; value.signedToken = nil }
        value.activationToken = refreshed.activationToken ?? current.activationToken
        value.licenseId = refreshed.licenseId ?? current.licenseId
        value.licenseKind = refreshed.licenseKind ?? current.licenseKind; value.entitlementStage = refreshed.entitlementStage ?? current.entitlementStage
        value.licenseIssuedAt = refreshed.licenseIssuedAt ?? current.licenseIssuedAt; value.confirmationDueAt = refreshed.confirmationDueAt ?? current.confirmationDueAt
        return value
    }

    private static func license(from lease: LicenseLeaseEnvelope, activationToken: String, deviceId: String, activatedAt: String? = nil) -> StoredLicense {
        StoredLicense(status: "active", deviceId: deviceId, activationToken: activationToken, validUntil: lease.expiresAt, activatedAt: activatedAt ?? ISO8601DateFormatter().string(from: Date()), lastRefreshedAt: ISO8601DateFormatter().string(from: Date()), signedToken: lease.entitlement, leaseId: lease.leaseId, leaseGeneration: lease.generation, leaseExpiresAtUnix: ISO8601DateFormatter().date(from: lease.expiresAt).map { Int64($0.timeIntervalSince1970) }, leaseGraceSeconds: lease.graceSeconds, entitlementScope: lease.entitlementScope)
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
    /// and is bound to `app`, to this record's device (and to `deviceId` when the caller supplies
    /// the live device identity), and to an unexpired `exp`. The record must also still be
    /// `active`; a missing, unsigned, forged, wrong-app, wrong-device or expired token yields nil.
    func authorizedClaims(
        publicKeyBase64url: String, app: String, deviceId expectedDeviceId: String? = nil,
        now: Int = Int(Date().timeIntervalSince1970)
    ) -> LicenseClaims? {
        guard status == "active" else { return nil }
        if let expectedDeviceId, expectedDeviceId != deviceId { return nil }
        return verifiedClaims(publicKeyBase64url: publicKeyBase64url, app: app, now: now)
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

public func isPro(_ license: StoredLicense?, publicKeyBase64url: String, app: String, deviceId: String? = nil, now: Int = Int(Date().timeIntervalSince1970)) -> Bool {
    license?.authorizedClaims(publicKeyBase64url: publicKeyBase64url, app: app, deviceId: deviceId, now: now)?.isPro == true
}

public func satisfiesRequirement(_ license: StoredLicense?, _ requirement: LicenseRequirement, publicKeyBase64url: String, app: String, deviceId: String? = nil, now: Int = Int(Date().timeIntervalSince1970)) -> Bool {
    license?.authorizedClaims(publicKeyBase64url: publicKeyBase64url, app: app, deviceId: deviceId, now: now)?.satisfies(requirement) == true
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
        return license?.verifiedClaims(publicKeyBase64url: publicKeyBase64url, app: app, now: now())
    }
}

#if canImport(Combine)
@MainActor
public final class LicenseController: ObservableObject {
    @Published public private(set) var license: StoredLicense?
    @Published public private(set) var isBusy = false
    @Published public private(set) var message = ""

    public let client: LicenseActivationClient

    public init(client: LicenseActivationClient) {
        self.client = client
        Task { [weak self] in self?.license = await client.load() }
    }

    public var isProCandidate: Bool {
        guard let key = client.configuration.publicKeyBase64url else { return false }
        return license?.isProCandidate(publicKeyBase64url: key, app: client.configuration.app) == true
    }

    public func activate(_ key: String) async {
        isBusy = true; defer { isBusy = false }
        do { license = try await client.activate(key: key); message = license?.message ?? "License activated." }
        catch { message = error.localizedDescription }
    }

    public func refresh() async {
        isBusy = true; defer { isBusy = false }
        do { license = try await client.refresh(); message = license?.message ?? "License refreshed." }
        catch { message = error.localizedDescription }
    }

    public func remove() async {
        isBusy = true; defer { isBusy = false }
        try? await client.deactivate(); license = nil; message = "License removed."
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
