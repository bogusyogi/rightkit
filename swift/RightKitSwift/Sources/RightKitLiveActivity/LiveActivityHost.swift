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
/// `Attributes` must be `Sendable` (any value-type attribute struct is; a `public` one needs the
/// explicit conformance): ActivityKit's async calls run off the caller's actor, so the attributes
/// conformance must not be actor-isolated.
public final class LiveActivityHost<Attributes: ActivityAttributes & Sendable> where Attributes.ContentState: Sendable {
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

/// Keyed variant of `LiveActivityHost`: one Live Activity per key (a bot id, a session id) of one
/// Activity type, each started, adopted, updated, alerted and ended independently under its own
/// fence. Unlike `LiveActivityHost.start`, starting a key never ends another key's activity, and
/// system activities that no key has claimed yet (after a process restart) are ended only by an
/// explicit `endForeignOrphans(isClaimed:)`, which spares every orphan some key still claims.
/// Same isolation model as `LiveActivityHost`: async methods run on the caller's actor.
public final class KeyedLiveActivityHost<Key: Hashable & Sendable, Attributes: ActivityAttributes & Sendable>
where Attributes.ContentState: Sendable {
    public enum StartOutcome { case adopted, started }

    public private(set) var activities: [Key: Activity<Attributes>] = [:]
    private var fences = KeyedLifecycleFence<Key>()

    public init() {}

    public func activity(for key: Key) -> Activity<Attributes>? { liveActivity(for: key) }
    public var keys: Set<Key> { Set(activities.keys) }

    public func startToken(for key: Key) -> UInt64 { fences.advance(key) }
    public func isCurrent(_ token: UInt64, for key: Key) -> Bool { fences.isCurrent(key, token) }

    /// Start or adopt the activity for `key`. Order: a tracked, still-live activity for the key is
    /// kept (`.adopted`); else a system activity that no other key tracks and `isMatchingOrphan`
    /// accepts is adopted; else a new one is requested. Nothing else is ended.
    public func start(
        key: Key,
        attributes: Attributes,
        content: ActivityContent<Attributes.ContentState>,
        isMatchingOrphan: (Activity<Attributes>) -> Bool,
        isolation: isolated (any Actor)? = #isolation
    ) async throws -> StartOutcome {
        guard !Task.isCancelled else { throw LiveActivityStartError.superseded }
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { throw LiveActivityStartError.disabled }
        let token = fences.advance(key)
        if liveActivity(for: key) != nil { return .adopted }
        let trackedIDs = Set(activities.values.map(\.id))
        if let match = Activity<Attributes>.activities.first(where: {
            !trackedIDs.contains($0.id) && Self.isLive($0) && isMatchingOrphan($0)
        }) {
            activities[key] = match
            return .adopted
        }
        guard fences.isCurrent(key, token) else { throw LiveActivityStartError.superseded }
        do {
            activities[key] = try Activity.request(attributes: attributes, content: content, pushType: nil)
            return .started
        } catch {
            activities[key] = nil
            if let authorization = error as? ActivityAuthorizationError { throw LiveActivityStartError.authorization(authorization) }
            let nsError = error as NSError
            throw LiveActivityStartError.requestFailed(domain: nsError.domain, code: nsError.code, description: error.localizedDescription)
        }
    }

    public func update(
        key: Key,
        content: ActivityContent<Attributes.ContentState>,
        alert: AlertConfiguration? = nil,
        isolation: isolated (any Actor)? = #isolation
    ) async {
        await liveActivity(for: key)?.update(content, alertConfiguration: alert)
    }

    /// Ends and releases `key`'s activity and advances its fence only.
    public func end(
        key: Key,
        content: ActivityContent<Attributes.ContentState>?,
        policy: ActivityUIDismissalPolicy,
        isolation: isolated (any Actor)? = #isolation
    ) async {
        fences.invalidate(key)
        guard let activity = activities.removeValue(forKey: key) else { return }
        await activity.end(content, dismissalPolicy: policy)
    }

    /// Ends every tracked activity whose key is not in `keys` (each under its own fence).
    public func endAll(
        except keys: Set<Key>,
        policy: ActivityUIDismissalPolicy = .immediate,
        isolation: isolated (any Actor)? = #isolation
    ) async {
        for key in Array(activities.keys) where !keys.contains(key) {
            await end(key: key, content: nil, policy: policy)
        }
    }

    /// Ends system activities of this type that no key tracks and that `isClaimed` says no key
    /// owns (for example, none of the live bots carries that `botId`). Run it after the keys have
    /// started or adopted, so a restart never ends an activity a key is about to adopt.
    public func endForeignOrphans(
        isClaimed: (Activity<Attributes>) -> Bool,
        isolation: isolated (any Actor)? = #isolation
    ) async {
        let trackedIDs = Set(activities.values.map(\.id))
        for orphan in Activity<Attributes>.activities where !trackedIDs.contains(orphan.id) && !isClaimed(orphan) {
            await orphan.end(nil, dismissalPolicy: .immediate)
        }
    }

    private func liveActivity(for key: Key) -> Activity<Attributes>? {
        guard let activity = activities[key] else { return nil }
        guard Self.isLive(activity) else {
            activities[key] = nil
            return nil
        }
        return activity
    }

    private static func isLive(_ activity: Activity<Attributes>) -> Bool {
        switch activity.activityState {
        case .active, .stale: true
        default: false
        }
    }
}
#endif
