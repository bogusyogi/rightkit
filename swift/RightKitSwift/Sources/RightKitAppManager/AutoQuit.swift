#if os(macOS)
import AppKit
import ApplicationServices
import RightKitAccessibility

/// Pulse: quits apps the person opted in once their last window closes.
///
/// A window closing (an Accessibility notification), then a short debounce,
/// then every check below must pass or nothing happens:
/// - the app lists its windows through Accessibility and the list is empty;
/// - the system's window list, which includes minimized windows and windows on
///   other Spaces, has no normal window for that process either;
/// - the app is an ordinary foreground app that has finished launching.
/// Anything unknown refuses. The quit is `terminate()`, the polite request
/// the app can still answer with a save prompt, never a forced kill.
@MainActor
final class AutoQuit {
    private static let debounce: TimeInterval = 1.5
    private static let neverQuit: Set<String> = ["com.apple.finder", "com.apple.dock", "dev.orthic.pulse"]

    private var bundleIDs: Set<String> = []
    private var observers: [pid_t: RightKitAXObserver] = [:]
    private var listeners: [pid_t: Task<Void, Never>] = [:]
    private var pending: [pid_t: DispatchWorkItem] = [:]
    private var workspaceTokens: [NSObjectProtocol] = []

    var isRunning: Bool { !workspaceTokens.isEmpty }

    func start(bundleIDs: [String]) {
        self.bundleIDs = Set(bundleIDs).subtracting(Self.neverQuit)
        if workspaceTokens.isEmpty {
            let center = NSWorkspace.shared.notificationCenter
            workspaceTokens.append(center.addObserver(
                forName: NSWorkspace.didLaunchApplicationNotification, object: nil, queue: .main
            ) { [weak self] note in
                guard let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
                else { return }
                MainActor.assumeIsolated { self?.attachIfListed(app) }
            })
            workspaceTokens.append(center.addObserver(
                forName: NSWorkspace.didTerminateApplicationNotification, object: nil, queue: .main
            ) { [weak self] note in
                guard let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
                else { return }
                MainActor.assumeIsolated { self?.detach(app.processIdentifier) }
            })
        }
        for pid in Array(observers.keys) {
            let id = NSRunningApplication(processIdentifier: pid)?.bundleIdentifier
            if id == nil || !self.bundleIDs.contains(id!) { detach(pid) }
        }
        for app in NSWorkspace.shared.runningApplications { attachIfListed(app) }
    }

    func stop() {
        let center = NSWorkspace.shared.notificationCenter
        workspaceTokens.forEach { center.removeObserver($0) }
        workspaceTokens.removeAll()
        for pid in Array(observers.keys) { detach(pid) }
        pending.values.forEach { $0.cancel() }
        pending.removeAll()
        bundleIDs.removeAll()
    }

    // MARK: - Watching windows

    private func attachIfListed(_ app: NSRunningApplication) {
        guard let id = app.bundleIdentifier, bundleIDs.contains(id),
              app.activationPolicy == .regular, observers[app.processIdentifier] == nil
        else { return }
        let pid = app.processIdentifier
        // The application's window created and focus changes, plus each window's destruction
        // (the observer subscribes to those itself).
        let observer = RightKitAXObserver(
            pid: pid,
            notifications: [RightKitAXNotifications.windowCreated, RightKitAXNotifications.focusedWindowChanged])
        guard (try? observer.start()) != nil else { return }
        observers[pid] = observer
        listeners[pid] = Task { [weak self] in
            for await event in observer.events { self?.windowEvent(pid: pid, name: event.notification) }
        }
    }

    private func detach(_ pid: pid_t) {
        pending[pid]?.cancel()
        pending[pid] = nil
        listeners.removeValue(forKey: pid)?.cancel()
        observers.removeValue(forKey: pid)?.stop()
    }

    private func windowEvent(pid: pid_t, name: String) {
        guard observers[pid] != nil else { return }
        if name == RightKitAXNotifications.windowCreated {
            // A new window: the app is not finished (the observer watches this one too).
            pending[pid]?.cancel()
            pending[pid] = nil
            return
        }
        pending[pid]?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.quitIfFinished(pid) }
        }
        pending[pid] = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.debounce, execute: work)
    }

    // MARK: - The decision

    private func quitIfFinished(_ pid: pid_t) {
        pending[pid] = nil
        guard let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated,
              app.isFinishedLaunching, app.activationPolicy == .regular,
              let id = app.bundleIdentifier, bundleIDs.contains(id), !Self.neverQuit.contains(id)
        else { return }
        // Known and empty through Accessibility. An unreadable list is unknown.
        let application = RightKitAXElement.application(pid: pid)
        application.setMessagingTimeout(0.25)
        guard let windows = try? application.value(kAXWindowsAttribute) as? [AXUIElement], windows.isEmpty else { return }
        // And none anywhere else: minimized, hidden, or on another Space.
        guard !Self.hasAnyWindow(pid) else { return }
        app.terminate()
    }

    private static func hasAnyWindow(_ pid: pid_t) -> Bool {
        guard let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]]
        else { return true }
        return list.contains { info in
            guard (info[kCGWindowOwnerPID as String] as? Int32) == pid,
                  (info[kCGWindowLayer as String] as? Int) == 0
            else { return false }
            let bounds = info[kCGWindowBounds as String] as? [String: CGFloat]
            return (bounds?["Width"] ?? 100) >= 40 && (bounds?["Height"] ?? 100) >= 40
        }
    }
}
#endif
