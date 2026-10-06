#if canImport(ActivityKit) && os(iOS)
@preconcurrency import ActivityKit
import Foundation

/// Typed start failures so callers can tell Settings denial from visibility limits and request
/// failures; a Bool cannot carry the right remedy.
public enum LiveActivityStartError: Error, LocalizedError {
    case disabled
    case authorization(ActivityAuthorizationError)
    case stateBlocked
    case superseded
    case requestFailed(domain: String, code: Int, description: String)

    public var errorDescription: String? {
        switch self {
        case .disabled: "Live Activities are disabled."
        case .authorization(let error): error.localizedDescription
        case .stateBlocked: "The Live Activity is already in a terminal state."
        case .superseded: "The Live Activity start was superseded."
        case .requestFailed(_, _, let description): description
        }
    }
}

/// Fenced start/adopt/update/end of one Activity type. Owns the sequencing that is hard to get
/// right (enabled check, adopt a matching orphan, end foreign orphans, supersede races, typed
/// errors). Content, copy, alerts and timing stay in the app; the widget UI stays in the app's
/// widget extension (see the app template).
/// Not actor-bound: async methods run on the caller's actor (`#isolation`), so a `@MainActor`
/// controller keeps ActivityKit calls and fence updates serialised without sending state across actors.
public final class LiveActivityHost<Attributes: ActivityAttributes> where Attributes.ContentState: Sendable {
    public enum StartOutcome { case adopted, started }

    public private(set) var activity: Activity<Attributes>?
    private var fence = LifecycleFence()

    public init() {}

    public func startToken() -> UInt64 { fence.advance() }
    public func isCurrent(_ token: UInt64) -> Bool { fence.isCurrent(token) }

    /// - Parameter isMatchingOrphan: true for an existing system activity that belongs to the
    ///   session being started and should be adopted instead of replaced.
    public func start(
        attributes: Attributes,
        content: ActivityContent<Attributes.ContentState>,
        isMatchingOrphan: (Activity<Attributes>) -> Bool,
        isolation: isolated (any Actor)? = #isolation
    ) async throws -> StartOutcome {
        guard !Task.isCancelled else { throw LiveActivityStartError.superseded }
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { throw LiveActivityStartError.disabled }
        let token = fence.advance()
        if let match = Activity<Attributes>.activities.first(where: isMatchingOrphan) {
            activity = match
            return .adopted
        }
        for orphan in Activity<Attributes>.activities {
            await orphan.end(nil, dismissalPolicy: .immediate)
        }
        guard fence.isCurrent(token) else { throw LiveActivityStartError.superseded }
        do {
            activity = try Activity.request(attributes: attributes, content: content, pushType: nil)
            return .started
        } catch {
            activity = nil
            if let authorization = error as? ActivityAuthorizationError { throw LiveActivityStartError.authorization(authorization) }
            let nsError = error as NSError
            throw LiveActivityStartError.requestFailed(domain: nsError.domain, code: nsError.code, description: error.localizedDescription)
        }
    }

    public func update(_ content: ActivityContent<Attributes.ContentState>, alert: AlertConfiguration? = nil, isolation: isolated (any Actor)? = #isolation) async {
        await activity?.update(content, alertConfiguration: alert)
    }

    /// Ends and releases the activity. Advances the fence so any in-flight start or delayed end for
    /// the old activity is recognised as stale by `isCurrent`.
    public func end(_ content: ActivityContent<Attributes.ContentState>?, policy: ActivityUIDismissalPolicy, isolation: isolated (any Actor)? = #isolation) async {
        _ = fence.advance()
        guard let activity else { return }
        self.activity = nil
        await activity.end(content, dismissalPolicy: policy)
    }
}
#endif
