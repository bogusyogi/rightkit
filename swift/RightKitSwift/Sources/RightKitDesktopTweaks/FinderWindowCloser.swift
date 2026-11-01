#if os(macOS)
// Copyright (c) 2026 Damned Ventures LLC, d/b/a Orthic Labs. All rights reserved.

import AppKit
import ApplicationServices
import RightKitAccessibility

/// **Closes the Finder window Finder opens for a mounted disk image**, through
/// the Accessibility permission Pulse already holds (no Finder Automation).
/// Only a window that shows exactly that volume's root is closed: matched by its
/// document URL, or by its title when it states no document. Finder opens the
/// window a moment after the mount, so the look repeats for about three seconds.
public enum FinderWindowCloser {
    private static let finderID = "com.apple.finder"
    private static let patience: TimeInterval = 3
    private static let interval: UInt64 = 300_000_000

    /// Starts the closing in the background; returns at once. Nothing happens
    /// when Accessibility is not granted.
    public static func closeWindows(forVolume mount: URL) {
        guard RightKitAXTrust.isTrusted() else { return }
        let root = mount.standardizedFileURL.resolvingSymlinksInPath().path
        let name = (try? mount.resourceValues(forKeys: [.volumeNameKey]))?.volumeName
            ?? mount.lastPathComponent
        Task.detached(priority: .utility) {
            let deadline = Date().addingTimeInterval(patience)
            repeat {
                if closeMatching(root: root, name: name) { return }
                try? await Task.sleep(nanoseconds: interval)
            } while Date() < deadline
        }
    }

    /// Closes the volume's Finder window on the calling thread and waits, up to `patience`, until
    /// Finder has none left, so a detach that follows is not refused as busy (#412). Returns at once
    /// when no window was ever seen. Call it off the main thread; nothing happens without Accessibility.
    public static func releaseVolume(_ mount: URL) {
        guard RightKitAXTrust.isTrusted() else { return }
        let root = mount.standardizedFileURL.resolvingSymlinksInPath().path
        let name = (try? mount.resourceValues(forKeys: [.volumeNameKey]))?.volumeName
            ?? mount.lastPathComponent
        let deadline = Date().addingTimeInterval(patience)
        var closedOne = false
        repeat {
            if closeMatching(root: root, name: name) {
                closedOne = true
            } else if closedOne {
                return
            }
            Thread.sleep(forTimeInterval: 0.3)
        } while Date() < deadline
    }

    /// True when a window was closed.
    private static func closeMatching(root: String, name: String) -> Bool {
        guard let finder = NSRunningApplication.runningApplications(withBundleIdentifier: finderID).first
        else { return false }
        let app = RightKitAXElement.application(pid: finder.processIdentifier)
        app.setMessagingTimeout(1)
        guard let list = try? app.value(kAXWindowsAttribute) as? [AXUIElement] else { return false }
        var closed = false
        for window in list.map(RightKitAXElement.init) where matches(window, root: root, name: name) {
            if (try? RightKitAXWindowAction.close(window)) != nil { closed = true }
        }
        return closed
    }

    private static func matches(_ window: RightKitAXElement, root: String, name: String) -> Bool {
        if let text = window.string(kAXDocumentAttribute), !text.isEmpty {
            let path = URL(string: text)?.standardizedFileURL.resolvingSymlinksInPath().path
                ?? URL(fileURLWithPath: text).standardizedFileURL.resolvingSymlinksInPath().path
            return path == root
        }
        // No document: the title is all there is.
        guard let text = window.title else { return false }
        return text == name
    }
}
#endif
