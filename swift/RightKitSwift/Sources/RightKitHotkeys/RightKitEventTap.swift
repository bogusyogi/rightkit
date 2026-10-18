import Foundation

#if os(macOS)
import ApplicationServices
import CoreGraphics

/// What a tap handler decides for one event: let it through, or remove it from the event stream.
public enum RightKitTapDecision: Sendable, Equatable {
    case pass
    case swallow
}

/// Called on the tap's own thread. Decide quickly and hop off it for anything that takes time: a
/// slow handler stalls every keystroke and click until macOS disables the tap.
public typealias RightKitTapHandler = @Sendable (CGEventType, CGEvent) -> RightKitTapDecision

/// Identifies one registration so it can be removed again.
public struct RightKitEventTapToken: Hashable, Sendable {
    fileprivate let id = UUID()
    fileprivate init() {}
}

/// Ordered handlers for one event tap. Highest priority runs first; handlers with equal priority run
/// in registration order; the first to swallow wins and later handlers do not see the event.
/// Pure value logic: it holds no tap and needs no permission, so it is unit-testable.
public struct RightKitTapChain: Sendable {
    fileprivate struct Entry: Sendable {
        let token: RightKitEventTapToken
        let priority: Int
        let sequence: Int
        let handler: RightKitTapHandler
    }

    private var entries: [Entry] = []
    private var nextSequence = 0

    public init() {}

    public var count: Int { entries.count }

    @discardableResult
    public mutating func add(priority: Int, handler: @escaping RightKitTapHandler) -> RightKitEventTapToken {
        let token = RightKitEventTapToken()
        entries.append(Entry(token: token, priority: priority, sequence: nextSequence, handler: handler))
        nextSequence += 1
        entries.sort { lhs, rhs in
            lhs.priority != rhs.priority ? lhs.priority > rhs.priority : lhs.sequence < rhs.sequence
        }
        return token
    }

    public mutating func remove(_ token: RightKitEventTapToken) {
        entries.removeAll { $0.token == token }
    }

    public func decide(_ type: CGEventType, _ event: CGEvent) -> RightKitTapDecision {
        for entry in entries where entry.handler(type, event) == .swallow {
            return .swallow
        }
        return .pass
    }
}

/// Rate limit for the "macOS turned the tap off" notice: at most one per `interval` seconds, so a tap
/// that keeps being disabled can be seen in a log without flooding it.
public struct RightKitTapDisableThrottle: Sendable, Equatable {
    public var interval: TimeInterval
    private var last: TimeInterval?

    public init(interval: TimeInterval = 60) { self.interval = interval }

    /// `now` is a monotonic clock reading in seconds (for example `ProcessInfo.systemUptime`).
    public mutating func shouldNote(now: TimeInterval) -> Bool {
        if let last, now - last < interval { return false }
        last = now
        return true
    }
}

public enum RightKitEventTapMask {
    /// The `eventsOfInterest` mask for `events` (one bit per event type).
    public static func mask(for events: [CGEventType]) -> CGEventMask {
        var mask = CGEventMask(0)
        for event in events { mask |= CGEventMask(1) << CGEventMask(event.rawValue) }
        return mask
    }
}

/// One session event tap shared by every feature of an app. Features register ordered handlers
/// (highest priority first, first swallow wins). The tap lives on a thread of its own so the main
/// thread never sits in the event path, and `stop()` returns only once that thread has finished.
///
/// Creating a tap needs Accessibility or Input Monitoring. `start()` never prompts: it returns
/// `false` when macOS refuses, so the host decides whether and how to ask. A tap macOS disables
/// (a handler ran too long, or user input stopped it) is switched back on.
public final class RightKitEventTapHub: @unchecked Sendable {
    /// Key and left-button events, the Pulse default.
    public static var defaultEvents: [CGEventType] { [.keyDown, .keyUp, .leftMouseDown, .leftMouseUp] }

    private struct PortBox: @unchecked Sendable { let port: CFMachPort }

