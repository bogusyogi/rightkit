#if os(macOS)
import Cocoa
import FinderSync
import notify

// Layout, NSExtension keys and sandbox entitlements adapted from Uninstally's
// UninstallyFinder extension (github.com/gostonx/uninstally, MIT, (c) 2026
// Codenta). See docs/donors.md.

/// Adds Cut, Copy Path and Open in Terminal as top-level items in Finder's
/// right-click menu, system-wide.
///
/// Finder Sync extensions must be sandboxed, so this does nothing but the
/// pasteboard and a Launch Services open; it never touches file contents.
final class FinderSync: FIFinderSync {
    override init() {
        super.init()
        // The startup disk ("/") and drive D only. Finder puts a Finder Sync extension's icon on every
        // observed volume in its sidebar, so observing each mounted volume replaced the drives' own
        // icons with the app's (#402). Adrian 2026-10-19: "I need Copy Path there; put D back even if D
        // shows the the app icon, but keep E and disk images on their own icons." D can mount after
        // Finder starts the extension, so the set is refreshed on mount and unmount.
        updateObservedFolders()
        let center = NSWorkspace.shared.notificationCenter
        for name in [NSWorkspace.didMountNotification, NSWorkspace.didUnmountNotification] {
            volumeObservers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                self?.updateObservedFolders()
            })
        }
    }

    private var volumeObservers: [NSObjectProtocol] = []

    deinit {
        volumeObservers.forEach { NSWorkspace.shared.notificationCenter.removeObserver($0) }
    }

    private static let workDrive = URL(fileURLWithPath: "/Volumes/D", isDirectory: true)

    private func updateObservedFolders() {
        var folders: Set<URL> = [URL(fileURLWithPath: "/")]
        if FileManager.default.fileExists(atPath: Self.workDrive.path) { folders.insert(Self.workDrive) }
        FIFinderSyncController.default().directoryURLs = folders
    }

    // MARK: Menu

    override func menu(for menuKind: FIMenuKind) -> NSMenu? {
        guard menuKind == .contextualMenuForItems || menuKind == .contextualMenuForContainer else { return nil }
        // Top-level items, no submenu. Cut only for selected items.
        let menu = NSMenu(title: "")
        let hidden = Self.hiddenItems()
        if menuKind == .contextualMenuForItems, hidden & 1 == 0 {
            menu.addItem(item("Cut", #selector(cut(_:)), symbol: "scissors"))
        }
        if hidden & 2 == 0 {
            menu.addItem(item("Copy Path", #selector(copyPath(_:)), symbol: "doc.on.clipboard"))
        }
        if hidden & 4 == 0 {
            menu.addItem(item("Open in Terminal", #selector(openInTerminal(_:)), symbol: "apple.terminal"))
        }
        return menu.items.isEmpty ? nil : menu
    }

    /// Which items the app has switched off in Settings: bit 1 Cut, 2 Copy Path,
    /// 4 Open in Terminal. the app publishes it as the state of a system
    /// notification name (Conveniences/FinderMenuState.swift); unset reads 0,
    /// which shows all three.
    private static func hiddenItems() -> UInt64 {
        var token: Int32 = 0
        guard notify_register_check("dev.orthic.pulse.finder.menu", &token) == 0 else { return 0 }
        defer { notify_cancel(token) }
        var state: UInt64 = 0
        _ = notify_get_state(token, &state)
        return state
    }

    private func item(_ title: String, _ action: Selector, symbol: String) -> NSMenuItem {
        let entry = NSMenuItem(title: title, action: action, keyEquivalent: "")
        entry.target = self
        entry.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)
        return entry
    }

    // MARK: Actions

    @objc private func copyPath(_ sender: AnyObject?) {
        let paths = targets().map(\.path)
        guard !paths.isEmpty else { return }
        write(paths.joined(separator: "\n"))
    }

    /// Puts the items on the pasteboard as files, as Finder's Copy does, and
    /// tells the app, whose next ⌘V in Finder then moves them (Move Item Here).
    @objc private func cut(_ sender: AnyObject?) {
        guard let selected = FIFinderSyncController.default().selectedItemURLs(), !selected.isEmpty
        else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.writeObjects(selected as [NSURL])
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName("dev.orthic.pulse.finder.cut" as CFString), nil, nil, true)
    }

    @objc private func openInTerminal(_ sender: AnyObject?) {
        guard let first = targets().first,
              let terminal = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal")
        else { return }
        let folder = Self.isFolder(first) ? first : first.deletingLastPathComponent()
        NSWorkspace.shared.open([folder], withApplicationAt: terminal,
                                configuration: NSWorkspace.OpenConfiguration())
    }

    // MARK: Helpers

    /// The selection, or the folder itself when the click was on its background.
    private func targets() -> [URL] {
        let controller = FIFinderSyncController.default()
        if let selected = controller.selectedItemURLs(), !selected.isEmpty { return selected }
        return controller.targetedURL().map { [$0] } ?? []
    }

    private func write(_ text: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
    }

    /// A real folder; app bundles and other packages count as files.
    private static func isFolder(_ url: URL) -> Bool {
        guard let values = try? url.resourceValues(forKeys: [.isDirectoryKey, .isPackageKey]) else {
            return url.hasDirectoryPath
        }
        return values.isDirectory == true && values.isPackage != true
    }
}
#endif
