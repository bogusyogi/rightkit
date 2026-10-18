import Foundation

/// What happens to a running call when the caller stops waiting for it (the call's `timeout`
/// passes, or the Swift task is cancelled). Maps to `rkp_host_call`'s `resume` argument.
public enum EngineResumePolicy: Sendable, Equatable {
    /// The engine keeps working and stays up; only the wait ends. The default.
    case resume
    /// A wait that times out is treated as a wedged engine: the supervisor kills and restarts it.
    case never
}

/// An engine this process owns as a stdio child, supervised by `rightkit-job-queue`'s
/// `EngineSupervisor` through its C ABI (`rightkit-engine-native`, ABI 1). It ships first and is
/// what a Mac menu-bar or notch app uses.
///
/// The engine is started with ``start()``, or on the first call. Starting runs, in order: check
/// the native ABI version, install the engine command, register the snapshot (generation 1),
/// start the host. Every blocking ABI call runs on a thread of its own, so the main thread and
/// the cooperative pool never wait on the engine.
///
/// ## Process-wide state
/// The ABI keeps ONE engine command and ONE snapshot per process. Two `SupervisedEngine` values
/// with different executables, arguments or snapshots in one process would overwrite each other:
/// the second install replaces the command for later starts, and a snapshot that differs from the
/// registered one is refused (`stale_generation`). Create one `SupervisedEngine` per process.
///
/// ## Cancellation
/// The ABI has no way to interrupt a call that is waiting. Cancelling the Swift task detaches the
/// wait: ``call(_:params:timeout:resume:onEvent:)`` throws `CancellationError` at once and stops
/// delivering events, while the native call runs to its result or its timeout on its own thread
/// and the result is dropped. With ``EngineResumePolicy/resume`` the engine keeps the work;
/// with ``EngineResumePolicy/never`` a timed-out wait kills the engine.
///
/// A cancelled call keeps its two threads until its timeout, so cancelling and reissuing in a loop
/// is bounded: a method that was just cancelled waits 100 ms, doubling per consecutive cancel up to
/// 5 s, before its next call starts, and while 8 cancelled calls are still running a new call
/// fails at once with the retryable `busy`.
public final class SupervisedEngine: EngineClient {
    /// The engine executable the supervisor starts.
    public let enginePath: URL
    /// Arguments passed to the engine executable.
    public let arguments: [String]
    /// The engine method that receives the snapshot on every boot (default `engine.init`).
    public let snapshotMethod: String
    /// The snapshot: a JSON object the engine receives through `snapshotMethod` on every boot and
    /// restart.
    public let snapshotParams: Data
    /// How long ``start()`` waits for the first `Ready`.
    public let startBudget: Duration
    /// Environment variable names passed to the engine beyond the supervisor's default allowlist
    /// (PATH, HOME, USER, LANG, TMPDIR, USERPROFILE, LOCALAPPDATA and the like). The engine
    /// otherwise starts with a cleared environment. Always includes ``requiredEnvironmentKeys``.
    public let environmentKeys: [String]
    /// Names always passed beyond the supervisor's scrubbed default. Empty: RightKit names no
    /// product-specific variable; the app lists the ones its engine needs in `environmentKeys`.
    /// A name that is not set in this process is simply not passed.
    public static let requiredEnvironmentKeys: [String] = []
    /// The wait for a call that passes no `timeout`.
    public let defaultCallTimeout: Duration
    /// The resume policy of ``call(_:params:timeout:onEvent:)`` (the protocol form has no argument for it).
    public let defaultResumePolicy: EngineResumePolicy

    private let state = Locked(State())

    /// The snapshot generation this engine registers. The ABI fences hosts by generation; a
    /// process that never replaces its snapshot stays on 1.
    private static let snapshotGeneration: UInt64 = 1

    public init(
        enginePath: URL,
        arguments: [String] = [],
        snapshotMethod: String = "engine.init",
        snapshotParams: Data = Data("{}".utf8),
        startBudget: Duration = .seconds(60),
        environmentKeys: [String] = [],
        defaultCallTimeout: Duration = .seconds(60),
        defaultResumePolicy: EngineResumePolicy = .resume
    ) {
        self.enginePath = enginePath
        self.arguments = arguments
        self.snapshotMethod = snapshotMethod
        self.snapshotParams = snapshotParams
        self.startBudget = startBudget
        var keys = Self.requiredEnvironmentKeys
        for key in environmentKeys where !keys.contains(key) { keys.append(key) }
        self.environmentKeys = keys
        self.defaultCallTimeout = defaultCallTimeout
        self.defaultResumePolicy = defaultResumePolicy
    }

