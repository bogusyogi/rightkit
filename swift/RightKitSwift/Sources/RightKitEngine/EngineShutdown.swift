import Foundation

// Replaces the retired RightKitPulse shutdown types with the same semantics.

/// How an engine shutdown ended. The cases are ordered from best to worst.
public enum EngineShutdownOutcome: String, Sendable, Comparable {
    /// No engine was running.
    case noEngine
    /// Another caller had already started the shutdown.
    case alreadyShuttingDown
    /// The engine exited by itself within the budget.
    case exited
    /// The engine did not exit in time and was killed.
    case killed
    /// The supervisor could not confirm the engine is gone. Never a success.
    case unconfirmed

    private var rank: Int {
        switch self {
        case .noEngine: return 0
        case .alreadyShuttingDown: return 1
        case .exited: return 2
        case .killed: return 3
        case .unconfirmed: return 4
        }
    }

    public static func < (lhs: Self, rhs: Self) -> Bool { lhs.rank < rhs.rank }
}

/// The result of shutting an engine down.
public struct EngineShutdownReport: Sendable, Equatable {
    /// How the shutdown ended.
    public var outcome: EngineShutdownOutcome
    /// Child processes that were started by the engine and not reaped.
    public var unreaped: Int

    public init(outcome: EngineShutdownOutcome, unreaped: Int = 0) {
        self.outcome = outcome
        self.unreaped = unreaped
    }

    /// Only a reaped engine that exited by itself (or was never running) is clean;
    /// `unconfirmed` is never success.
    public var isClean: Bool {
        unreaped == 0 && (outcome == .exited || outcome == .noEngine)
    }
}
