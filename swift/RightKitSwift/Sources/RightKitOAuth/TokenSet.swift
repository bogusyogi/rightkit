import Foundation
import RightKitCredentials

/// Tokens from an OAuth provider. `description` and `debugDescription` redact every secret.
public struct TokenSet: Sendable, Equatable, CustomStringConvertible, CustomDebugStringConvertible {
    public var accessToken: String
    public var tokenType: String
    public var expiresIn: TimeInterval?
    public var refreshToken: String?
    public var scope: String?
    /// OpenID Connect ID token (a JWT). Unverified here.
    public var idToken: String?
    public var obtainedAt: Date

    public init(
        accessToken: String, tokenType: String = "Bearer", expiresIn: TimeInterval? = nil,
        refreshToken: String? = nil, scope: String? = nil, idToken: String? = nil, obtainedAt: Date = Date(),
    ) {
        self.accessToken = accessToken
        self.tokenType = tokenType
        self.expiresIn = expiresIn
        self.refreshToken = refreshToken
        self.scope = scope
        self.idToken = idToken
        self.obtainedAt = obtainedAt
    }

    /// True when the access token expires within `skew` of `now`. A token with no `expires_in`
    /// never reports expired here; the provider's 401 is the signal.
    public func isExpired(skew: TimeInterval = 60, now: Date = Date()) -> Bool {
        guard let expiresIn else { return false }
        return obtainedAt.addingTimeInterval(expiresIn - skew) <= now
    }

    public var description: String {
        "TokenSet(access: [redacted], refresh: \(refreshToken == nil ? "none" : "[redacted]"), expiresIn: \(expiresIn.map { String($0) } ?? "none"))"
    }
    public var debugDescription: String { description }

    // MARK: Stored record (shared with rightkit-oauth's `token_codec`)

    private struct Record: Codable {
        var v: Int
        var access: String
        var type: String?
        var expiresIn: UInt64?
        var renew: String?
        var scope: String?
        var id: String?
        var obtainedAt: UInt64?

        enum CodingKeys: String, CodingKey {
            case v, access, type, renew, scope, id
            case expiresIn = "expires_in"
            case obtainedAt = "obtained_at"
        }
    }

    /// The JSON record other RightKit platforms read. Never log it.
    public func encoded() throws -> Data {
        let record = Record(
            v: 1, access: accessToken, type: tokenType, expiresIn: expiresIn.map { UInt64(max(0, $0)) },
            renew: refreshToken, scope: scope, id: idToken, obtainedAt: UInt64(max(0, obtainedAt.timeIntervalSince1970)),
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(record)
    }

    public init(decoding data: Data) throws {
        guard let record = try? JSONDecoder().decode(Record.self, from: data), record.v == 1, !record.access.isEmpty else {
            throw OAuthError.storage("stored sign-in is unreadable")
        }
        self.init(
            accessToken: record.access, tokenType: record.type ?? "Bearer", expiresIn: record.expiresIn.map { TimeInterval($0) },
            refreshToken: (record.renew?.isEmpty ?? true) ? nil : record.renew, scope: record.scope,
            idToken: (record.id?.isEmpty ?? true) ? nil : record.id,
            obtainedAt: Date(timeIntervalSince1970: TimeInterval(record.obtainedAt ?? 0)),
        )
    }
}

/// Persistence seam for one sign-in.
public protocol OAuthTokenStore: Sendable {
    func load() throws -> TokenSet?
    func save(_ tokens: TokenSet) throws
    func clear() throws
}

/// Keeps the token record in a RightKitCredentials ``SecretsStore`` (Keychain): namespace = the
/// store's service, account = `key`.
public struct SecretsTokenStore: OAuthTokenStore {
    public let store: SecretsStore
    public let key: String

    public init(store: SecretsStore, key: String) {
        self.store = store
        self.key = key
    }

    public func load() throws -> TokenSet? {
        do {
            guard let data = try store.get(key) else { return nil }
            return try TokenSet(decoding: data)
        } catch let error as OAuthError { throw error } catch { throw OAuthError.storage("sign-in could not be read") }
    }

    public func save(_ tokens: TokenSet) throws {
        do { try store.set(key, try tokens.encoded()) } catch let error as OAuthError { throw error } catch {
            throw OAuthError.storage("sign-in could not be saved")
        }
    }

    public func clear() throws {
        do { try store.delete(key) } catch { throw OAuthError.storage("sign-in could not be cleared") }
    }
}

/// In-memory store for tests and one-shot tools.
public final class MemoryTokenStore: OAuthTokenStore, @unchecked Sendable {
    private let lock = NSLock()
    private var tokens: TokenSet?
    public init(_ tokens: TokenSet? = nil) { self.tokens = tokens }
    public func load() throws -> TokenSet? { lock.withLock { tokens } }
    public func save(_ tokens: TokenSet) throws { lock.withLock { self.tokens = tokens } }
    public func clear() throws { lock.withLock { tokens = nil } }
}
