import Foundation

/// Non-secret token-plan facts persisted with an account. `refreshToken` is
/// intentionally absent: `ProviderAccountStore` keeps it in Keychain.
public struct TokenPlanConfiguration: Codable, Equatable, Sendable {
    public var planID: String
    public var remainingQuota: UInt64
    public var refreshEndpoint: String

    public init(planID: String, remainingQuota: UInt64, refreshEndpoint: String) {
        self.planID = planID
        self.remainingQuota = remainingQuota
        self.refreshEndpoint = refreshEndpoint
    }

    public var isValid: Bool {
        !planID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && refreshEndpoint.hasPrefix("https://")
    }
}

/// Swift wire form of `coderight_providers::TokenPlanCredential`.
/// It exists only in memory between Keychain, Rust refresh bridge, & active
/// runtime; do not encode it into account or session persistence.
public struct MobileTokenPlanCredential: Codable, Equatable, Sendable {
    public var planID: String
    public var remainingQuota: UInt64
    public var refreshEndpoint: String
    public var refreshToken: String

    enum CodingKeys: String, CodingKey {
        case planID = "plan_id"
        case remainingQuota = "remaining_quota"
        case refreshEndpoint = "refresh_endpoint"
        case refreshToken = "refresh_token"
    }

    public init(
        planID: String,
        remainingQuota: UInt64,
        refreshEndpoint: String,
        refreshToken: String
    ) {
        self.planID = planID
        self.remainingQuota = remainingQuota
        self.refreshEndpoint = refreshEndpoint
        self.refreshToken = refreshToken
    }
}

/// Session-only provider auth. This value is never stored in UserDefaults or
/// a session checkpoint; token-plan route supplies its refresh credential as
/// current provider bearer material while Rust owns quota reservation.
public enum MobileProviderSessionAuthentication: Sendable {
    case apiKey(String)
    case tokenPlan(MobileTokenPlanCredential)

    /// Non-empty bootstrap value required while Rust constructs its provider
    /// route. For token plans, Rust replaces this with current plan state
    /// before every request; callers must never use it for catalog/image
    /// requests or persist it outside Keychain.
    public var runtimeBootstrapCredential: String {
        switch self {
        case let .apiKey(apiKey): return apiKey
        case let .tokenPlan(plan): return plan.refreshToken
        }
    }

    public var tokenPlan: MobileTokenPlanCredential? {
        if case let .tokenPlan(plan) = self { return plan }
        return nil
    }
}

/// Rust-backed, bounded refresh seam. Production conformance must call
/// `TokenPlanRefreshClient::refresh` through mobile runtime's C ABI; this
/// protocol keeps account/UI code independent from that ABI.
public protocol TokenPlanRuntimeRefreshing: Sendable {
    func refreshTokenPlan(
        _ credential: MobileTokenPlanCredential
    ) async throws -> MobileTokenPlanCredential
}

/// Active runtime seam for receiving a rotated credential before Keychain
/// persistence. Updating runtime first prevents a resumed turn from using a
/// stale refresh token if persistence is delayed.
public protocol TokenPlanRuntimeCredentialUpdating: Sendable {
    func updateTokenPlanCredential(_ credential: MobileTokenPlanCredential) async throws
}

/// iOS token-plan coordinator. Session launcher supplies account, lock gate,
/// Rust refresher, & live runtime; this keeps refresh state out of defaults
/// and commits a rotation in one explicit order.
@MainActor
public struct TokenPlanMobileAuthBridge {
    private let accounts: ProviderAccountStore

    public init(accounts: ProviderAccountStore) {
        self.accounts = accounts
    }

    @discardableResult
    public func refreshActivePlan(
        account: ProviderAccount,
        gate: AppLockGate,
        refresher: any TokenPlanRuntimeRefreshing,
        runtime: any TokenPlanRuntimeCredentialUpdating
    ) async throws -> MobileTokenPlanCredential {
        let credential = try await accounts.tokenPlanCredential(for: account, gate: gate)
        let refreshed = try await refresher.refreshTokenPlan(credential)
        try await runtime.updateTokenPlanCredential(refreshed)
        try accounts.updateTokenPlanCredential(refreshed, for: account)
        return refreshed
    }
}