    deinit {
        // Dropping the engine without `shutdown` releases the handle; the library then stops the
        // child in the background and keeps counting it until it is confirmed gone.
        let (host, subscribers) = state.withLock { s -> (UInt64?, [AsyncStream<EngineLifecycle>.Continuation]) in
            var running: UInt64?
            if case .running(let handle) = s.phase { running = handle }
            s.phase = .stopped
            let all = Array(s.subscribers.values)
            s.subscribers.removeAll()
            return (running, all)
        }
        if let host { Native.releaseHost(host) }
        for subscriber in subscribers { subscriber.finish() }
    }

    // MARK: Start

    /// Starts the engine and returns once it is `Ready`. Safe to call more than once and from
    /// several tasks: they share one start. A failed start can be retried.
    public func start() async throws {
        _ = try await ensureStarted()
    }

    private func ensureStarted() async throws -> UInt64 {
        enum Step {
            case ready(UInt64)
            case join(Task<UInt64, Error>)
            case stopped
        }
        let step = state.withLock { s -> Step in
            switch s.phase {
            case .running(let host):
                return .ready(host)
            case .starting(let task):
                return .join(task)
            case .stopped:
                return .stopped
            case .idle:
                let task = Self.launchStart(
                    state: state,
                    settings: StartSettings(
                        enginePath: enginePath.path,
                        arguments: arguments,
                        environmentKeys: environmentKeys,
                        snapshotMethod: snapshotMethod,
                        snapshotParams: snapshotParams,
                        startBudget: startBudget
                    )
                )
                s.phase = .starting(task)
                return .join(task)
            }
        }
        switch step {
        case .ready(let host):
            return host
        case .join(let task):
            return try await task.value
        case .stopped:
            throw Self.stoppedError
        }
    }

    /// How long a start that finished after its engine was shut down waits for the child to exit
    /// before the library kills it.
    private static let lateStopBudgetMilliseconds: UInt64 = 5000

    private static let stoppedError = EngineError(
        code: "engine_stopped",
        message: "the engine was shut down",
        retryable: false
    )

    private struct StartSettings: Sendable {
        let enginePath: String
        let arguments: [String]
        let environmentKeys: [String]
        let snapshotMethod: String
        let snapshotParams: Data
        let startBudget: Duration
    }

    /// The start task. It holds `state` and the settings, not the engine, so dropping the engine
    /// while a start is in flight is safe.
    private static func launchStart(state: Locked<State>, settings: StartSettings) -> Task<UInt64, Error> {
        Task {
            do {
                let host = try await runOnThread(named: "rightkit-engine-start") {
                    Result { try startNative(settings) }
                }.get()
                let accepted = state.withLock { s -> Bool in
                    guard case .starting = s.phase else { return false }
                    s.phase = .running(host)
                    return true
                }
                guard accepted else {
                    // The engine was shut down or dropped while it started (a shutdown whose budget
                    // ran out stopped waiting for this start). Nobody will use this host and no
                    // half-started engine may stay behind: stop the child within a bounded time,
                    // then release the handle.
                    _ = await runOnThread(named: "rightkit-engine-late-stop") {
                        let report = Native.shutdown(host: host, budgetMilliseconds: lateStopBudgetMilliseconds)
                        Native.releaseHost(host)
                        return report
                    }
                    throw stoppedError
                }
                publish(.ready, to: state)
                startLifecyclePump(host: host, state: state)
                return host
            } catch {
                state.withLock { s in
                    if case .starting = s.phase { s.phase = .idle }
                }
                publish(.down, to: state)
                throw error
            }
        }
    }

