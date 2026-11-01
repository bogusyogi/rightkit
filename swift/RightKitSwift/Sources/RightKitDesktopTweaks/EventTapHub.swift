#if os(macOS)
import ApplicationServices
import CoreGraphics
import Foundation

public enum TapDecision { case pass, swallow }

/// Called on the tap's own thread. Decide quickly and hop off it for anything
/// that takes time: a slow handler stalls every keystroke and click.
typealias TapHandler = (CGEventType, CGEvent) -> TapDecision

public struct EventTapToken: Hashable { fileprivate let id = UUID() }

/// Pulse: the one session event tap behind the Mac conveniences. Features
/// register ordered handlers (highest priority first; the first to swallow
/// wins). The tap lives on a thread of its own so the main thread never sits
/// in the event path, and `stop()` returns only once that thread has finished.
public final class EventTapHub {
    private struct Entry {
        let token: EventTapToken
        let priority: Int
        let handler: TapHandler
    }

    private let location: CGEventTapLocation
    private let events: [CGEventType]

    public init(location: CGEventTapLocation = .cgSessionEventTap,
         events: [CGEventType] = [.keyDown, .keyUp, .leftMouseDown, .leftMouseUp]) {
        self.location = location
        self.events = events
    }

    private let lock = NSLock()
    private var entries: [Entry] = []
    private var tap: CFMachPort?
    private var runLoop: CFRunLoop?
    private var finished: DispatchSemaphore?
    private var lastDisabledNote: TimeInterval?
    /// Callback time over the current minute (#436): logged when the slowest passes 5 ms.
    private var latencyEvents = 0
    private var latencyTotalNs: UInt64 = 0
    private var latencyMaxNs: UInt64 = 0
    private var latencySince: UInt64 = 0

    public var isRunning: Bool {
        lock.lock(); defer { lock.unlock() }
        return tap != nil
    }

    public func register(priority: Int, handler: @escaping TapHandler) -> EventTapToken {
        let token = EventTapToken()
        lock.lock()
        entries.append(Entry(token: token, priority: priority, handler: handler))
        entries.sort { $0.priority > $1.priority }
        lock.unlock()
        return token
    }

    public func unregister(_ token: EventTapToken) {
        lock.lock()
        entries.removeAll { $0.token == token }
        lock.unlock()
    }

    /// False when macOS refuses the tap (Accessibility or Input Monitoring not
    /// granted). Never prompts.
    @discardableResult
    public func start() -> Bool {
        if isRunning { return true }
        var mask = CGEventMask(0)
        for event in events { mask |= CGEventMask(1) << CGEventMask(event.rawValue) }
        let callback: CGEventTapCallBack = { _, type, event, info in
            guard let info else { return Unmanaged.passUnretained(event) }
            return Unmanaged<EventTapHub>.fromOpaque(info).takeUnretainedValue().dispatch(type, event)
        }
        guard let port = CGEvent.tapCreate(
            tap: location, place: .headInsertEventTap, options: .defaultTap,
            eventsOfInterest: mask, callback: callback,
            userInfo: Unmanaged.passUnretained(self).toOpaque())
        else { return false }

        let ready = DispatchSemaphore(value: 0)
        let done = DispatchSemaphore(value: 0)
        lock.lock(); tap = port; finished = done; lock.unlock()
        let thread = Thread { [weak self] in
            guard let self else { ready.signal(); done.signal(); return }
            let loop = CFRunLoopGetCurrent()
            let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, port, 0)
            self.lock.lock(); self.runLoop = loop; self.lock.unlock()
            CFRunLoopAddSource(loop, source, .commonModes)
            CGEvent.tapEnable(tap: port, enable: true)
            ready.signal()
            CFRunLoopRun()
            CFRunLoopRemoveSource(loop, source, .commonModes)
            done.signal()
        }
        thread.name = "dev.orthic.pulse.eventtap"
        thread.qualityOfService = .userInteractive
        thread.start()
        ready.wait()
        return true
    }

    /// Disables the tap, stops its thread and waits for any callback in flight.
    public func stop() {
        lock.lock()
        let port = tap, loop = runLoop, done = finished
        tap = nil; runLoop = nil; finished = nil
        lock.unlock()
        guard let port else { return }
        CGEvent.tapEnable(tap: port, enable: false)
        if let loop { CFRunLoopStop(loop) }
        _ = done?.wait(timeout: .now() + 2)
        CFMachPortInvalidate(port)
    }

    fileprivate func dispatch(_ type: CGEventType, _ event: CGEvent) -> Unmanaged<CGEvent>? {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            lock.lock(); let port = tap
            // macOS turned the tap off (a handler ran too long, or the user's input
            // stopped it): it is turned back on, and said once a minute at most in the
            // durable log so a tap that keeps being disabled can be seen.
            let now = ProcessInfo.processInfo.systemUptime
            let note = lastDisabledNote.map { now - $0 >= 60 } ?? true
            if note { lastDisabledNote = now }
            lock.unlock()
            if let port { CGEvent.tapEnable(tap: port, enable: true) }
            if note {
                Log.record(.usage, "eventtap.reenabled",
                           counts: ["byTimeout": type == .tapDisabledByTimeout ? 1 : 0])
            }
            return Unmanaged.passUnretained(event)
        }
        let started = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
        lock.lock(); let current = entries; lock.unlock()
        var swallowed = false
        for entry in current where entry.handler(type, event) == .swallow {
            swallowed = true
            break
        }
        noteLatency(since: started)
        return swallowed ? nil : Unmanaged.passUnretained(event)
    }

    /// Adds one callback's time; once a minute, says the slowest and the average
    /// in the durable log when the slowest took over 5 ms, so a stalled tap shows.
    private func noteLatency(since started: UInt64) {
        let now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW)
        let spent = now &- started
        lock.lock()
        if latencySince == 0 { latencySince = now }
        latencyEvents += 1
        latencyTotalNs &+= spent
        latencyMaxNs = max(latencyMaxNs, spent)
        var report: [String: Int]?
        if now &- latencySince >= 60_000_000_000 {
            if latencyMaxNs > 5_000_000 {
                report = ["maxMicros": Int(latencyMaxNs / 1_000),
                          "averageMicros": Int(latencyTotalNs / UInt64(latencyEvents) / 1_000),
                          "events": latencyEvents,
                          "hid": location == .cghidEventTap ? 1 : 0]
            }
            latencyEvents = 0; latencyTotalNs = 0; latencyMaxNs = 0; latencySince = now
        }
        lock.unlock()
        if let report { Log.record(.usage, "eventtap.slow", counts: report) }
    }
}
#endif
