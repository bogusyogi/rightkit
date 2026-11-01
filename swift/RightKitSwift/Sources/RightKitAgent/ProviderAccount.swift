import Foundation
import Observation

/// Distinct OpenCode subscription & usage-billed API routes.
public enum OpenCodeService: String, CaseIterable, Identifiable {
    case go, zen
    public var id: String { rawValue }
    public var label: String { self == .go ? "OpenCode Go" : "OpenCode Zen" }
    public var baseURL: String { self == .go ? "https://opencode.ai/zen/go/v1" : "https://opencode.ai/zen/v1" }
}

/// A model provider the user has configured for on-device sessions.
///
/// The record here is metadata only — provider, endpoint, model. **The API key
/// is never in this type and never in `UserDefaults`;** it lives in
/// `SecretStore` behind a Face ID gate and is fetched only at the moment a
/// session starts. Keeping the two apart is what lets the settings screen list
/// accounts without prompting for biometrics.
public struct ProviderAccount: Codable, Identifiable, Equatable, Sendable {
    public static let onDeviceAccountID = "coderight.on-device"
    public static let onDeviceModelID = "apple.foundation-models"

    /// Wire protocol the endpoint speaks. Matches `ProviderConfig.protocol` in
    /// `engine/crates/mobile_runtime/src/lib.rs`.
    public enum WireProtocol: String, Codable, CaseIterable, Sendable {
        case anthropic
        case openai

        public var displayName: String {
            switch self {
            case .anthropic: return "Anthropic"
            case .openai: return "OpenAI-compatible"
            }
        }

        public var defaultBaseURL: String {
            switch self {
            case .anthropic: return "https://api.anthropic.com"
            case .openai: return "https://api.openai.com/v1"
            }
        }

        public var defaultModel: String {
            switch self {
            case .anthropic: return "claude-sonnet-4-5"
            case .openai: return "gpt-5.1"
            }
        }
    }

    /// How this account authorizes a model request. Token-plan state is kept
    /// deliberately separate from an API key or OAuth credential: it has a
    /// bounded quota plus a provider refresh route, neither of which may be
    /// mistaken for a bearer key by session setup.
    public enum AuthMode: String, Codable, CaseIterable, Sendable {
        case apiKey
        case oauth
        case tokenPlan

        public var displayName: String {
            switch self {
            case .apiKey: return "API key"
            case .oauth: return "OAuth"
            case .tokenPlan: return "Token plan"
            }
        }
    }

    public var id: String
    public var label: String
    public var wireProtocol: WireProtocol
    public var baseURL: String
    public var model: String
    /// Defaults to API-key auth when decoding existing accounts.
    public var authMode: AuthMode
    /// Non-secret plan metadata. Its refresh credential is stored only in the
    /// Keychain under `tokenPlanSecretID`, never in this Codable record.
    public var tokenPlan: TokenPlanConfiguration?

    public var openCodeService: OpenCodeService? {
        guard let url = URL(string: baseURL), url.host == "opencode.ai" else { return nil }
        switch url.path.trimmingCharacters(in: CharacterSet(charactersIn: "/")) {
        case "zen/go/v1": return .go
        case "zen/v1": return .zen
        default: return nil
        }
    }

    public init(
        id: String = UUID().uuidString,
        label: String,
        wireProtocol: WireProtocol,
        baseURL: String,
        model: String,
        authMode: AuthMode = .apiKey,
        tokenPlan: TokenPlanConfiguration? = nil
    ) {
        self.id = id
        self.label = label
        self.wireProtocol = wireProtocol
        self.baseURL = baseURL
        self.model = model
        self.authMode = authMode
        self.tokenPlan = tokenPlan
    }

