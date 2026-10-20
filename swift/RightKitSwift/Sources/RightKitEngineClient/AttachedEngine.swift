import Foundation

/// An engine another process owns, reached over its local endpoint: a resident engine
/// executable, served by `rightkit-daemon` on an owner-only Unix socket. A client
/// attaches; it never starts, restarts or owns the engine (the OS service manager does).
///
/// ## Finding the engine
/// `resident.json` in the state folder (the `stateDirectory` the app passes) names the endpoint.
/// The file is only a hint: before a single byte is written, the connection asks the kernel who
/// is listening and refuses anyone but the user's own process whose executable is one of
/// `expectedExecutables` (`getpeereid`, `LOCAL_PEERPID`, `proc_pidpath`, the process start time
/// checked twice). A forged record or a squatted socket gets a connection that closes silently.
///
/// ## Lifecycle
/// `.booting` until the engine answers `doctor` with `ready: true` ("<engineName> is starting"),
/// `.ready` while a watch connection stays open, `.recovering` when it drops and
/// `resident.json` is still there (a crash: the OS restarts it), `.down` when the record is gone
/// (a clean Quit, not restarted) or the engine never appeared within `bootBudget`. In `.down` the
/// watcher stops (no idle polling); a call or ``retry()`` starts it again.
///
/// ## Calls
/// One connection per concurrent call (the host serves a connection in lock step), reused when
/// idle. A call that fails to connect throws the retryable `engine_starting`. A connection that
/// closes mid-call throws `delivery_unknown`: the engine may have run the request, so it is never
/// resent.
///
/// ## Events
/// `events.subscribe` is not published yet; ``subscribe(_:)`` long-polls the configured `eventsPollMethod`
/// (the engine's interim event read). A gap in the engine's log yields one `resync` event
/// (payload `{}`): re-read state, then continue.
///
/// Peer verification exists on macOS. Other platforms fail closed with `unsupported_platform`
/// (the Windows named-pipe client, which must check the pipe server's SID, is not written).
public final class AttachedEngine: EngineClient {
    /// A fixed endpoint (socket path), or nil to read it from `resident.json`.
    public let endpoint: String?
    /// The folder holding `resident.json`.
    public let stateDirectory: URL
    /// The executables the listener may be (the installed resident engine).
    public let expectedExecutables: [URL]
    /// The app label sent in `hello` (for example `"<product>-hub"`). A label picks the engine's
    /// per-app allowlist; it is not proof of identity.
    public let app: String
    /// The product name used in user-facing messages ("<engineName> is starting").
    public let engineName: String
    /// The engine's interim event-read method, long-polled by ``subscribe(_:)``
    /// (for example `"<product>.events.poll"`).
    public let eventsPollMethod: String
    /// The wait for a call that passes no `timeout`.
    public let defaultCallTimeout: Duration

    private let core: AttachedCore

    public init(
        endpoint: String? = nil,
        stateDirectory: URL,
        expectedExecutables: [URL],
        app: String,
        engineName: String,
        eventsPollMethod: String,
        defaultCallTimeout: Duration = .seconds(60),
        bootBudget: Duration = .seconds(60)
    ) {
        self.endpoint = endpoint
        self.stateDirectory = stateDirectory
        self.expectedExecutables = expectedExecutables
        self.app = app
        self.engineName = engineName
        self.eventsPollMethod = eventsPollMethod
        self.defaultCallTimeout = defaultCallTimeout
        self.core = AttachedCore(
            endpoint: endpoint,
            recordURL: stateDirectory.appendingPathComponent("resident.json"),
            expectedExecutables: expectedExecutables.map(\.path),
            app: app,
            engineName: engineName,
            bootBudget: bootBudget.attachedSeconds
        )
    }

    /// `~/Library/Application Support/<folderName>`: the usual state folder of a Mac product.
    /// RightKit names no product; the app passes its own folder name.
    public static func applicationSupportDirectory(named folderName: String) -> URL {
        URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
            .appendingPathComponent("Library/Application Support/\(folderName)", isDirectory: true)
    }

    deinit {
        core.stop()
    }

    /// Starts watching for the engine (idempotent). Calls and ``lifecycle`` do this on their own.
    public func start() {
        core.ensureWatcher()
    }

    /// After `.down`: look for the engine again.
    public func retry() {
        core.ensureWatcher()
    }

