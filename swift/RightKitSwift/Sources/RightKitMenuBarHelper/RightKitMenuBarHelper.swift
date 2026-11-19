#if os(macOS)
import AppKit
import Foundation
import RightKitActionChannel

// A standalone menu-bar helper process (macOS only; empty elsewhere).
//
// WHY A SEPARATE APP: macOS can permanently refuse to let Control Center adopt a given bundle
// identifier's NSStatusItem (the item is created and permitted, but parked off-screen). Renaming the
// MAIN app would cost every user their Accessibility, Microphone and Keychain grants, and would cost
// it again whenever the bug recurred. So the status item lives in a helper whose identifier is
// disposable. The helper holds NO permissions: it draws an icon and forwards clicks; everything else
// stays in the main app.
//
// ACTIONS go to the main app over the authenticated action channel (``ActionChannelConnection``): the
// app executes one only after the operating system has shown that this process is the signed helper,
// and this process sends nothing to a socket whose listener is not the signed main app. A distributed
// notification would let any process of the user forge an action, so none is used for actions.
// STATE flowing the other way (the icon) carries no authority (any process may post it; the worst
// outcome is a wrong icon until the next real state), so it stays a distributed notification.

/// One entry of the right-click menu.
public enum RightKitMenuBarEntry: Sendable {
    /// A titled item; choosing it sends `action` to the app.
    case item(title: String, action: String)
    case separator
}

/// Everything product-specific about the helper. Every value is a host input.
public struct RightKitMenuBarConfiguration: Sendable {
    /// Status item tooltip.
    public var tooltip: String
    /// The right-click menu, in order.
    public var menu: [RightKitMenuBarEntry]
    /// Action sent on a left click.
    public var leftClickAction: String
    /// The icon states the app may announce. Any other value is ignored, so a future state cannot
    /// blank the icon.
    public var states: [String]
    /// The state shown until the app announces one. Must be one of ``states``.
    public var initialState: String
    /// Name of the distributed notification carrying the icon state as its `object` (a plain string).
    public var stateNotificationName: String
    /// Environment variable holding the main app's pid, so the helper exits when it is orphaned.
    public var parentPIDEnvironmentKey: String
    /// Directory inside this helper's own `Bundle.main.resourceURL` that holds the icons.
    public var iconDirectory: String
    /// File name of the icon for a state.
    public var iconFileName: @Sendable (_ state: String) -> String
    /// Side of the square icon, in points.
    public var iconSide: CGFloat
    /// Action posted once at launch.
    public var readyAction: String
    /// Action posted every ``heartbeatInterval`` seconds; the main app's supervisor relaunches the
    /// helper if it stops (crash, or being killed). Its delivery failures are not logged.
    public var heartbeatAction: String
    public var heartbeatInterval: TimeInterval
    /// How often the helper checks that its parent is still alive.
    public var parentPollInterval: TimeInterval
    /// The action channel: the stem the app listens on and the requirement its listener must satisfy.
    public var socketName: String
    public var serverRequirement: String
    /// Label of the serial queue the blocking socket I/O runs on, and the prefix of delivery-failure log lines.
    public var queueLabel: String
    public var logPrefix: String

    public init(
        tooltip: String,
        menu: [RightKitMenuBarEntry],
        leftClickAction: String,
        states: [String],
        initialState: String,
        stateNotificationName: String,
        parentPIDEnvironmentKey: String,
        iconDirectory: String,
        iconFileName: @escaping @Sendable (_ state: String) -> String,
        iconSide: CGFloat = 18,
        readyAction: String = "helper_ready",
        heartbeatAction: String = "heartbeat",
        heartbeatInterval: TimeInterval = 3,
        parentPollInterval: TimeInterval = 2,
        socketName: String,
        serverRequirement: String,
        queueLabel: String,
        logPrefix: String
    ) {
        precondition(states.contains(initialState), "the initial state must be one of the states")
        self.tooltip = tooltip
        self.menu = menu
        self.leftClickAction = leftClickAction
        self.states = states
        self.initialState = initialState
        self.stateNotificationName = stateNotificationName
        self.parentPIDEnvironmentKey = parentPIDEnvironmentKey
        self.iconDirectory = iconDirectory
        self.iconFileName = iconFileName
        self.iconSide = iconSide
        self.readyAction = readyAction
        self.heartbeatAction = heartbeatAction
        self.heartbeatInterval = heartbeatInterval
        self.parentPollInterval = parentPollInterval
        self.socketName = socketName
        self.serverRequirement = serverRequirement
        self.queueLabel = queueLabel
        self.logPrefix = logPrefix
    }
}

public enum RightKitMenuBarHelper {
    /// Runs the helper: an accessory-policy application with one status item. Returns only when the
    /// application loop ends. Call from the executable's top level (the main thread).
    @MainActor
    public static func run(_ configuration: RightKitMenuBarConfiguration) {
        let app = NSApplication.shared
        let delegate = RightKitMenuBarDelegate(configuration)
        app.delegate = delegate
        app.run()
        withExtendedLifetime(delegate) {}
    }
}