    /// check versions, install engine, register snapshot, start host. Blocking.
    private static func startNative(_ settings: StartSettings) throws -> UInt64 {
        try Native.checkVersions()
        try Native.installEngine(
            path: settings.enginePath,
            arguments: settings.arguments,
            environmentKeys: settings.environmentKeys,
            snapshotMethod: settings.snapshotMethod,
            startBudgetMilliseconds: UInt32(clamping: settings.startBudget.nativeMilliseconds)
        )
        try Native.registerSnapshot(settings.snapshotParams, generation: snapshotGeneration)
        return try Native.startHost()
    }

    // MARK: Calls

    public func call(
        _ method: String,
        params: Data,
        timeout: Duration?,
        onEvent: (@Sendable (Data) -> Void)?
    ) async throws -> Data {
        try await call(method, params: params, timeout: timeout, resume: defaultResumePolicy, onEvent: onEvent)
    }

    /// ``EngineClient/call(_:params:timeout:onEvent:)`` with an explicit resume policy.
    ///
    /// `onEvent` receives each progress frame the command streams before its response. Frames are
    /// copied out of the native callback and handed to `onEvent` from a separate task, so a slow
    /// `onEvent` never blocks the engine; all frames are delivered before this method returns or
    /// throws an engine error, but none are delivered after cancellation.
    public func call(
        _ method: String,
        params: Data,
        timeout: Duration?,
        resume: EngineResumePolicy,
        onEvent: (@Sendable (Data) -> Void)?
    ) async throws -> Data {
        try Task.checkCancellation()
        let host = try await ensureStarted()
        try Task.checkCancellation()
        let state = self.state
        try await Self.admit(method, state: state)
        let timeoutMilliseconds = (timeout ?? defaultCallTimeout).nativeMilliseconds
        let pump = onEvent.map { EventPump(handler: $0) }
        let wait = CallWait()
        let track = CallTrack()
        do {
            let data = try await withTaskCancellationHandler {
                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Data, Error>) in
                    guard wait.install(continuation) else { return }
                    if track.markStarted() { Self.noteDetached(method, in: state) }
                    let thread = Thread {
                        let result = Native.call(
                            host: host,
                            method: method,
                            params: params,
                            resume: resume,
                            timeoutMilliseconds: timeoutMilliseconds,
                            pump: pump
                        )
                        // The native call has returned: no callback runs or can start now.
                        pump?.finish()
                        if track.markFinished() {
                            Self.noteDetachedFinished(in: state)
                        } else if case .success = result {
                            Self.noteCompleted(method, in: state)
                        }
                        wait.complete(result)
                    }
                    thread.name = "rightkit-engine-call"
                    thread.start()
                }
            } onCancel: {
                pump?.cancel()
                wait.cancel()
                if track.markCancelled() { Self.noteDetached(method, in: state) }
            }
            await pump?.drain()
            return data
        } catch is CancellationError {
            pump?.cancel()
            throw CancellationError()
        } catch {
            await pump?.drain()
            throw error
        }
    }

    // MARK: Cancel and reissue bounds

    /// Most cancelled calls whose native thread is still running (each holds two threads until its
    /// timeout, because the ABI cannot interrupt a wait). A call beyond this fails at once with the
    /// retryable `busy` instead of adding threads.
    static let maxDetachedCalls = 8
    private static let backoffBase: Duration = .milliseconds(100)
    private static let backoffCap: Duration = .seconds(5)

    /// Refuses a call while too many cancelled calls still hold threads, and delays a call whose
    /// method was just cancelled, doubling the delay with each consecutive cancel (100 ms up to
    /// 5 s). The delay is a cancellable sleep, so a caller that gives up again costs no thread.
    private static func admit(_ method: String, state: Locked<State>) async throws {
        let verdict = state.withLock { s -> (busy: Bool, until: ContinuousClock.Instant?) in
            if s.detachedCalls >= maxDetachedCalls { return (true, nil) }
            if let until = s.notBefore[method], until > ContinuousClock.now { return (false, until) }
            return (false, nil)
        }
        if verdict.busy {
            throw EngineError(
                code: "busy",
                message: "too many cancelled engine calls are still running; retry shortly",
                retryable: true
            )
        }
        if let until = verdict.until {
            try await Task.sleep(until: until, clock: .continuous)
        }
    }

    private static func noteDetached(_ method: String, in state: Locked<State>) {
        state.withLock { s in
            s.detachedCalls += 1
            let streak = min((s.cancelStreak[method] ?? 0) + 1, 16)
            s.cancelStreak[method] = streak
            let delay = min(backoffBase * (1 << min(streak - 1, 10)), backoffCap)
            s.notBefore[method] = ContinuousClock.now + delay
        }
    }

    private static func noteDetachedFinished(in state: Locked<State>) {
        state.withLock { s in s.detachedCalls = max(s.detachedCalls - 1, 0) }
    }

    /// A call that ran to a result for its caller ends the method's cancel streak.
    private static func noteCompleted(_ method: String, in state: Locked<State>) {
        state.withLock { s in
            guard s.cancelStreak[method] != nil else { return }
            s.cancelStreak.removeValue(forKey: method)
            s.notBefore.removeValue(forKey: method)
        }
    }

    /// Whether one native call is still wanted, cancelled while it runs, or over.
    private final class CallTrack: Sendable {
        private enum Phase: Sendable { case idle, running, cancelledEarly, detached, done }
        private let phase = Locked<Phase>(.idle)

        /// The native call is about to start. True when the caller was cancelled before this point
        /// (the call still runs, now detached, and must be counted).
        func markStarted() -> Bool {
            phase.withLock { p -> Bool in
                if p == .cancelledEarly {
                    p = .detached
                    return true
                }
                p = .running
                return false
            }
        }

        /// The caller cancelled. True when a native call is running detached.
        func markCancelled() -> Bool {
            phase.withLock { p -> Bool in
                switch p {
                case .running:
                    p = .detached
                    return true
                case .idle:
                    p = .cancelledEarly
                    return false
                default:
                    return false
                }
            }
        }

        /// The native call returned. True when it had been detached.
        func markFinished() -> Bool {
            phase.withLock { p -> Bool in
                let wasDetached = p == .detached
                p = .done
                return wasDetached
            }
        }
    }

    /// `events.subscribe` is not published by RightKit yet.
    public func subscribe(_ topics: [String]) -> AsyncThrowingStream<EngineEvent, Error> {
        AsyncThrowingStream<EngineEvent, Error> { continuation in
            continuation.finish(
                throwing: EngineError(
                    code: "unsupported",
                    message: "events.subscribe is not published yet",
                    retryable: false
                )
            )
        }
    }

    // MARK: Lifecycle

    /// The lifecycle, starting with the current state: `booting` until the engine has started
    /// (and while it starts), then whatever the supervisor reports, then `down` after a failed
    /// start, a shutdown or a release.
    public var lifecycle: AsyncStream<EngineLifecycle> {
        let (stream, continuation) = AsyncStream<EngineLifecycle>.makeStream(bufferingPolicy: .unbounded)
        let id = UUID()
        // Registering and yielding the current value under one lock keeps the order: a change
        // published meanwhile waits for the lock and arrives after it.
        state.withLock { s in
            s.subscribers[id] = continuation
            continuation.yield(s.lifecycle)
        }
        continuation.onTermination = { [state] _ in
            state.withLock { s in
                _ = s.subscribers.removeValue(forKey: id)
            }
        }
        return stream
    }

    private static func publish(_ lifecycle: EngineLifecycle, to state: Locked<State>) {
        state.withLock { s in
            guard s.lifecycle != lifecycle else { return }
            s.lifecycle = lifecycle
            for continuation in s.subscribers.values { continuation.yield(lifecycle) }
        }
    }

    /// One thread per engine that polls `rkp_host_lifecycle` in its blocking form and publishes
    /// each change. It holds `state` and the handle, never the engine, and ends when the library
    /// reports the handle released (or fails).
    private static func startLifecyclePump(host: UInt64, state: Locked<State>) {
        let thread = Thread {
            var since = Native.lifecycleCode(.ready)
            while true {
                switch Native.pollLifecycle(host: host, since: since, timeoutMilliseconds: 1000) {
                case .changed(let lifecycle):
                    since = Native.lifecycleCode(lifecycle)
                    publish(lifecycle, to: state)
                case .unchanged:
                    continue
                case .released, .failed:
                    publish(.down, to: state)
                    return
                }
            }
        }
        thread.name = "rightkit-engine-lifecycle"
        thread.start()
    }

    // MARK: Shutdown

    /// Quit path: stops the child within `budget`, releases the handle and reports how it ended.
    /// An engine that was never started reports `.noEngine`. After this the engine is stopped for
    /// good: later calls throw `engine_stopped`.
    ///
    /// A shutdown that arrives during a start waits for the start to finish or fail, but only until
    /// `budget` has passed; the child then gets the time that is left. If the start is still running
    /// when the budget ends, the engine is marked stopped, the report is `.unconfirmed`, and the
    /// start task stops and releases its host itself when it completes, so no half-started engine
    /// outlives the shutdown.
    @discardableResult
    public func shutdown(budget: Duration) async -> EngineShutdownReport {
        let deadline = ContinuousClock.now + budget
        while true {
            enum Step {
                case nothing
                case host(UInt64)
                case wait(Task<UInt64, Error>)
            }
            let step = state.withLock { s -> Step in
                switch s.phase {
                case .idle, .stopped:
                    s.phase = .stopped
                    return .nothing
                case .starting(let task):
                    return .wait(task)
                case .running(let host):
                    s.phase = .stopped
                    return .host(host)
                }
            }
            switch step {
            case .nothing:
                Self.publish(.down, to: state)
                return EngineShutdownReport(outcome: .noEngine)
            case .wait(let task):
                // Let the start finish (or fail), then decide again: it ends `.running` or `.idle`.
                if await Self.waitForStart(task, until: deadline) { continue }
                // Out of budget. Stop the engine unless the start completed in the meantime.
                let abandoned = state.withLock { s -> Bool in
                    guard case .starting = s.phase else { return false }
                    s.phase = .stopped
                    return true
                }
                if abandoned {
                    Self.publish(.down, to: state)
                    return EngineShutdownReport(outcome: .unconfirmed, unreaped: 1)
                }
            case .host(let host):
                let remaining = max(deadline - ContinuousClock.now, .milliseconds(1))
                let report = await Self.runOnThread(named: "rightkit-engine-shutdown") {
                    let report = Native.shutdown(host: host, budgetMilliseconds: remaining.nativeMilliseconds)
                    // The host stays registered after a shutdown until it is released; a release
                    // with an unreaped engine keeps a bounded reaper running in the library.
                    Native.releaseHost(host)
                    return report
                }
                Self.publish(.down, to: state)
                return report
            }
        }
    }

    /// True when `task` finished (successfully or not) before `deadline`; false when the deadline
    /// came first. Never cancels `task`, and neither waiter outlives the answer by more than the
    /// remaining budget.
    private static func waitForStart(_ task: Task<UInt64, Error>, until deadline: ContinuousClock.Instant) async -> Bool {
        await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
            let answered = Locked(false)
            let answer: @Sendable (Bool) -> Void = { value in
                let first = answered.withLock { done -> Bool in
                    if done { return false }
                    done = true
                    return true
                }
                if first { continuation.resume(returning: value) }
            }
            let timer = Task {
                do { try await Task.sleep(until: deadline, clock: .continuous) } catch { return }
                answer(false)
            }
            Task {
                _ = try? await task.value
                timer.cancel()
                answer(true)
            }
        }
    }

    // MARK: State

    fileprivate enum Phase: Sendable {
        case idle
        case starting(Task<UInt64, Error>)
        case running(UInt64)
        case stopped
    }

    fileprivate struct State: Sendable {
        var phase: Phase = .idle
        /// `booting` until the first start finishes; then mirrors the native lifecycle.
        var lifecycle: EngineLifecycle = .booting
        var subscribers: [UUID: AsyncStream<EngineLifecycle>.Continuation] = [:]
        /// Cancelled calls whose native thread has not returned yet.
        var detachedCalls = 0
        /// Consecutive cancelled calls per method, and when the next one may start.
        var cancelStreak: [String: Int] = [:]
        var notBefore: [String: ContinuousClock.Instant] = [:]
    }

    /// Runs blocking `work` on a thread of its own and resumes the caller with its result.
    fileprivate static func runOnThread<T: Sendable>(
        named name: String,
        _ work: @escaping @Sendable () -> T
    ) async -> T {
        await withCheckedContinuation { (continuation: CheckedContinuation<T, Never>) in
            let thread = Thread { continuation.resume(returning: work()) }
            thread.name = name
            thread.start()
        }
    }
}
