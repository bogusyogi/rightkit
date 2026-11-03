import Foundation

#if os(macOS)
import Darwin

/// How often and how fast a crashing child is restarted.
public struct RightKitRestartPolicy: Equatable, Sendable {
    /// Delay before the nth restart in a row (the last entry repeats).
    public var backoff: [TimeInterval]
    /// Exits within `window` seconds that latch the child off until `retry()`.
    public var latchExits: Int
    public var window: TimeInterval

    public init(backoff: [TimeInterval] = [1, 2, 4, 8, 16], latchExits: Int = 5, window: TimeInterval = 60) {
        self.backoff = backoff
        self.latchExits = latchExits
        self.window = window
    }
}

/// Restart bookkeeping as a value, with no process and no clock of its own, so it is testable.
public struct RightKitRestartState: Equatable, Sendable {
    public enum Outcome: Equatable, Sendable {
        case respawnScheduled(afterSeconds: Int)
        case latched(reason: String)
        /// Already latched (for example an explicit open): recorded, nothing scheduled.
        case alreadyLatched
    }

    public private(set) var exits: [Date] = []
    public private(set) var nextLaunch = Date.distantPast
    public private(set) var latchReason: String?

    public init() {}

    public func canLaunch(now: Date) -> Bool { latchReason == nil && now >= nextLaunch }

    /// Records an unplanned exit and decides what follows.
    public mutating func recordExit(now: Date, status: Int32, policy: RightKitRestartPolicy) -> Outcome {
        exits = exits.filter { now.timeIntervalSince($0) < policy.window } + [now]
        if latchReason != nil { return .alreadyLatched }
        if exits.count >= policy.latchExits {
            let reason = "exited \(exits.count) times in \(Int(policy.window)) s (last status \(status))"
            latchReason = reason
            return .latched(reason: reason)
        }
        let delay = policy.backoff.isEmpty ? 0 : policy.backoff[min(exits.count, policy.backoff.count) - 1]
        nextLaunch = now.addingTimeInterval(delay)
        return .respawnScheduled(afterSeconds: Int(delay))
    }

    /// Clears the latch and the exit history, so the next check may start the child at once.
    public mutating func retry() {
        latchReason = nil
        exits = []
        nextLaunch = .distantPast
    }
}

/// A child process kept alive for as long as its owner runs: started now and again every tick if
/// gone, with backoff between restarts and a latch after repeated crashes, stderr appended to a
/// rotated log, lifecycle records as JSON lines. Main-actor isolated; file work runs off it.
///
/// A missing executable is `.notInstalled` with no retries. A pid stopped on purpose is not a
/// failure. The owner calls `terminate()` when it quits.
///
/// Host seams, all optional and all called on the main actor: `prepareProcess` receives each
/// configured `Process` just before it runs (set standard input, arguments, environment);
/// `onSpawned` and `onExit` bracket the child's life so the host can keep and close its pipe;
/// `canStart` is a start gate consulted before every launch; `externalRunning` lets `status` report
/// a copy of the program that something else started. `start(arguments:ignoringBackoff:)` spawns
/// now with its own arguments, and `stop(grace:)` stops the child without ending supervision.
@MainActor
public final class RightKitSupervisedChild {
    public struct Configuration: Sendable {
        /// Names the log files: `<name>-stderr.log` and `<name>-respawns.jsonl`.
        public var name: String
        public var executable: URL
        public var arguments: [String]
        /// nil inherits this process's environment.
        public var environment: [String: String]?
        /// nil disables logs (a test host has no real state folder); stderr is then discarded.
        public var logsDirectory: URL?
        public var policy: RightKitRestartPolicy
        public var tick: TimeInterval
        public var qualityOfService: QualityOfService

        public init(
            name: String, executable: URL, arguments: [String] = [], environment: [String: String]? = nil,
            logsDirectory: URL? = nil, policy: RightKitRestartPolicy = RightKitRestartPolicy(),
            tick: TimeInterval = 5, qualityOfService: QualityOfService = .utility,
        ) {
            self.name = name
            self.executable = executable
            self.arguments = arguments
            self.environment = environment
            self.logsDirectory = logsDirectory
            self.policy = policy
            self.tick = tick
            self.qualityOfService = qualityOfService
        }
    }

