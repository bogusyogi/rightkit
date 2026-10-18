import Foundation

#if os(macOS)
import AppKit
import Darwin

/// A plain, `Sendable` snapshot of one running application, so lists can cross threads and be
/// filtered and tested without `NSRunningApplication`.
public struct RightKitRunningApp: Equatable, Sendable {
    public var pid: pid_t
    public var bundleIdentifier: String?
    public var name: String?
    public var bundlePath: String?
    /// True for an app that shows in the Dock (`NSApplication.ActivationPolicy.regular`).
    public var isRegular: Bool
    public var isFinishedLaunching: Bool

    public init(
        pid: pid_t, bundleIdentifier: String? = nil, name: String? = nil, bundlePath: String? = nil,
        isRegular: Bool = true, isFinishedLaunching: Bool = true,
    ) {
        self.pid = pid
        self.bundleIdentifier = bundleIdentifier
        self.name = name
        self.bundlePath = bundlePath
        self.isRegular = isRegular
        self.isFinishedLaunching = isFinishedLaunching
    }

    public init(_ app: NSRunningApplication) {
        self.init(
            pid: app.processIdentifier, bundleIdentifier: app.bundleIdentifier, name: app.localizedName,
            bundlePath: app.bundleURL?.path, isRegular: app.activationPolicy == .regular,
            isFinishedLaunching: app.isFinishedLaunching)
    }
}

/// A process read straight from the kernel process table.
public struct RightKitKernelProcess: Equatable, Sendable {
    public var pid: pid_t
    public var path: String
    public var start: Date
    public var parentPID: pid_t
}

/// One change in the set of running applications.
public enum RightKitProcessEvent: Equatable, Sendable {
    case launched(RightKitRunningApp)
    case terminated(RightKitRunningApp)
    case activated(RightKitRunningApp)
}

/// Process enumeration, observation, liveness and stopping. Everything here is read-only except
/// `stop(_:grace:)` and `terminate(pid:)`, which signal only the pids they are given.
public enum RightKitProcesses {
    // MARK: Applications

    /// Every running application, from `NSWorkspace`.
    public static func runningApps() -> [RightKitRunningApp] {
        NSWorkspace.shared.runningApplications.map(RightKitRunningApp.init)
    }

    /// Apps that show in the Dock, sorted by name, leaving out `excluding` (default: this process)
    /// and anything without a bundle. Pure over `apps`.
    public static func dockApps(
        from apps: [RightKitRunningApp], excluding pid: pid_t = ProcessInfo.processInfo.processIdentifier,
    ) -> [RightKitRunningApp] {
        apps.filter { $0.isRegular && $0.pid != pid && $0.bundlePath != nil }
            .sorted { ($0.name ?? "") < ($1.name ?? "") }
    }

    public static func dockApps() -> [RightKitRunningApp] { dockApps(from: runningApps()) }

    public static func isRunning(bundleIdentifier: String) -> Bool {
        !NSRunningApplication.runningApplications(withBundleIdentifier: bundleIdentifier)
            .allSatisfy { $0.isTerminated }
    }

    /// Asks an app to quit (a polite `terminate`, which may prompt to save). False when it is gone.
    @discardableResult
    public static func terminate(pid: pid_t) -> Bool {
        guard let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated else { return false }
        return app.terminate()
    }

    /// Launch, termination and activation of applications as an `AsyncStream`. Observation ends
    /// (and the notification observers are removed) when the stream is cancelled or dropped.
    public static func events() -> AsyncStream<RightKitProcessEvent> {
        AsyncStream { continuation in
            let center = NSWorkspace.shared.notificationCenter
            let bag = ObserverBag()
            func watch(_ name: Notification.Name, _ make: @escaping @Sendable (RightKitRunningApp) -> RightKitProcessEvent) {
                bag.add(center.addObserver(forName: name, object: nil, queue: nil) { note in
                    guard let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
                    else { return }
                    continuation.yield(make(RightKitRunningApp(app)))
                })
            }
            watch(NSWorkspace.didLaunchApplicationNotification) { .launched($0) }
            watch(NSWorkspace.didTerminateApplicationNotification) { .terminated($0) }
            watch(NSWorkspace.didActivateApplicationNotification) { .activated($0) }
            continuation.onTermination = { _ in bag.removeAll(from: center) }
        }
    }

    private final class ObserverBag: @unchecked Sendable {
        private let lock = NSLock()
        private var tokens: [NSObjectProtocol] = []
        func add(_ token: NSObjectProtocol) { lock.lock(); tokens.append(token); lock.unlock() }
        func removeAll(from center: NotificationCenter) {
            lock.lock(); let taken = tokens; tokens = []; lock.unlock()
            for token in taken { center.removeObserver(token) }
        }
    }