@MainActor
final class RightKitMenuBarDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private let configuration: RightKitMenuBarConfiguration
    private let actionQueue: DispatchQueue
    private let actionChannel: ActionChannelConnection
    private var item: NSStatusItem!
    private var optionsMenu: NSMenu!
    private var iconState: String
    private var parentPid: pid_t = 0

    init(_ configuration: RightKitMenuBarConfiguration) {
        self.configuration = configuration
        self.iconState = configuration.initialState
        self.actionQueue = DispatchQueue(label: configuration.queueLabel, qos: .userInitiated)
        self.actionChannel = ActionChannelConnection(
            socketName: configuration.socketName,
            serverRequirement: configuration.serverRequirement
        )
        super.init()
    }

    func applicationDidFinishLaunching(_ note: Notification) {
        NSApp.setActivationPolicy(.accessory)

        // The main app passes its pid so the helper can exit if it is orphaned.
        if let raw = ProcessInfo.processInfo.environment[configuration.parentPIDEnvironmentKey], let pid = pid_t(raw) {
            parentPid = pid
        }
        watchParent()

        item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.toolTip = configuration.tooltip
        item.button?.target = self
        item.button?.action = #selector(statusItemClicked(_:))
        item.button?.sendAction(on: [.leftMouseUp, .rightMouseUp])
        applyIcon()

        optionsMenu = NSMenu()
        optionsMenu.delegate = self
        for entry in configuration.menu {
            switch entry {
            case let .item(title, action):
                let menuItem = optionsMenu.addItem(withTitle: title, action: #selector(menuItemChosen(_:)), keyEquivalent: "")
                menuItem.target = self
                menuItem.representedObject = action
            case .separator:
                optionsMenu.addItem(.separator())
            }
        }

        DistributedNotificationCenter.default().addObserver(
            self,
            selector: #selector(stateChanged(_:)),
            name: NSNotification.Name(configuration.stateNotificationName),
            object: nil
        )
        post(configuration.readyAction)
        // Proof of life for the main app's supervisor.
        Timer.scheduledTimer(withTimeInterval: configuration.heartbeatInterval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.post(self.configuration.heartbeatAction)
            }
        }
    }

    /// Icons live in THIS helper's own Resources (packaging copies them there). `Bundle.main.resourceURL`
    /// is the canonical, location-independent path to them, so it survives App Translocation and any
    /// symlinked launch path, unlike walking up to the parent app.
    private func iconURL(_ name: String) -> URL? {
        guard let res = Bundle.main.resourceURL else { return nil }
        let url = res.appendingPathComponent("\(configuration.iconDirectory)/\(name)")
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    private func applyIcon() {
        let name = configuration.iconFileName(iconState)
        guard let url = iconURL(name), let img = NSImage(contentsOf: url) else { return }
        img.size = NSSize(width: configuration.iconSide, height: configuration.iconSide)
        img.isTemplate = false
        item.button?.image = img
    }

    /// STATE only; carries no authority. The payload rides in `object` (a plain string for distributed
    /// notifications) rather than userInfo. An unknown value is ignored.
    @objc private func stateChanged(_ note: Notification) {
        guard let value = note.object as? String else { return }
        guard configuration.states.contains(value) else { return }
        guard value != iconState else { return }
        iconState = value
        applyIcon()
    }

    /// Send an ACTION to the main app over the authenticated channel. Blocking socket I/O stays off the
    /// main thread. A click that cannot be delivered is logged (the action name only).
    private func post(_ action: String) {
        let channel = actionChannel
        let heartbeat = configuration.heartbeatAction
        let prefix = configuration.logPrefix
        actionQueue.async {
            do {
                try channel.send(action)
            } catch {
                if action != heartbeat { NSLog("%@: action %@ not delivered: %@", prefix, action, "\(error)") }
            }
        }
    }

    /// Keep the menu detached during normal operation: AppKit opens an attached NSStatusItem menu for
    /// every click before its button action can run. A left click therefore forwards straight to its
    /// action; a right click attaches the menu only for the duration of AppKit's menu-tracking session.
    @objc private func statusItemClicked(_ sender: NSStatusBarButton) {
        switch NSApp.currentEvent?.type {
        case .leftMouseUp:
            post(configuration.leftClickAction)
        case .rightMouseUp:
            item.menu = optionsMenu
            sender.performClick(nil)
        default:
            break
        }
    }

    func menuDidClose(_ menu: NSMenu) {
        item.menu = nil
    }

    @objc private func menuItemChosen(_ sender: NSMenuItem) {
        if let action = sender.representedObject as? String { post(action) }
    }

    /// A helper outliving its main app would leave an orphaned icon that does nothing, so poll the
    /// parent and exit with it.
    private func watchParent() {
        guard parentPid > 0 else { return }
        let pid = parentPid
        Timer.scheduledTimer(withTimeInterval: configuration.parentPollInterval, repeats: true) { _ in
            if kill(pid, 0) != 0 { MainActor.assumeIsolated { NSApp.terminate(nil) } }
        }
    }
}
#endif