    public enum Status: Equatable, Sendable {
        case notInstalled
        case starting
        case running
        case restarting
        case latched(String)

        /// `running`, `starting`, `restarting`, `latched: <reason>` or `not installed`.
        public var text: String {
            switch self {
            case .notInstalled: "not installed"
            case .starting: "starting"
            case .running: "running"
            case .restarting: "restarting"
            case .latched(let reason): "latched: \(reason)"
            }
        }
    }

    public let configuration: Configuration
    /// Called when `status` may have changed.
    public var onChange: (@MainActor () -> Void)?
    /// Called with every configured `Process` just before `run()`, for the per-spawn standard input
    /// (a `Pipe` the host keeps the write end of), arguments and environment. The default standard
    /// input is the null device, and standard output and error are already set.
    public var prepareProcess: (@MainActor (Process) -> Void)?
    /// Called right after a spawn succeeded, with the running process.
    public var onSpawned: (@MainActor (Process) -> Void)?
    /// Called when a spawned child has exited: the process, its termination status, whether a signal
    /// ended it, and `planned` (true when it was stopped through `stop(grace:)` or `terminate(grace:)`,
    /// so the exit is not counted as a failure). Not called for a spawn that failed to start.
    public var onExit: (@MainActor (Process, Int32, Bool, Bool) -> Void)?
    /// Consulted before each launch, the timer's and `start(arguments:ignoringBackoff:)`'s; a launch
    /// is deferred (and tried again on the next tick) while it returns false.
    public var canStart: (@MainActor () -> Bool)?
    /// Whether a copy of the program that this object did not spawn is running (for example one
    /// Launch Services started). Only `status` reads it: a true value reports `.running`.
    public var externalRunning: (@MainActor () -> Bool)?

    private var child: Process?
    private var spawned: [pid_t: Process] = [:]
    private var timer: Timer?
    private var quitting = false
    private var spawning = false
    private var installed = true
    private var restart = RightKitRestartState()
    private var lastStatus: Status?
    private var intentionalStops: Set<pid_t> = []

    public init(_ configuration: Configuration) { self.configuration = configuration }

    public var isRunning: Bool { child?.isRunning == true }

    public var status: Status {
        if !installed { return .notInstalled }
        if let reason = restart.latchReason { return .latched(reason) }
        if isRunning || externalRunning?() == true { return .running }
        return restart.exits.isEmpty ? .starting : .restarting
    }

    /// Starts the child now and keeps it alive until `terminate()`.
    public func supervise() {
        guard timer == nil, !quitting else { return }
        ensureRunning()
        let timer = Timer(timeInterval: configuration.tick, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.ensureRunning() }
        }
        timer.tolerance = min(1, configuration.tick / 5)
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    /// Clears the latch and the exit history and starts the child at once.
    public func retry() {
        restart.retry()
        ensureRunning()
    }

    /// Spawns the child now, with `arguments` in place of `Configuration.arguments` for this spawn
    /// (nil keeps the configured ones). With `ignoringBackoff` a pending backoff and a crash latch do
    /// not hold the launch back (a user's explicit open), and a failure of this child is still
    /// counted like any other exit. Does nothing, and returns false, when the owner is quitting, a
    /// child is already running or starting, the executable is missing, or `canStart` says no.
    /// Does not begin supervision: call `supervise()` for that.
    @discardableResult
    public func start(arguments: [String]? = nil, ignoringBackoff: Bool = true) -> Bool {
        launch(arguments: arguments, ignoringBackoff: ignoringBackoff)
    }

    /// Stops the child (SIGTERM, up to `grace` seconds, SIGKILL) and keeps supervising: the exit is
    /// planned, so it is not a failure, and the timer starts the child again on a later tick unless
    /// `canStart` holds it off. Blocks the caller for at most `grace`. No-op when nothing runs.
    public func stop(grace: TimeInterval = 3) {
        guard let process = child, process.isRunning else { return }
        let pid = process.processIdentifier
        intentionalStops.insert(pid)
        RightKitProcesses.stop([pid], grace: grace)
    }

