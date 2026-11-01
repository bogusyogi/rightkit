import Foundation
import RightKitCredentials
import Security

/// Dedicated Keychain store for MCP/connector server credentials (product
/// atom 38). Mirrors `SecretStore`'s device-only, no-per-item-biometric-ACL
/// pattern (see that file's doc comment for the `.userPresence` pitfall this
/// avoids), but stays a separate type and a separate Keychain `service` so
/// an MCP server credential can never collide with, or be enumerated
/// alongside, a provider API key or forge token.
///
/// # Where the secret value goes (and where it never goes)
///
/// A credential stored here is written straight to the Keychain by the app
/// and is read back only to be handed to the daemon's connection layer out
/// of band from the model-visible tool-call stream. It must never be placed
/// into a `spawn_subagent`/`mcp_call`-style tool-args JSON payload, because
/// that payload is exactly what `ToolActivity`/`CodeRightToolInteractionsPort`
/// render into the transcript — the daemon's own `mcp_auth` tool documents
/// the same boundary by writing credentials to an owner-only file on disk
/// rather than accepting them as an argument a model could echo back. UI
/// built on top of this store (see `LocalMcpConnectorsView`) must only ever
/// display whether a credential is saved, never its value.
public struct MCPSecretStore: Sendable {
    public enum Failure: Error, LocalizedError, Equatable {
        case keychain(OSStatus)

        public var errorDescription: String? {
            switch self {
            case let .keychain(status):
                return "Keychain error \(status)."
            }
        }
    }

    /// The credential shape an MCP server was configured with. Kept explicit
    /// (rather than a bare string) so a connectors UI can render the right
    /// input affordance without guessing.
    public enum CredentialKind: String, Sendable, Codable {
        case bearer = "bearer"
        case apiKey = "api_key"
    }

    /// One Keychain item per server: the kind and the value travel together, so a kind can never
    /// outlive or precede its credential. Keychain access is RightKitCredentials' `SecretsStore`.
    private struct Record: Codable {
        let kind: CredentialKind
        let value: String
    }

    private let keychain: SecretsStore?

    public init(service: String = "app.coderight.CodeRight.mcp-secrets") {
        keychain = try? SecretsStore(namespace: service, accessibility: .whenUnlockedThisDeviceOnly)
    }

    private func backing() throws -> SecretsStore {
        guard let keychain else { throw Failure.keychain(errSecParam) }
        return keychain
    }

    private static func map(_ error: Error) -> Error {
        guard let error = error as? SecretsError else { return error }
        switch error {
        case let .keychain(status): return Failure.keychain(status)
        case .invalid: return Failure.keychain(errSecParam)
        case .guarded: return Failure.keychain(errSecNotAvailable)
        }
    }

    private func record(serverId: String) throws -> Record? {
        let data: Data?
        do {
            data = try backing().get(account(serverId: serverId))
        } catch {
            throw Self.map(error)
        }
        guard let data else { return nil }
        guard let record = try? JSONDecoder().decode(Record.self, from: data) else {
            throw Failure.keychain(errSecDecode)
        }
        return record
    }

    // MARK: - Write

    /// Stores a server's credential. Device-only, off backups: same
    /// accessibility class as `SecretStore`, so an MCP credential is exactly
    /// as protected as a provider API key, no more and no less.
    public func store(_ credential: String, kind: CredentialKind, serverId: String) throws {
        do {
            let data = try JSONEncoder().encode(Record(kind: kind, value: credential))
            try backing().set(account(serverId: serverId), data)
        } catch {
            throw Self.map(error)
        }
    }

    // MARK: - Read

    /// Reads a server's credential value. Callers that render UI must never
    /// surface this return value directly, only whether it is non-nil.
    public func read(serverId: String) throws -> String? {
        try record(serverId: serverId)?.value
    }

    /// Reads the credential kind saved for a server.
    public func credentialKind(serverId: String) -> CredentialKind? {
        (try? record(serverId: serverId))?.kind
    }

    /// Whether a credential is currently saved for this server. The
    /// connectors UI uses this, never `read(serverId:)`, to render a
    /// "credential saved" indicator without ever touching the secret value.
    public func hasCredential(serverId: String) -> Bool {
        (try? backing().exists(account(serverId: serverId))) ?? false
    }

    // MARK: - Delete

    public func remove(serverId: String) throws {
        do {
            try backing().delete(account(serverId: serverId))
        } catch {
            throw Self.map(error)
        }
    }

    private func account(serverId: String) -> String {
        "mcp-server:\(serverId)"
    }
}
