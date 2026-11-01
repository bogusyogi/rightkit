#if os(macOS)
import AppKit
import ApplicationServices
import RightKitAccessibility

/// the app: the green zoom button fills the screen's visible frame instead of
/// entering a full-screen Space. Option-click keeps macOS's own behaviour, and
/// so do windows already full screen or that cannot be resized.
///
/// A second click on a window that is still where the button put it restores
/// where it was. The frames and the move come from the window-management
/// engine (`Windows/WindowActions.swift`), so Restore and the shortcuts see the
/// same memory.
///
/// The press must be judged on the event tap's thread (it may be swallowed),
/// and every left press on the Mac passes through it, so the judgement is
/// cheap first (#436): a cached list of on-screen windows, refreshed off the
/// tap, says whether the press is in the traffic-light corner of the topmost
/// normal window. Only then is Accessibility asked, through an element with a
/// short timeout of its own; a timeout, an overlay, or any doubt passes.
final class WindowMaximizer {
    private struct OnScreenWindow {
        let pid: pid_t
        let layer: Int
        let bounds: CGRect
    }

    /// The traffic lights of a standard titled window sit inside this corner.
    private static let buttonCorner = CGSize(width: 80, height: 30)
    /// Per Accessibility message, and for the whole check, on the tap's thread.
    private static let messageTimeout: Float = 0.05
    private static let checkBudget: UInt64 = 120_000_000
    /// The refresh timer stops after this long without a left click.
    private static let idleStop: TimeInterval = 60
    private static let captureBundles: Set<String> = ["com.apple.screencaptureui", "com.apple.screencapture"]
    private static let captureOwners: Set<String> = ["screencaptureui", "screencapture"]

    private let lock = NSLock()
    private var swallowingMouseUp = false
    /// Front to back, from `CGWindowListCopyWindowInfo`; written only by `refreshQueue`.
    private var windows: [OnScreenWindow] = []
    /// A screenshot overlay is on screen or in front: every press passes.
    private var captureUp = false
    private var lastClick: TimeInterval = 0
    private var timer: DispatchSourceTimer?
    private var activation: NSObjectProtocol?
    private var refreshQueued = false
    private let refreshQueue = DispatchQueue(label: "dev.orthic.pulse.maximizer.windows", qos: .userInitiated)

    func handle(_ type: CGEventType, _ event: CGEvent) -> TapDecision {
        switch type {
        case .leftMouseDown:
            keepFresh()
            guard !event.flags.contains(.maskAlternate),
                  let window = zoomButtonWindow(at: event.location)
            else { return .pass }
            lock.lock(); swallowingMouseUp = true; lock.unlock()
            DispatchQueue.main.async { [weak self] in self?.toggle(window) }
            return .swallow
        case .leftMouseUp:
            // A click often moves or opens windows: read them again once it lands.
            scheduleRefresh()
            lock.lock(); defer { lock.unlock() }
            guard swallowingMouseUp else { return .pass }
            swallowingMouseUp = false
            return .swallow
        default:
            return .pass
        }
    }

    func reset() {
        lock.lock()
        swallowingMouseUp = false
        let stopped = timer, observer = activation
        timer = nil; activation = nil
        windows = []; captureUp = false
        lock.unlock()
        stopped?.cancel()
        if let observer { NSWorkspace.shared.notificationCenter.removeObserver(observer) }
    }

