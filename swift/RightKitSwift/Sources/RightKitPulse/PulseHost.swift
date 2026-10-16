import Foundation

/// Bounded, copy-on-receive event buffer. Overflow drops the oldest and counts it, never grows.
public final class PulseEventBuffer: @unchecked Sendable {
    private let lock = NSLock()
    private var items: [Data] = []
    private var bytes = 0
    private let maxEvents: Int, maxBytes: Int
    public private(set) var dropped = 0
    public init(maxEvents: Int = 256, maxBytes: Int = 1 << 20) { self.maxEvents = maxEvents; self.maxBytes = maxBytes }

    /// Copies the borrowed callback bytes; an oversized single event is dropped.
    public func append(copying raw: UnsafeRawBufferPointer) {
        lock.lock(); defer { lock.unlock() }
        guard raw.count <= maxBytes else { dropped += 1; return }
        items.append(Data(raw)); bytes += raw.count
        while items.count > maxEvents || bytes > maxBytes { bytes -= items.removeFirst().count; dropped += 1 }
    }
    public func drain() -> [Data] {
        lock.lock(); defer { lock.unlock() }
        let out = items; items = []; bytes = 0; return out
    }
}

/// Delivers exactly one terminal result, whatever races (completion, cancel, deadline).
final class PulseTerminal<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false
    private var cont: CheckedContinuation<T, Error>?
    private var early: Result<T, Error>?
    /// Returns true when this call won.
    @discardableResult func finish(_ r: Result<T, Error>) -> Bool {
        lock.lock()
        if done { lock.unlock(); return false }
        done = true
        if let c = cont { cont = nil; lock.unlock(); c.resume(with: r) } else { early = r; lock.unlock() }
        return true
    }
    func wait() async throws -> T {
        try await withCheckedThrowingContinuation { c in
            lock.lock()
            if let e = early { early = nil; lock.unlock(); c.resume(with: e) } else { cont = c; lock.unlock() }
        }
    }
}

public struct PulseCallResult: Sendable {
    public var payload: Data
    public var events: [Data]
    public var droppedEvents: Int
}

/// Pending call. `cancel()` detaches this observer only: the job may keep running (Resume).
public final class PulseCall: @unchecked Sendable {
    let terminal = PulseTerminal<PulseCallResult>()
    public let resume: PulseResumePolicy
    init(resume: PulseResumePolicy) { self.resume = resume }
    public func cancel() { terminal.finish(.failure(PulseError.cancelled)) }
    public func result() async throws -> PulseCallResult { try await terminal.wait() }
}

/// Owner of the shared host. Handle stays alive until every outstanding call has reached its
/// terminal completion (the native call returned) and only then is released; context is never freed
/// while a native callback may still run because release waits for that quiescence.
public final class PulseHost: @unchecked Sendable {
    private let abi: PulseNativeABI
    private let handle: PulseHostHandle
    private let lock = NSLock()
    private let quiescent = NSCondition()
    private var inFlight = 0
    private var shutDown = false
    private var released = false
    private(set) public var generation: UInt64
    private var connection: PulseConnectionHandle?
    private let queue = DispatchQueue(label: "rightkit.pulse.native", attributes: .concurrent)

    /// Registers the complete snapshot before start, checks ABI/contract/schema, then reconciles
    /// generation: Ready is returned only when the host snapshot generation matches the registered one.
    public static func start(abi: PulseNativeABI, config: PulseHostConfig) async throws -> PulseHost {
        let v = abi.versions()
        guard v == PulseVersions(abi: PulseCompat.abiVersion, contract: PulseCompat.contractVersion, schema: PulseCompat.schemaVersion)
        else { throw PulseError.incompatible(v) }
        let host: PulseHost = try await withCheckedThrowingContinuation { c in
            DispatchQueue.global().async {
                do {
                    try abi.registerSnapshot(config)
                    let h = try abi.hostStart()
                    c.resume(returning: PulseHost(abi: abi, handle: h, generation: config.generation))
                } catch { c.resume(throwing: error) }
            }
        }
        let snap = try await host.snapshot()
        guard snap.lifecycle == .ready, snap.generation == config.generation else {
            _ = try? await host.shutdown(budget: 1)
            throw PulseError.notReady
        }
        return host
    }

    private init(abi: PulseNativeABI, handle: PulseHostHandle, generation: UInt64) {
        self.abi = abi; self.handle = handle; self.generation = generation
    }

    private func enter() throws {
        lock.lock(); defer { lock.unlock() }
        if shutDown { throw PulseError.shuttingDown }
        inFlight += 1
    }
    private func leave() {
        quiescent.lock(); lock.lock(); inFlight -= 1; lock.unlock(); quiescent.broadcast(); quiescent.unlock()
    }

