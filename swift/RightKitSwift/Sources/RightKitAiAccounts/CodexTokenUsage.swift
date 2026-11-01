import Foundation

/// Account-wide Codex activity once returned by the Codex profile endpoint.
///
/// The endpoint is no longer fetched; the type stays so archived readings that
/// carry it still decode (`UsageArchive`), and `ProviderSnapshot.tokenUsage`
/// stays nil for new ones.
struct CodexTokenUsage: Codable, Equatable, Sendable {
    struct Summary: Codable, Equatable, Sendable {
        let lifetimeTokens: Int?
        let peakDailyTokens: Int?
        let longestRunningTurnSeconds: Double?
        let currentStreakDays: Int?
        let longestStreakDays: Int?
    }

    struct DailyBucket: Codable, Equatable, Identifiable, Sendable {
        let startDate: String
        let tokens: Int

        var id: String { startDate }
    }

    let summary: Summary?
    let dailyUsageBuckets: [DailyBucket]
}