    /// Stops watching and closes every connection. The engine keeps running.
    public func detach() {
        core.stop()
    }

    /// Asks the engine to shut down (the notch's Quit). The engine closes the connection as it
    /// goes, so a connection that ends after the request counts as done.
    public func shutdownEngine() async throws {
        do {
            _ = try await call("shutdown", params: Data("{}".utf8), timeout: .seconds(10))
        } catch let error as EngineError where error.code == "delivery_unknown" {
            return
        }
    }

    // MARK: Calls

    public func call(
        _ method: String,
        params: Data,
        timeout: Duration?,
        onEvent: (@Sendable (Data) -> Void)?
    ) async throws -> Data {
        try Task.checkCancellation()
        core.ensureWatcher()
        let deadline = Date().addingTimeInterval((timeout ?? defaultCallTimeout).attachedSeconds)
        let pump = onEvent.map { EventPump(handler: $0) }
        let wait = CallWait()
        let holder = ConnectionHolder()
        let core = self.core
        do {
            let data = try await withTaskCancellationHandler {
                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Data, Error>) in
                    guard wait.install(continuation) else { return }
                    let thread = Thread {
                        let result = core.perform(
                            method: method,
                            params: params,
                            deadline: deadline,
                            pump: pump,
                            holder: holder
                        )
                        pump?.finish()
                        wait.complete(result)
                    }
                    thread.name = "rightkit-attached-call"
                    thread.start()
                }
            } onCancel: {
                pump?.cancel()
                holder.cancel()
                wait.cancel()
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

    // MARK: Events

    public func subscribe(_ topics: [String]) -> AsyncThrowingStream<EngineEvent, Error> {
        AsyncThrowingStream<EngineEvent, Error> { continuation in
            let task = Task { [weak self] in
                var after: UInt64 = 0
                while !Task.isCancelled {
                    guard let engine = self else {
                        continuation.finish()
                        return
                    }
                    do {
                        let body = try JSONSerialization.data(
                            withJSONObject: ["after": after, "topics": topics, "waitMs": 20_000] as [String: Any]
                        )
                        let reply = try await engine.call(
                            engine.eventsPollMethod,
                            params: body,
                            timeout: .seconds(40)
                        )
                        guard let object = try JSONSerialization.jsonObject(with: reply) as? [String: Any]
                        else {
                            throw AttachedFailure.protocolError("\(engine.eventsPollMethod) answered something else")
                        }
                        if let next = (object["next"] as? NSNumber)?.uint64Value { after = next }
                        if object["dropped"] as? Bool == true {
                            continuation.yield(EngineEvent(topic: "resync", payload: Data("{}".utf8)))
                        }
                        for row in object["events"] as? [[String: Any]] ?? [] {
                            guard let topic = row["topic"] as? String else { continue }
                            continuation.yield(
                                EngineEvent(topic: topic, payload: try AttachedJSON.value(row["payload"]))
                            )
                        }
                    } catch is CancellationError {
                        continuation.finish()
                        return
                    } catch let error as EngineError where error.retryable || error.code == "delivery_unknown" {
                        // The engine is starting, restarting or busy: wait, then continue from
                        // the same cursor (a restarted engine reports `dropped`).
                        try? await Task.sleep(for: .seconds(1))
                    } catch {
                        continuation.finish(throwing: error)
                        return
                    }
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    // MARK: Lifecycle

    public var lifecycle: AsyncStream<EngineLifecycle> {
        core.ensureWatcher()
        return core.lifecycleStream()
    }
}

// MARK: - Core

private extension Duration {
    var attachedSeconds: Double {
        let parts = components
        return Double(parts.seconds) + Double(parts.attoseconds) / 1e18
    }
}

/// Lets a task's cancellation interrupt the connection a call is using.
final class ConnectionHolder: Sendable {
    private struct State: Sendable {
        var connection: AttachedConnection?
        var cancelled = false
    }

    private let state = Locked(State())

    func set(_ connection: AttachedConnection) {
        let cancelled = state.withLock { s -> Bool in
            s.connection = connection
            return s.cancelled
        }
        if cancelled { connection.interrupt() }
    }

    func clear() {
        state.withLock { $0.connection = nil }
    }

    func cancel() {
        let connection = state.withLock { s -> AttachedConnection? in
            s.cancelled = true
            return s.connection
        }
        connection?.interrupt()
    }
}

/// The attached engine's shared state: the connection pool, the lifecycle and the watcher. A
/// class of its own so the watcher thread never keeps the public engine alive.
final class AttachedCore: Sendable {
    private let endpointOverride: String?
    private let recordURL: URL
    private let expectedExecutables: [String]
    private let app: String
    private let engineName: String
    private let bootBudget: Double
    private let wakeup = DispatchSemaphore(value: 0)

    private struct State: Sendable {
        var idle: [AttachedConnection] = []
        var lifecycle: EngineLifecycle = .booting
        var subscribers: [UUID: AsyncStream<EngineLifecycle>.Continuation] = [:]
        var nextRequest: UInt64 = 0
        var stopped = false
        var watching = false
        var watchConnection: AttachedConnection?
    }

    private let state = Locked(State())

    private static let maxIdle = 4

    init(
        endpoint: String?,
        recordURL: URL,
        expectedExecutables: [String],
        app: String,
        engineName: String,
        bootBudget: Double
    ) {
        self.endpointOverride = endpoint
        self.recordURL = recordURL
        self.expectedExecutables = expectedExecutables
        self.app = app
        self.engineName = engineName
        self.bootBudget = bootBudget
    }

    // MARK: Endpoint and connections

    /// `resident.json` as the daemon heartbeat writes it
    /// (`rightkit_daemon::ticker::spawn_heartbeat`, every second):
    /// `{"pid":N,"updated_ms":<Unix ms>,"state":{"endpoint":"<socket path>",...}}`. A flat
    /// `{"endpoint":"..."}` (a hand-written record) is also read.
    private struct Record: Decodable {
        struct State: Decodable {
            let endpoint: String
        }
        let updatedMs: Double?
        let state: State?
        let endpoint: String?

        enum CodingKeys: String, CodingKey {
            case updatedMs = "updated_ms"
            case state
            case endpoint
        }
    }

    /// A heartbeat older than this is not a running engine (it is written every second), so a
    /// record left behind by a crash or a kill does not name a socket to dial.
    static let recordFreshSeconds: Double = 30

    /// The endpoint a `resident.json` body names, or nil when the body is unreadable, names none, or
    /// is a heartbeat older than ``recordFreshSeconds``. A flat record has no timestamp and is
    /// accepted as written.
    static func endpoint(fromRecord data: Data, now: Date = Date()) -> String? {
        guard let record = try? JSONDecoder().decode(Record.self, from: data) else { return nil }
        if let state = record.state {
            guard !state.endpoint.isEmpty, let updatedMs = record.updatedMs,
                now.timeIntervalSince1970 - updatedMs / 1000 <= recordFreshSeconds
            else { return nil }
            return state.endpoint
        }
        guard let endpoint = record.endpoint, !endpoint.isEmpty else { return nil }
        return endpoint
    }

    private func resolveEndpoint() throws -> String {
        if let endpointOverride { return endpointOverride }
        guard let data = try? Data(contentsOf: recordURL), let endpoint = Self.endpoint(fromRecord: data) else {
            throw AttachedFailure.starting("no engine record yet", engineName: engineName)
        }
        return endpoint
    }

    private var recordExists: Bool {
        FileManager.default.fileExists(atPath: recordURL.path)
    }

    private func openConnection() throws -> AttachedConnection {
        try AttachedConnection.open(
            path: try resolveEndpoint(),
            app: app,
            engineName: engineName,
            expectedExecutables: expectedExecutables
        )
    }

    private func acquire() throws -> AttachedConnection {
        while true {
            let idle = state.withLock { s -> AttachedConnection? in
                if s.stopped { return nil }
                return s.idle.popLast()
            }
            guard let idle else { break }
            if idle.isReusable { return idle }
        }
        if state.withLock({ $0.stopped }) {
            throw EngineError(code: "engine_stopped", message: "the client was detached", retryable: false)
        }
        return try openConnection()
    }

    private func release(_ connection: AttachedConnection) {
        state.withLock { s in
            if !s.stopped && s.idle.count < Self.maxIdle { s.idle.append(connection) }
        }
    }

    /// Runs one call on the calling (worker) thread. Never throws: every failure is a `Result`.
    func perform(
        method: String,
        params: Data,
        deadline: Date,
        pump: EventPump?,
        holder: ConnectionHolder
    ) -> Result<Data, EngineError> {
        do {
            let connection = try acquire()
            holder.set(connection)
            defer { holder.clear() }
            let id = state.withLock { s -> UInt64 in
                s.nextRequest += 1
                return s.nextRequest
            }
            let outcome: Result<Data, EngineError>
            do {
                outcome = try connection.call(
                    id: id,
                    method: method,
                    params: params,
                    deadline: deadline,
                    onEvent: pump.map { pump in { @Sendable frame in pump.push(frame) } }
                )
            } catch {
                // Transport failure: this connection is finished; the caller learns why.
                connection.interrupt()
                throw error
            }
            release(connection)
            return outcome
        } catch let error as EngineError {
            return .failure(error)
        } catch is CancellationError {
            return .failure(EngineError(code: "cancelled", message: "the call was cancelled", retryable: false))
        } catch {
            return .failure(AttachedFailure.io("\(error)"))
        }
    }

    // MARK: Lifecycle

    func lifecycleStream() -> AsyncStream<EngineLifecycle> {
        let (stream, continuation) = AsyncStream<EngineLifecycle>.makeStream(bufferingPolicy: .unbounded)
        let id = UUID()
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

    private func publish(_ lifecycle: EngineLifecycle) {
        state.withLock { s in
            guard s.lifecycle != lifecycle else { return }
            s.lifecycle = lifecycle
            for continuation in s.subscribers.values { continuation.yield(lifecycle) }
        }
    }

    private var isStopped: Bool {
        state.withLock { $0.stopped }
    }

    /// Starts the watcher thread unless one runs or the client is detached.
    func ensureWatcher() {
        let start = state.withLock { s -> Bool in
            if s.stopped || s.watching { return false }
            s.watching = true
            return true
        }
        guard start else { return }
        publish(.booting)
        let thread = Thread { [self] in watch() }
        thread.name = "rightkit-attached-watch"
        thread.start()
    }

    /// Detaches for good: closes every connection, ends the watcher and the lifecycle streams.
    func stop() {
        let (idle, watch, subscribers) = state.withLock {
            s -> ([AttachedConnection], AttachedConnection?, [AsyncStream<EngineLifecycle>.Continuation]) in
            s.stopped = true
            let all = (s.idle, s.watchConnection, Array(s.subscribers.values))
            s.idle = []
            s.watchConnection = nil
            s.subscribers.removeAll()
            return all
        }
        for connection in idle { connection.interrupt() }
        watch?.interrupt()
        wakeup.signal()
        for subscriber in subscribers {
            subscriber.yield(.down)
            subscriber.finish()
        }
    }

    private func sleep(_ seconds: Double) {
        _ = wakeup.wait(timeout: .now() + seconds)
    }

    /// The watcher: connect, ask `doctor` until `ready`, then hold one idle connection open and
    /// block until the engine closes it (a close wakes it at once; no timer runs while it is up).
    private func watch() {
        let began = Date()
        var backoff = 0.25
        var wasReady = false
        defer { state.withLock { $0.watching = false } }
        while !isStopped {
            do {
                let connection = try openConnection()
                if try isReady(connection) {
                    state.withLock { $0.watchConnection = connection }
                    publish(.ready)
                    wasReady = true
                    backoff = 0.25
                    connection.waitUntilClosed()
                    state.withLock { $0.watchConnection = nil }
                    if isStopped { return }
                    if !recordExists {
                        publish(.down)
                        return
                    }
                    publish(.recovering)
                    continue
                }
                publish(wasReady ? .recovering : .booting)
            } catch let error as EngineError where error.code == "host_identity" || error.code == "unsupported_platform" {
                // Not retryable and not "starting": surface it, stop trying.
                publish(.down)
                return
            } catch {
                publish(wasReady ? .recovering : .booting)
            }
            if wasReady && !recordExists {
                publish(.down)
                return
            }
            if !wasReady && Date().timeIntervalSince(began) > bootBudget {
                publish(.down)
                return
            }
            sleep(backoff)
            backoff = min(backoff * 2, 2)
        }
    }

    /// `doctor` answers while the engine starts; its `ready` says whether commands are served.
    private func isReady(_ connection: AttachedConnection) throws -> Bool {
        let result = try connection.call(
            id: 1,
            method: "doctor",
            params: Data("{}".utf8),
            deadline: Date().addingTimeInterval(10),
            onEvent: nil
        )
        guard case .success(let data) = result,
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return false }
        return object["ready"] as? Bool == true
    }
}