    /// The window whose zoom button is under the pointer, when this should
    /// take over the click. Runs on the tap's thread: nil unless the cache puts
    /// the press in the button corner of the topmost normal window, and nil once
    /// the Accessibility check runs past its budget.
    private func zoomButtonWindow(at location: CGPoint) -> RightKitAXElement? {
        lock.lock(); let list = windows, capture = captureUp; lock.unlock()
        guard !capture,
              let top = list.first(where: { $0.bounds.contains(location) }),
              top.layer == 0,
              top.pid != ProcessInfo.processInfo.processIdentifier,
              CGRect(origin: top.bounds.origin, size: Self.buttonCorner).contains(location)
        else { return nil }

        let started = DispatchTime.now().uptimeNanoseconds
        let inBudget = { DispatchTime.now().uptimeNanoseconds - started < Self.checkBudget }
        // RightKit's hit test sets its timeout on a system-wide element, which is global
        // (rightkit#189): the element under the pointer is a raw AX read on the owner app.
        guard let found = AX.elementAt(location, in: top.pid, timeout: Self.messageTimeout) else { return nil }
        let hit = RightKitAXElement(found)
        hit.setMessagingTimeout(Self.messageTimeout)
        guard hit.string("AXRole") == "AXButton",
              hit.string("AXSubrole") == "AXZoomButton", inBudget(),
              let window = hit.element("AXParent") ?? hit.element("AXWindow")
        else { return nil }
        window.setMessagingTimeout(Self.messageTimeout)
        guard window.string("AXRole") == "AXWindow", window.pid == top.pid, inBudget() else { return nil }
        guard window.isFullScreen != true, inBudget() else { return nil }
        guard AX.isSettable(window, "AXSize"), AX.isSettable(window, "AXPosition"), inBudget() else { return nil }
        return window
    }

    // MARK: - The window cache (off the tap's thread)

    /// On the tap's thread: notes the click and starts the refresh timer and the
    /// app-activation observer when they are not running. Cheap; no reads.
    private func keepFresh() {
        let now = ProcessInfo.processInfo.systemUptime
        lock.lock()
        lastClick = now
        let start = timer == nil
        var fresh: DispatchSourceTimer?
        if start {
            let source = DispatchSource.makeTimerSource(queue: refreshQueue)
            source.schedule(deadline: .now(), repeating: 0.5, leeway: .milliseconds(100))
            source.setEventHandler { [weak self] in self?.tick() }
            timer = source
            fresh = source
        }
        lock.unlock()
        guard let fresh else { return }
        fresh.resume()
        let observer = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: nil
        ) { [weak self] _ in self?.scheduleRefresh() }
        lock.lock()
        if timer === fresh, activation == nil {
            activation = observer
            lock.unlock()
        } else {
            lock.unlock()
            NSWorkspace.shared.notificationCenter.removeObserver(observer)
        }
    }

    private func scheduleRefresh() {
        lock.lock()
        let due = !refreshQueued && timer != nil
        if due { refreshQueued = true }
        lock.unlock()
        guard due else { return }
        refreshQueue.async { [weak self] in self?.refresh() }
    }

    /// On `refreshQueue`: refreshes, or stops after a minute without a click.
    private func tick() {
        lock.lock()
        let idle = ProcessInfo.processInfo.systemUptime - lastClick > Self.idleStop
        let stopped = idle ? timer : nil
        let observer = idle ? activation : nil
        if idle { timer = nil; activation = nil }
        lock.unlock()
        guard idle else { refresh(); return }
        stopped?.cancel()
        if let observer { NSWorkspace.shared.notificationCenter.removeObserver(observer) }
    }

    /// On `refreshQueue`: the on-screen windows, front to back, and whether a
    /// screenshot overlay is up. Windows that are fully transparent are left
    /// out, so an invisible overlay never hides the window under it.
    private func refresh() {
        lock.lock(); refreshQueued = false; lock.unlock()
        let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
            as? [[String: Any]] ?? []
        var list: [OnScreenWindow] = []
        var capture = false
        for entry in info {
            guard let pid = (entry[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value,
                  let raw = entry[kCGWindowBounds as String] as? NSDictionary,
                  let bounds = CGRect(dictionaryRepresentation: raw as CFDictionary)
            else { continue }
            if let owner = entry[kCGWindowOwnerName as String] as? String, Self.captureOwners.contains(owner) {
                capture = true
            }
            if ((entry[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 1) <= 0 { continue }
            let layer = (entry[kCGWindowLayer as String] as? NSNumber)?.intValue ?? 0
            list.append(OnScreenWindow(pid: pid, layer: layer, bounds: bounds))
        }
        if let front = NSWorkspace.shared.frontmostApplication?.bundleIdentifier,
           Self.captureBundles.contains(front) {
            capture = true
        }
        lock.lock()
        if timer != nil { windows = list; captureUp = capture }
        lock.unlock()
    }

    private func toggle(_ window: RightKitAXElement) {
        WindowActions.toggleMaximize(window)
    }
}
#endif
