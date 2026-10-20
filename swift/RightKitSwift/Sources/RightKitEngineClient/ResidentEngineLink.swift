import Foundation

// The "when" half of a menu-bar or notch app's client of a resident engine, moved out of Pulse's
// `ResidentEngineClient`. `AttachedEngine` is the "how" (transport, peer check, reconnecting); this
// type only decides when to hold one. The app keeps what is its own: the app label it attaches as,
// the executable path, the setting that wants the engine, and the probe that says it is healthy.

public extension EngineError {
    /// What ``ResidentEngineLink`` throws while the engine is not wanted or not healthy. Retryable: the
    /// caller tries again on its own schedule.
    static let engineOff = EngineError(
        code: "engine_off", message: "the resident engine is not attached", retryable: true
    )
}

/// Holds an ``AttachedEngine`` only while the engine is wanted and healthy.
///
/// - It attaches lazily: nothing is created until a call (or ``current()``) finds the engine
///   available, and then exactly one engine is created and started, however many callers race.
/// - When the engine is no longer available (the setting turned off, the service unhealthy) the next
///   ``current()`` or ``call(_:params:timeout:)`` detaches and drops the engine, so no connection or
///   watcher thread is held. The resident engine itself keeps running.
/// - A later available call attaches a fresh engine.
///
/// The closures run on whatever thread asks, possibly under this type's lock for `makeEngine`: they
/// must be quick and must not call back into the link.
public final class ResidentEngineLink: Sendable {
    private let wanted: @Sendable () -> Bool
    private let healthy: @Sendable () -> Bool
    private let makeEngine: @Sendable () -> AttachedEngine
    private let held = Locked<AttachedEngine?>(nil)

    /// - Parameters:
    ///   - wanted: whether the app wants the engine now (a user setting, for example "agent resident").
    ///   - healthy: whether the engine's service reports healthy now.
    ///   - makeEngine: builds the engine to attach (not started; the link starts it).
    public init(
        wanted: @escaping @Sendable () -> Bool,
        healthy: @escaping @Sendable () -> Bool,
        makeEngine: @escaping @Sendable () -> AttachedEngine
    ) {
        self.wanted = wanted
        self.healthy = healthy
        self.makeEngine = makeEngine
    }

    /// True when the engine is wanted and healthy now.
    public var isAvailable: Bool { wanted() && healthy() }

    /// True while an engine is held.
    public var isAttached: Bool { held.withLock { $0 != nil } }

    /// The attached engine, attaching when available and detaching when not.
    public func current() -> AttachedEngine? {
        guard isAvailable else {
            detach()
            return nil
        }
        let (engine, created) = held.withLock { slot -> (AttachedEngine, Bool) in
            if let existing = slot { return (existing, false) }
            let created = makeEngine()
            slot = created
            return (created, true)
        }
        if created { engine.start() }
        return engine
    }

    /// Stops watching and closes the connections. The engine keeps running.
    public func detach() {
        let old = held.withLock { slot -> AttachedEngine? in
            let old = slot
            slot = nil
            return old
        }
        old?.detach()
    }

    /// Calls one engine method. Throws ``EngineError/engineOff`` (retryable) while the engine is not
    /// available.
    public func call(_ method: String, params: Data, timeout: Duration? = nil) async throws -> Data {
        guard let engine = current() else { throw EngineError.engineOff }
        return try await engine.call(method, params: params, timeout: timeout)
    }
}
