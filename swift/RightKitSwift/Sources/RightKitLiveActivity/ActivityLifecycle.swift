import Foundation

/// Phase graph for one Live Activity. The app supplies its own phases; the package owns the guard
/// logic (session binding, terminal restart, adoption, watchdog) that is identical for every app.
public struct ActivityLifecycleRules<Phase: Hashable & Sendable>: Sendable {
    public let idle: Phase
    /// Phase entered by `begin`.
    public let start: Phase
    /// Phase a watchdog forces on a stalled active activity.
    public let failure: Phase
    public let terminal: Set<Phase>
    /// Phases that need a watchdog while active.
    public let watchdog: Set<Phase>
    public let transitions: [Phase: Set<Phase>]

    public init(idle: Phase, start: Phase, failure: Phase, terminal: Set<Phase>, watchdog: Set<Phase>, transitions: [Phase: Set<Phase>]) {
        self.idle = idle
        self.start = start
        self.failure = failure
        self.terminal = terminal
        self.watchdog = watchdog
        self.transitions = transitions
    }
}

/// Pure lifecycle guard. No ActivityKit dependency, so it runs in unit tests and extensions alike.
public struct ActivityLifecycle<Phase: Hashable & Sendable>: Sendable {
    public let rules: ActivityLifecycleRules<Phase>
    public private(set) var phase: Phase
    public private(set) var sessionID: String?

    public init(rules: ActivityLifecycleRules<Phase>) {
        self.rules = rules
        phase = rules.idle
    }

    public var isTerminal: Bool { rules.terminal.contains(phase) }
    public var needsWatchdog: Bool { rules.watchdog.contains(phase) }

    @discardableResult
    public mutating func begin(sessionID: String) -> Bool {
        guard phase == rules.idle || isTerminal else { return false }
        phase = rules.start
        self.sessionID = sessionID
        return true
    }

    /// Reattach after a process restart only when the system activity and the app's session agree.
    @discardableResult
    public mutating func adopt(sessionID: String, phase: Phase) -> Bool {
        guard phase != rules.idle else { return false }
        self.phase = phase
        self.sessionID = sessionID
        return true
    }

    @discardableResult
    public mutating func transition(to target: Phase, sessionID: String? = nil) -> Bool {
        guard let current = self.sessionID else { return false }
        if let sessionID, sessionID != current { return false }
        // Coalesced updates reaffirming the current phase are not invalid hops.
        if target == phase { return true }
        guard rules.transitions[phase, default: []].contains(target) else { return false }
        phase = target
        return true
    }

    /// A watchdog can only fail an active activity for its own session.
    @discardableResult
    public mutating func expireWatchdog(sessionID: String) -> Bool {
        guard self.sessionID == sessionID, needsWatchdog else { return false }
        phase = rules.failure
        return true
    }

    public mutating func reset() {
        phase = rules.idle
        sessionID = nil
    }
}

/// Monotonic token for operations that can replace controller state. ActivityKit awaits re-enter
/// the actor, so a slow cleanup must not clear state created by a newer start.
public struct LifecycleFence: Sendable {
    public private(set) var generation: UInt64 = 0
    public init() {}
    public mutating func advance() -> UInt64 {
        generation &+= 1
        return generation
    }
    public func isCurrent(_ token: UInt64) -> Bool { token == generation }
}