    private func setGeneration(_ g: UInt64) { lock.lock(); generation = g; lock.unlock() }
    private func currentConnection() -> PulseConnectionHandle? { lock.lock(); defer { lock.unlock() }; return connection }
    /// Returns true when shutdown was already underway.
    private func markShutDown() -> Bool { lock.lock(); defer { lock.unlock() }; let a = shutDown; shutDown = true; return a }
    struct Claim { var connection: PulseConnectionHandle? }
    /// Waits for in-flight native calls to finish; returns the release claim only when idle and unclaimed.
    private func awaitQuiescenceAndClaim(until limit: Date) -> Claim? {
        quiescent.lock(); defer { quiescent.unlock() }
        while true {
            lock.lock(); let n = inFlight; lock.unlock()
            if n == 0 || !quiescent.wait(until: limit) { break }
        }
        lock.lock(); defer { lock.unlock() }
        guard inFlight == 0, !released else { return nil }
        released = true
        let c = connection; connection = nil
        return Claim(connection: c)
    }

    private func offMain<T: Sendable>(_ work: @escaping @Sendable () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { c in
            queue.async { c.resume(with: Result { try work() }) }
        }
    }

    public func snapshot() async throws -> PulseSnapshot {
        try enter(); defer { leave() }
        let abi = abi, h = handle
        let s = try await offMain { try abi.hostSnapshot(h) }
        setGeneration(s.generation)
        return s
    }

    public func lifecycle(since: PulseLifecycle?, timeout: TimeInterval) async throws -> PulseLifecycle? {
        try enter(); defer { leave() }
        let abi = abi, h = handle, d = Date().addingTimeInterval(timeout)
        return try await offMain { try abi.hostLifecycle(h, since: since, deadline: d) }
    }

    /// Starts a call under one monotonic-style deadline. Native work runs off the caller's actor.
    public func call(_ method: String, params: Data, resume: PulseResumePolicy, timeout: TimeInterval) throws -> PulseCall {
        try enter()
        let call = PulseCall(resume: resume)
        let buf = PulseEventBuffer()
        let abi = abi, h = handle, deadline = Date().addingTimeInterval(timeout)
        queue.async { [self] in
            defer { leave() }
            do {
                let out = try abi.hostCall(h, method: method, params: params, resume: resume, deadline: deadline) {
                    buf.append(copying: $0)
                }
                call.terminal.finish(.success(PulseCallResult(payload: out, events: buf.drain(), droppedEvents: buf.dropped)))
            } catch { call.terminal.finish(.failure(error)) }
        }
        // Deadline backstop: the caller gets its one terminal even if the native side overruns.
        queue.asyncAfter(deadline: .now() + timeout) { call.terminal.finish(.failure(PulseError.deadlineExceeded)) }
        return call
    }

    public func serviceCall(_ method: String, params: Data, timeout: TimeInterval) async throws -> Data {
        try enter(); defer { leave() }
        let abi = abi, h = handle, deadline = Date().addingTimeInterval(timeout)
        let terminal = PulseTerminal<Data>()
        let existing = currentConnection()
        queue.async { [self] in
            do {
                let conn = try existing ?? abi.serviceConnect(h, deadline: deadline)
                lock.lock(); if connection == nil { connection = conn }; lock.unlock()
                terminal.finish(.success(try abi.serviceCall(conn, method: method, params: params, deadline: deadline)))
            } catch { terminal.finish(.failure(error)) }
        }
        queue.asyncAfter(deadline: .now() + timeout) { terminal.finish(.failure(PulseError.deadlineExceeded)) }
        return try await terminal.wait()
    }

    /// Quit path: refuses new work, requests shared-owner shutdown within its own budget, waits for
    /// in-flight native calls to quiesce, then releases handles. Window close must NOT call this.
    @discardableResult
    public func shutdown(budget: TimeInterval) async throws -> PulseShutdownReport {
        if markShutDown() { return PulseShutdownReport(outcome: .alreadyShuttingDown) }
        let abi = abi, h = handle
        let report: PulseShutdownReport = try await offMain { try abi.hostShutdown(h, budget: budget) }
        let conn = awaitQuiescenceAndClaim(until: Date().addingTimeInterval(budget))
        // Unconfirmed or still-busy: keep the host handle so Rust can keep reaping; never free under callbacks.
        if let claim = conn, report.outcome != .unconfirmed {
            if let c = claim.connection { abi.release(connection: c) }
            abi.release(host: h)
        }
        return report
    }

    /// Window disconnect: observers go away, the shared host and its jobs stay.
    public func windowClosed() { /* intentionally retains host, engine, and services */ }
}