    /// Stops supervising and stops the child: SIGTERM, up to `grace` seconds, SIGKILL. Blocks the
    /// caller for at most `grace`; meant for application quit.
    public func terminate(grace: TimeInterval = 3) {
        quitting = true
        timer?.invalidate()
        timer = nil
        guard let process = child, process.isRunning else { return }
        let pid = process.processIdentifier
        intentionalStops.insert(pid)
        RightKitProcesses.stop([pid], grace: grace)
    }

    private func publish() {
        let now = status
        guard now != lastStatus else { return }
        lastStatus = now
        onChange?()
    }

    private func ensureRunning() {
        launch(arguments: nil, ignoringBackoff: false)
    }

    @discardableResult
    private func launch(arguments: [String]?, ignoringBackoff: Bool) -> Bool {
        defer { publish() }
        guard !quitting, !spawning, !isRunning else { return false }
        guard ignoringBackoff || restart.canLaunch(now: Date()) else { return false }
        guard canStart?() ?? true else { return false }
        guard FileManager.default.isExecutableFile(atPath: configuration.executable.path) else {
            installed = false
            return false
        }
        installed = true
        spawning = true
        let configuration = configuration
        Task.detached(priority: .userInitiated) { [weak self] in
            let handle = configuration.logsDirectory.flatMap {
                RightKitChildLog.openForAppend($0.appendingPathComponent("\(configuration.name)-stderr.log"))
            }
            await self?.finishStart(stderr: handle, arguments: arguments)
        }
        return true
    }

    private func finishStart(stderr: FileHandle?, arguments: [String]?) {
        spawning = false
        // The owner started quitting, or something else started it while the log was opening.
        guard !quitting, !isRunning else { try? stderr?.close(); return }
        let process = Process()
        process.executableURL = configuration.executable
        process.arguments = arguments ?? configuration.arguments
        if let environment = configuration.environment { process.environment = environment }
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = stderr ?? FileHandle.nullDevice
        process.qualityOfService = configuration.qualityOfService
        prepareProcess?(process)
        process.terminationHandler = { [weak self] finished in
            let code = finished.terminationStatus
            let signaled = finished.terminationReason == .uncaughtSignal
            let pid = finished.processIdentifier
            Task { @MainActor in self?.handleExit(pid: pid, code: code, signaled: signaled) }
        }
        do {
            try process.run()
            try? stderr?.close()   // the child holds its own copy
            child = process
            spawned[process.processIdentifier] = process
            record("spawned", [
                "pid": Int(process.processIdentifier), "exits_in_window": restart.exits.count,
                "log": stderr == nil ? 0 : 1, "respawn": restart.exits.isEmpty ? 0 : 1,
            ])
            onSpawned?(process)
        } catch {
            try? stderr?.close()
            recordFailure(code: -1, signaled: false, pid: 0)
        }
        publish()
    }

    private func handleExit(pid: pid_t, code: Int32, signaled: Bool) {
        if child?.processIdentifier == pid { child = nil }
        let planned = intentionalStops.remove(pid) != nil
        if let process = spawned.removeValue(forKey: pid) { onExit?(process, code, signaled, planned || quitting) }
        if planned || quitting { publish(); return }
        recordFailure(code: code, signaled: signaled, pid: pid)
    }

    private func recordFailure(code: Int32, signaled: Bool, pid: pid_t) {
        guard !quitting else { return }
        let outcome = restart.recordExit(now: Date(), status: code, policy: configuration.policy)
        var event = "exited"
        var delay = 0
        switch outcome {
        case .latched: event = "latched"
        case .respawnScheduled(let seconds): delay = seconds
        case .alreadyLatched: break
        }
        record(event, [
            "pid": Int(pid), "status": Int(code), "signaled": signaled ? 1 : 0,
            "exits_in_window": restart.exits.count, "backoff_s": delay,
        ])
        publish()
    }

    private func record(_ event: String, _ fields: [String: Int]) {
        guard let logs = configuration.logsDirectory else { return }
        RightKitChildLog.appendRecord(
            "\(configuration.name).\(event)", fields: fields,
            to: logs.appendingPathComponent("\(configuration.name)-respawns.jsonl"))
    }
}
#endif