    enum CodingKeys: String, CodingKey {
        case id, label, wireProtocol, baseURL, model, authMode, tokenPlan
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        id = try values.decode(String.self, forKey: .id)
        label = try values.decode(String.self, forKey: .label)
        wireProtocol = try values.decode(WireProtocol.self, forKey: .wireProtocol)
        baseURL = try values.decode(String.self, forKey: .baseURL)
        model = try values.decode(String.self, forKey: .model)
        authMode = try values.decodeIfPresent(AuthMode.self, forKey: .authMode) ?? .apiKey
        tokenPlan = try values.decodeIfPresent(TokenPlanConfiguration.self, forKey: .tokenPlan)
    }

    public func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(id, forKey: .id)
        try values.encode(label, forKey: .label)
        try values.encode(wireProtocol, forKey: .wireProtocol)
        try values.encode(baseURL, forKey: .baseURL)
        try values.encode(model, forKey: .model)
        try values.encode(authMode, forKey: .authMode)
        try values.encodeIfPresent(tokenPlan, forKey: .tokenPlan)
    }
}

/// Persists provider metadata and brokers the biometric key fetch.
@MainActor
@Observable
public final class ProviderAccountStore {
    private static let defaultsKey = "app.coderight.providerAccounts"

    public private(set) var accounts: [ProviderAccount] = []
    /// Last error surfaced to the UI, e.g. a cancelled Face ID prompt.
    public var lastError: String?

    private let defaults: UserDefaults
    private let secrets: SecretStore

    public enum AuthenticationError: Error, LocalizedError, Equatable {
        case wrongAuthMode(ProviderAccount.AuthMode)
        case missingTokenPlanConfiguration
        case tokenPlanRequiresRuntimeBridge
        case oauthRequiresRuntimeBridge

        public var errorDescription: String? {
            switch self {
            case let .wrongAuthMode(mode):
                return "This account uses \(mode.displayName), not an API key."
            case .missingTokenPlanConfiguration:
                return "This token-plan account is missing its plan configuration."
            case .tokenPlanRequiresRuntimeBridge:
                return "Token-plan authentication must be prepared by the local runtime."
            case .oauthRequiresRuntimeBridge:
                return "OAuth authentication must be prepared by the local runtime."
            }
        }
    }

    public init(defaults: UserDefaults = .standard, secrets: SecretStore = SecretStore()) {
        self.defaults = defaults
        self.secrets = secrets
        load()
    }

    private func load() {
        guard let data = defaults.data(forKey: Self.defaultsKey),
            let decoded = try? JSONDecoder().decode([ProviderAccount].self, from: data)
        else { return }
        accounts = decoded
    }

    private func persist() {
        guard let data = try? JSONEncoder().encode(accounts) else { return }
        defaults.set(data, forKey: Self.defaultsKey)
    }

    /// Change billing route without reading or replacing the stored API key.
    public func setOpenCodeService(_ service: OpenCodeService, for account: ProviderAccount) {
        guard account.openCodeService != nil,
              let index = accounts.firstIndex(where: { $0.id == account.id }) else { return }
        accounts[index].baseURL = service.baseURL
        accounts[index].label = service.label
        persist()
    }

    /// Save an account and its key. The key goes to Keychain storage gated
    /// by `AppLockGate`; only the metadata is written to defaults.
    ///
    /// If the key cannot be stored, the account is **not** recorded either —
    /// an account whose key is missing would fail confusingly at session start.
    public func save(_ account: ProviderAccount, apiKey: String) throws {
        guard account.authMode == .apiKey else {
            throw AuthenticationError.wrongAuthMode(account.authMode)
        }
        try secrets.store(apiKey, kind: .providerAPIKey, id: account.id)
        if let index = accounts.firstIndex(where: { $0.id == account.id }) {
            accounts[index] = account
        } else {
            accounts.append(account)
        }
        persist()
    }

    public func remove(_ account: ProviderAccount) {
        try? secrets.remove(kind: .providerAPIKey, id: account.id)
        try? secrets.remove(kind: .providerAPIKey, id: tokenPlanSecretID(account.id))
        accounts.removeAll { $0.id == account.id }
        persist()
    }

