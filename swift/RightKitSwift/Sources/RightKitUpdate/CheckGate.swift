import Foundation

/// Debounce and cache policy for update checks, like `createCheckGate`: a call inside the window
/// returns the cached result, concurrent calls share one run, and a failure is never cached.
public actor CheckGate<Value: Sendable> {
    private let minInterval: TimeInterval
    private let now: @Sendable () -> Date
    private var cached: (at: Date, value: Value)?
    private var inflight: (id: Int, task: Task<Value, Error>)?
    private var counter = 0

    public init(minInterval: TimeInterval, now: @escaping @Sendable () -> Date = { Date() }) {
        self.minInterval = minInterval
        self.now = now
    }

    public func run(force: Bool = false, _ work: @escaping @Sendable () async throws -> Value) async throws -> Value {
        if !force {
            if let inflight { return try await inflight.task.value }
            if let cached, now().timeIntervalSince(cached.at) < minInterval { return cached.value }
        }
        counter += 1
        let id = counter
        let task = Task { try await work() }
        inflight = (id, task)
        do {
            let value = try await task.value
            cached = (now(), value)
            if inflight?.id == id { inflight = nil }
            return value
        } catch {
            if inflight?.id == id { inflight = nil }
            throw error
        }
    }

    /// Drops the cached result so the next run is real.
    public func reset() { cached = nil }
}

/// Reports a recurring failure once, then stays quiet until the next success (discovery-failure
/// gating: a flaky network must not log on every check).
public actor WarnOnceGate {
    private var warned = false
    public init() {}

    /// True the first time after construction or after ``recordSuccess()``.
    public func shouldWarn() -> Bool {
        if warned { return false }
        warned = true
        return true
    }

    public func recordSuccess() { warned = false }
}
