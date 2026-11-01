import Foundation

/// Automatic fixture fallback for a failed real connection attempt (APP-09).
///
/// Not wired into any pairing code by this lane — `DaemonClientFactory`/`PairingView` are
/// outside this lane's file allowlist. Any pairing/connect call site can adopt it directly,
/// e.g.:
///
/// ```swift
/// do {
///     try await client.connect()
/// } catch {
///     if DemoFallback.shouldOfferFixtureFallback(for: error) {
///         // present DemoWalkthroughView() or seed a session from
///         // DemoFallback.fixtureSessionOnConnectFailure() instead of showing a raw error.
///     }
/// }
/// ```
public enum DemoFallback {
    /// True for failures worth softening into "try the demo instead" rather than a bare
    /// error banner — timeouts and unreachable hosts, not auth failures (a bad token should
    /// still surface as a bad token, not silently swap in fixture data).
    public static func shouldOfferFixtureFallback(for error: Error) -> Bool {
        let nsError = error as NSError
        if nsError.domain == NSURLErrorDomain {
            let timeoutLike: Set<Int> = [
                NSURLErrorTimedOut,
                NSURLErrorCannotConnectToHost,
                NSURLErrorCannotFindHost,
                NSURLErrorNetworkConnectionLost,
                NSURLErrorNotConnectedToInternet,
            ]
            return timeoutLike.contains(nsError.code)
        }
        return false
    }

    /// The fixture session a caller can seed into its session list/detail view in place of a
    /// real connection, so a failed live pairing degrades to a working (offline) demo instead
    /// of a dead end.
    public static func fixtureSessionOnConnectFailure() -> LocalSessionRecord {
        DemoFixtures.session
    }
}
