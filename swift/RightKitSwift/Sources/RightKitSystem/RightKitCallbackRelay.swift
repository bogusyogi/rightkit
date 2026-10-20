import Foundation

// The mechanics of a native Swift surface that reports back to a foreign owner (a Rust or Tauri engine
// reached through C function pointers), moved out of HeardRight's `PillCallbacks` (PK16-2). The
// product keeps what is its own: the C ABI, the callback payloads and the domain actions (copy text,
// open the hub, stop recording, ...). Those stay thin closures over this relay.

/// Runs callbacks away from the caller's thread and holds back log lines until the owner has
/// registered its callbacks.
///
/// - ``perform(_:)`` hands an action to a background queue, so a UI event handler on the main queue
///   never waits on the owner (which may itself be waiting for the main queue).
/// - ``log(_:_:)`` buffers lines while the owner is not ready (a bounded buffer: the OLDEST line is
///   dropped past the limit) and delivers the buffered lines, oldest first, when ``markReady()`` is
///   called. After that every line is delivered in call order through one serial queue.
public final class RightKitCallbackRelay: Sendable {
    private struct State: Sendable {
        var ready = false
        var pending: [(surface: String, message: String)] = []
    }

    private let state = RelayLock(State())
    private let pendingLimit: Int
    private let sink: @Sendable (String, String) -> Void
    private let logQueue = DispatchQueue(label: "rightkit.callback-relay.log", qos: .userInitiated)
    private let actionQueue: DispatchQueue

    /// - Parameters:
    ///   - pendingLimit: lines kept while not ready (at least 1). The default matches the shipped pill.
    ///   - actionQueue: where ``perform(_:)`` runs; a concurrent background queue by default so one slow
    ///     owner callback does not hold up the others.
    ///   - sink: delivers one log line to the owner. Called off the caller's thread, in order.
    public init(
        pendingLimit: Int = 256,
        actionQueue: DispatchQueue = DispatchQueue.global(qos: .userInitiated),
        sink: @escaping @Sendable (_ surface: String, _ message: String) -> Void
    ) {
        self.pendingLimit = max(1, pendingLimit)
        self.actionQueue = actionQueue
        self.sink = sink
    }

    /// True once the owner's callbacks are registered.
    public var isReady: Bool { state.withLock { $0.ready } }

    /// How many lines are waiting for ``markReady()``.
    public var pendingCount: Int { state.withLock { $0.pending.count } }

    /// The owner has registered its callbacks: delivers every buffered line, oldest first, and
    /// everything logged afterwards goes straight through. Idempotent.
    public func markReady() {
        let backlog = state.withLock { s -> [(surface: String, message: String)] in
            s.ready = true
            let backlog = s.pending
            s.pending.removeAll(keepingCapacity: false)
            return backlog
        }
        for line in backlog { deliver(line.surface, line.message) }
    }

    /// Delivers `message` for `surface`, or buffers it until ``markReady()``.
    public func log(_ surface: String, _ message: String) {
        let deliverNow = state.withLock { s -> Bool in
            if s.ready { return true }
            if s.pending.count >= pendingLimit { s.pending.removeFirst() }
            s.pending.append((surface, message))
            return false
        }
        if deliverNow { deliver(surface, message) }
    }

    /// Runs `action` on the action queue, never on the caller's thread.
    public func perform(_ action: @escaping @Sendable () -> Void) {
        actionQueue.async(execute: action)
    }

    /// Returns once every line handed to the sink before this call has been delivered (shutdown, tests).
    public func waitUntilDelivered() {
        logQueue.sync {}
    }

    private func deliver(_ surface: String, _ message: String) {
        let sink = self.sink
        logQueue.async { sink(surface, message) }
    }
}

/// Runs work on the main thread without the two classic mistakes: dispatching when already there (an
/// event ordering change) and `DispatchQueue.main.sync` from the main thread (a deadlock).
public enum RightKitMainThread {
    /// Runs `action` now when already on the main thread, else asynchronously on the main queue.
    public static func run(_ action: @escaping @MainActor @Sendable () -> Void) {
        if Thread.isMainThread {
            MainActor.assumeIsolated { action() }
        } else {
            DispatchQueue.main.async { MainActor.assumeIsolated { action() } }
        }
    }

    /// Runs `action` on the main thread and returns its result: inline when already there, else
    /// synchronously on the main queue. Never call it while the main thread can be waiting on the
    /// caller.
    public static func sync<T: Sendable>(_ action: @MainActor @Sendable () -> T) -> T {
        if Thread.isMainThread { return MainActor.assumeIsolated { action() } }
        return DispatchQueue.main.sync { MainActor.assumeIsolated { action() } }
    }
}

/// A value guarded by a lock (the relay's one piece of shared state).
private final class RelayLock<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Value
    init(_ value: Value) { self.value = value }
    func withLock<R>(_ body: (inout Value) -> R) -> R {
        lock.lock()
        defer { lock.unlock() }
        return body(&value)
    }
}