    /// Store only token-plan refresh secret in Keychain. Plan ID, endpoint,
    /// & last known quota remain non-secret account metadata for presentation.
    public func saveTokenPlan(_ account: ProviderAccount, refreshToken: String) throws {
        guard account.authMode == .tokenPlan else {
            throw AuthenticationError.wrongAuthMode(account.authMode)
        }
        guard account.tokenPlan != nil else {
            throw AuthenticationError.missingTokenPlanConfiguration
        }
        try secrets.store(
            refreshToken,
            kind: .providerAPIKey,
            id: tokenPlanSecretID(account.id)
        )
        if let index = accounts.firstIndex(where: { $0.id == account.id }) {
            accounts[index] = account
        } else {
            accounts.append(account)
        }
        persist()
    }

    /// Fetch the key for a session start. Prompts via `gate` only when its
    /// grace window has lapsed — see `AppLockGate`.
    public func apiKey(for account: ProviderAccount, gate: AppLockGate) async throws -> String {
        switch account.authMode {
        case .apiKey:
            break
        case .oauth:
            throw AuthenticationError.oauthRequiresRuntimeBridge
        case .tokenPlan:
            throw AuthenticationError.tokenPlanRequiresRuntimeBridge
        }
        try await gate.unlockIfNeeded(
            reason: "Use your \(account.label) API key to run this session on this device."
        )
        guard let secret = try secrets.read(kind: .providerAPIKey, id: account.id) else {
            throw SecretStore.Failure.keychain(errSecItemNotFound)
        }
        return secret
    }

    /// Resolve one iOS session's authorization without ever treating a
    /// token-plan refresh secret as a persisted API key.
    public func sessionAuthentication(
        for account: ProviderAccount,
        gate: AppLockGate
    ) async throws -> MobileProviderSessionAuthentication {
        switch account.authMode {
        case .apiKey:
            return .apiKey(try await apiKey(for: account, gate: gate))
        case .tokenPlan:
            return .tokenPlan(try await tokenPlanCredential(for: account, gate: gate))
        case .oauth:
            throw AuthenticationError.oauthRequiresRuntimeBridge
        }
    }

    /// Build token-plan input for mobile runtime. It unlocks only long enough
    /// to read Keychain refresh secret; no secret enters UserDefaults.
    public func tokenPlanCredential(
        for account: ProviderAccount,
        gate: AppLockGate
    ) async throws -> MobileTokenPlanCredential {
        guard account.authMode == .tokenPlan else {
            throw AuthenticationError.wrongAuthMode(account.authMode)
        }
        guard let plan = account.tokenPlan else {
            throw AuthenticationError.missingTokenPlanConfiguration
        }
        try await gate.unlockIfNeeded(
            reason: "Use your \(account.label) token plan to run this session on this device."
        )
        guard let refreshToken = try secrets.read(
            kind: .providerAPIKey,
            id: tokenPlanSecretID(account.id)
        ) else {
            throw SecretStore.Failure.keychain(errSecItemNotFound)
        }
        return MobileTokenPlanCredential(
            planID: plan.planID,
            remainingQuota: plan.remainingQuota,
            refreshEndpoint: plan.refreshEndpoint,
            refreshToken: refreshToken
        )
    }

    /// Persist a successful refresh, including provider credential rotation.
    /// This is called only after mobile runtime accepts refreshed config.
    public func updateTokenPlanCredential(
        _ credential: MobileTokenPlanCredential,
        for account: ProviderAccount
    ) throws {
        guard let index = accounts.firstIndex(where: { $0.id == account.id }),
              var plan = accounts[index].tokenPlan
        else {
            throw AuthenticationError.missingTokenPlanConfiguration
        }
        plan.remainingQuota = credential.remainingQuota
        plan.refreshEndpoint = credential.refreshEndpoint
        accounts[index].tokenPlan = plan
        try secrets.store(
            credential.refreshToken,
            kind: .providerAPIKey,
            id: tokenPlanSecretID(account.id)
        )
        persist()
    }

    private func tokenPlanSecretID(_ accountID: String) -> String {
        "token-plan:\(accountID)"
    }
}