    private let location: CGEventTapLocation
    private let events: [CGEventType]
    private let options: CGEventTapOptions
    private let onTapDisabled: (@Sendable (_ byTimeout: Bool) -> Void)?

    private let lock = NSLock()
    private var chain = RightKitTapChain()
    private var tap: CFMachPort?
    private var runLoop: CFRunLoop?
    private var finished: DispatchSemaphore?
    private var throttle = RightKitTapDisableThrottle()

    /// - Parameters:
    ///   - listenOnly: a passive tap that can observe but never swallow; it needs only Input
    ///     Monitoring. The default active tap can swallow and needs Accessibility.
    ///   - onTapDisabled: called (at most once a minute) when macOS turned the tap off and it was
    ///     switched back on; `byTimeout` is true when a handler was too slow.
    public init(
        location: CGEventTapLocation = .cgSessionEventTap,
        events: [CGEventType] = RightKitEventTapHub.defaultEvents,
        listenOnly: Bool = false,
        onTapDisabled: (@Sendable (_ byTimeout: Bool) -> Void)? = nil,
    ) {
        self.location = location
        self.events = events
        self.options = listenOnly ? .listenOnly : .defaultTap
        self.onTapDisabled = onTapDisabled
    }

    deinit { stop() }

    public var isRunning: Bool {
        lock.lock(); defer { lock.unlock() }
        return tap != nil
    }

    @discardableResult
    public func register(priority: Int, handler: @escaping RightKitTapHandler) -> RightKitEventTapToken {
        lock.lock(); defer { lock.unlock() }
        return chain.add(priority: priority, handler: handler)
    }

    public func unregister(_ token: RightKitEventTapToken) {
        lock.lock(); defer { lock.unlock() }
        chain.remove(token)
    }

    /// False when macOS refuses the tap (permission not granted). Never prompts.
    @discardableResult
    public func start() -> Bool {
        if isRunning { return true }
        let callback: CGEventTapCallBack = { _, type, event, info in
            guard let info else { return Unmanaged.passUnretained(event) }
            return Unmanaged<RightKitEventTapHub>.fromOpaque(info).takeUnretainedValue().dispatch(type, event)
        }
        guard let port = CGEvent.tapCreate(
            tap: location, place: .headInsertEventTap, options: options,
            eventsOfInterest: RightKitEventTapMask.mask(for: events), callback: callback,
            userInfo: Unmanaged.passUnretained(self).toOpaque())
        else { return false }

        let ready = DispatchSemaphore(value: 0)
        let done = DispatchSemaphore(value: 0)
        lock.lock(); tap = port; finished = done; lock.unlock()
        let box = PortBox(port: port)
        let thread = Thread { [weak self] in
            guard let self else { ready.signal(); done.signal(); return }
            let loop = CFRunLoopGetCurrent()
            let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, box.port, 0)
            self.lock.lock(); self.runLoop = loop; self.lock.unlock()
            CFRunLoopAddSource(loop, source, .commonModes)
            CGEvent.tapEnable(tap: box.port, enable: true)
            ready.signal()
            CFRunLoopRun()
            CFRunLoopRemoveSource(loop, source, .commonModes)
            done.signal()
        }
        thread.name = "dev.orthic.rightkit.eventtap"
        thread.qualityOfService = .userInteractive
        thread.start()
        ready.wait()
        return true
    }

    /// Disables the tap, stops its thread and waits (up to two seconds) for a callback in flight.
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
            lock.lock()
            let port = tap
            let note = throttle.shouldNote(now: ProcessInfo.processInfo.systemUptime)
            lock.unlock()
            if let port { CGEvent.tapEnable(tap: port, enable: true) }
            if note { onTapDisabled?(type == .tapDisabledByTimeout) }
            return Unmanaged.passUnretained(event)
        }
        lock.lock(); let current = chain; lock.unlock()
        // A passive tap's return value is ignored by macOS, but never claim to swallow.
        if options == .listenOnly {
            _ = current.decide(type, event)
            return Unmanaged.passUnretained(event)
        }
        return current.decide(type, event) == .swallow ? nil : Unmanaged.passUnretained(event)
    }
}
#endif