    // MARK: Kernel process table

    /// Every process whose executable path ends with `suffix`, read from the kernel (no Launch
    /// Services: a child started with `Process` is not reliably listed there). Processes whose path
    /// or start time cannot be read (other users', hardened) are skipped.
    public static func processes(withExecutableSuffix suffix: String) -> [RightKitKernelProcess] {
        let bytes = proc_listpids(UInt32(PROC_ALL_PIDS), 0, nil, 0)
        guard bytes > 0 else { return [] }
        var pids = [pid_t](repeating: 0, count: Int(bytes) / MemoryLayout<pid_t>.size + 64)
        let got = proc_listpids(UInt32(PROC_ALL_PIDS), 0, &pids, Int32(pids.count * MemoryLayout<pid_t>.size))
        guard got > 0 else { return [] }
        var found: [RightKitKernelProcess] = []
        for pid in pids.prefix(Int(got) / MemoryLayout<pid_t>.size) where pid > 0 {
            var buffer = [CChar](repeating: 0, count: 4096)
            guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { continue }
            let path = String(cString: buffer)
            guard path.hasSuffix(suffix), let info = bsdInfo(pid) else { continue }
            found.append(RightKitKernelProcess(
                pid: pid, path: path, start: startDate(info), parentPID: pid_t(info.pbi_ppid)))
        }
        return found
    }

    private static func bsdInfo(_ pid: pid_t) -> proc_bsdinfo? {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.size)
        guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else { return nil }
        return info
    }

    private static func startDate(_ info: proc_bsdinfo) -> Date {
        Date(timeIntervalSince1970: TimeInterval(info.pbi_start_tvsec) + TimeInterval(info.pbi_start_tvusec) / 1_000_000)
    }

    /// When `pid` started, or nil when it cannot be read.
    public static func startTime(pid: pid_t) -> Date? { bsdInfo(pid).map(startDate) }

    /// `kill(pid, 0)`: true for a live process, including one owned by someone else (EPERM).
    public static func exists(pid: pid_t) -> Bool {
        if kill(pid, 0) == 0 { return true }
        return errno == EPERM
    }

    /// Whether `actual` is within `tolerance` of `expected`: the test that tells a pid still
    /// running the process you recorded from a pid the system has since handed to something else.
    public static func isSameProcess(actualStart: Date, expectedStart: Date, tolerance: TimeInterval) -> Bool {
        abs(actualStart.timeIntervalSince(expectedStart)) < tolerance
    }

    /// Is `pid` alive, and is it still the same process? A recorded start time (when the session
    /// registered, seconds after the process began, hence the generous default tolerance) rules out a
    /// recycled pid. When either time is unknown the pid is trusted rather than hiding a real process.
    public static func isAlive(pid: pid_t, startedAt: Date?, tolerance: TimeInterval = 5 * 60) -> Bool {
        guard exists(pid: pid) else { return false }
        guard let startedAt, let actual = startTime(pid: pid) else { return true }
        return isSameProcess(actualStart: actual, expectedStart: startedAt, tolerance: tolerance)
    }

    /// The working directory of `pid`, or nil when it cannot be read.
    public static func currentDirectory(of pid: pid_t) -> String? {
        var info = proc_vnodepathinfo()
        let size = Int32(MemoryLayout<proc_vnodepathinfo>.size)
        guard proc_pidinfo(pid, PROC_PIDVNODEPATHINFO, 0, &info, size) == size else { return nil }
        let path = withUnsafePointer(to: info.pvi_cdir.vip_path) {
            $0.withMemoryRebound(to: CChar.self, capacity: Int(MAXPATHLEN)) { String(cString: $0) }
        }
        return path.isEmpty ? nil : path
    }

    /// SIGTERM to each pid, up to `grace` seconds for them to exit, then SIGKILL to the rest.
    /// Blocking: call off the main thread. Returns how many needed SIGKILL.
    @discardableResult
    public static func stop(_ pids: [pid_t], grace: TimeInterval = 3) -> Int {
        for pid in pids { kill(pid, SIGTERM) }
        let deadline = Date().addingTimeInterval(grace)
        while Date() < deadline, pids.contains(where: exists(pid:)) {
            Thread.sleep(forTimeInterval: 0.05)
        }
        let remaining = pids.filter(exists(pid:))
        for pid in remaining { kill(pid, SIGKILL) }
        return remaining.count
    }
}
#endif
