#if os(macOS)
// Copyright (c) 2026 Damned Ventures LLC, d/b/a Orthic Labs. All rights reserved.

import AppKit
import ApplicationServices
import RightKitAccessibility
import RightKitOverlay

/// **An optional path field above the front Finder window** (Orthic-Labs/pulse#186,
/// `finderPathField`, off by default). Finder has no address bar to extend, so
/// the app draws its own small field flush against the window's top edge. It
/// shows the front window's folder (read through the Accessibility permission
/// the app already holds, as `FinderWindowCloser` does) and lets you type or paste
/// a path and press Return to send that window there (Finder Automation, the
/// same permission the Finder window targets need; macOS asks the first time).
/// A path that does not exist, or a refusal from Finder, is said in the field
/// and nothing moves. The field is hidden whenever Finder is not frontmost.
@MainActor
final class FinderPathField: NSObject, NSTextFieldDelegate {
    private static let finderID = "com.apple.finder"
    private static let height: CGFloat = 26

    private var panel: PathPanel?
    private let field = NSTextField()
    private var timer: Timer?
    private var activation: NSObjectProtocol?
    private var running = false
    private var shownPath = ""
    private var going = false
    private nonisolated static let work = DispatchQueue(label: "pulse.finderpathfield", qos: .userInitiated)

    var isRunning: Bool { running }

    /// The 0.5 s follow runs only while Finder is the front app, which is the only time the
    /// field is shown. Another app in front costs nothing: the activation notification starts
    /// the follow again when Finder comes forward.
    func start() {
        guard !running else { return }
        running = true
        activation = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.frontAppChanged() }
        }
        frontAppChanged()
    }

    func stop() {
        running = false
        if let activation { NSWorkspace.shared.notificationCenter.removeObserver(activation) }
        activation = nil
        timer?.invalidate()
        timer = nil
        panel?.orderOut(nil)
        panel = nil
        shownPath = ""
    }

    private func frontAppChanged() {
        guard running else { return }
        let finderFront = NSWorkspace.shared.frontmostApplication?.bundleIdentifier == Self.finderID
        if finderFront {
            if timer == nil {
                timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
                    MainActor.assumeIsolated { self?.refresh() }
                }
            }
        } else {
            timer?.invalidate()
            timer = nil
        }
        // One look either way: shows the field for Finder, hides it for anything else.
        refresh()
    }

    // MARK: - Following the front Finder window

    private func refresh() {
        guard RightKitAXTrust.isTrusted() else { panel?.orderOut(nil); return }
        let front = NSWorkspace.shared.frontmostApplication
        let editing = panel?.isKeyWindow == true
        let finderFront = front?.bundleIdentifier == Self.finderID
        // Another app in front hides the field even mid-edit.
        guard finderFront else {
            if editing { panel?.makeFirstResponder(nil) }
            panel?.orderOut(nil)
            return
        }
        guard let window = Self.frontFinderWindow() else {
            if !editing { panel?.orderOut(nil) }
            return
        }
        let panel = ensurePanel()
        if let frame = Self.frame(of: window) {
            position(panel, over: frame)
        }
        let path = Self.path(of: window)
        if !editing, let path, path != shownPath {
            shownPath = path
            field.stringValue = path
            field.textColor = .labelColor
        }
        if !panel.isVisible { panel.orderFront(nil) }
    }

    private func ensurePanel() -> PathPanel {
        if let panel { return panel }
        let panel = PathPanel(contentRect: NSRect(x: 0, y: 0, width: 400, height: Self.height))
        let bar = NSVisualEffectView(frame: panel.contentView?.bounds ?? .zero)
        bar.material = .titlebar
        bar.state = .active
        bar.autoresizingMask = [.width, .height]
        field.frame = bar.bounds.insetBy(dx: 6, dy: 3)
        field.autoresizingMask = [.width, .height]
        field.font = .systemFont(ofSize: 12)
        field.placeholderString = "Type or paste a path, press Return to go"
        field.isBezeled = true
        field.bezelStyle = .roundedBezel
        field.lineBreakMode = .byTruncatingHead
        field.cell?.isScrollable = true
        field.delegate = self
        bar.addSubview(field)
        panel.contentView = bar
        self.panel = panel
        return panel
    }

    /// The panel sits just above the window, or just under the menu bar when
    /// the window already touches the top of the screen.
    private func position(_ panel: NSPanel, over frame: CGRect) {
        guard let primary = NSScreen.screens.first else { return }
        let width = max(frame.width, 240)
        let topOfWindow = primary.frame.height - frame.minY
        var origin = CGPoint(x: frame.minX, y: topOfWindow)
        if let screen = NSScreen.screens.first(where: { $0.frame.contains(CGPoint(x: origin.x, y: topOfWindow - 1)) })
            ?? NSScreen.main {
            let ceiling = screen.visibleFrame.maxY - Self.height
            origin.y = min(origin.y, ceiling)
        }
        panel.setFrame(NSRect(x: origin.x, y: origin.y, width: width, height: Self.height), display: true)
    }

    // MARK: - Going somewhere

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        if selector == #selector(NSResponder.insertNewline(_:)) {
            go(to: field.stringValue)
            return true
        }
        if selector == #selector(NSResponder.cancelOperation(_:)) {
            field.stringValue = shownPath
            field.textColor = .labelColor
            giveFocusBack()
            return true
        }
        return false
    }

    /// Leaves editing and hands the keyboard back to Finder.
    private func giveFocusBack() {
        panel?.makeFirstResponder(nil)
        panel?.resignKey()
        NSRunningApplication.runningApplications(withBundleIdentifier: Self.finderID).first?
            .activate(options: [])
    }

    private func go(to typed: String) {
        guard !going else { return }
        going = true
        field.textColor = .secondaryLabelColor
        field.stringValue = "Going…"
        // The path check and the Finder request both wait, so neither runs on the main thread.
        Self.work.async { [weak self] in
            let trimmed = typed.trimmingCharacters(in: .whitespacesAndNewlines)
            let expanded = (trimmed as NSString).expandingTildeInPath
            var isDirectory: ObjCBool = false
            guard !trimmed.isEmpty, FileManager.default.fileExists(atPath: expanded, isDirectory: &isDirectory) else {
                DispatchQueue.main.async { MainActor.assumeIsolated { self?.finish(failure: "No such folder", typed: typed) } }
                return
            }
            // A file shows its folder, with the file selected.
            let url = URL(fileURLWithPath: expanded).standardizedFileURL
            let isDir = isDirectory.boolValue
            let folder = isDir ? url : url.deletingLastPathComponent()
            let message = Self.sendFinder(to: folder, select: isDir ? nil : url)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    if let message {
                        self?.finish(failure: message, typed: typed)
                    } else {
                        self?.finish(folder: folder.path)
                    }
                }
            }
        }
    }

    private func finish(folder: String) {
        going = false
        shownPath = folder
        field.stringValue = folder
        field.textColor = .labelColor
        giveFocusBack()
    }

    private func finish(failure message: String, typed: String) {
        going = false
        fail(message)
    }

    private func fail(_ message: String) {
        field.textColor = .systemRed
        field.stringValue = message
        field.selectText(nil)
    }

    /// Returns nil on success, else the words to show.
    private nonisolated static func sendFinder(to folder: URL, select file: URL?) -> String? {
        let target = appleScriptString(folder.path)
        var lines = [
            "tell application \"Finder\"",
            "set theFolder to (POSIX file \(target)) as alias",
            "if (count of Finder windows) is 0 then",
            "make new Finder window to theFolder",
            "else",
            "set target of front Finder window to theFolder",
            "end if",
        ]
        if let file {
            lines.append("select (POSIX file \(appleScriptString(file.path)) as alias)")
        }
        lines.append("end tell")
        guard let script = NSAppleScript(source: lines.joined(separator: "\n")) else { return "Could not build the request" }
        var error: NSDictionary?
        script.executeAndReturnError(&error)
        guard let error else { return nil }
        let number = error[NSAppleScript.errorNumber] as? Int ?? 0
        if number == -1743 { return "Allow the app to control Finder in System Settings > Privacy > Automation" }
        return (error[NSAppleScript.errorMessage] as? String) ?? "Finder refused"
    }

    private nonisolated static func appleScriptString(_ text: String) -> String {
        let escaped = text.replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        return "\"\(escaped)\""
    }

    // MARK: - Accessibility reads

    private static func frontFinderWindow() -> RightKitAXElement? {
        guard let finder = NSRunningApplication.runningApplications(withBundleIdentifier: finderID).first
        else { return nil }
        let app = RightKitAXElement.application(pid: finder.processIdentifier)
        app.setMessagingTimeout(axTimeout)
        let window = app.focusedWindow ?? app.windows.first
        window?.setMessagingTimeout(axTimeout)
        return window
    }

    /// Every Accessibility read gives up quickly, so the 0.5 s timer never stalls the main thread.
    private static let axTimeout: Float = 0.15

    /// The window's folder, or nil for a window that names none (Recents, AirDrop).
    private static func path(of window: RightKitAXElement) -> String? {
        window.setMessagingTimeout(axTimeout)
        guard let text = window.string(kAXDocumentAttribute), !text.isEmpty else { return nil }
        let url = URL(string: text) ?? URL(fileURLWithPath: text)
        return url.isFileURL ? url.standardizedFileURL.path : nil
    }

    /// Frame in screen points with the origin at the top left of the primary display.
    private static func frame(of window: RightKitAXElement) -> CGRect? {
        window.setMessagingTimeout(axTimeout)
        return window.frame
    }
}

/// A panel that can take typing without making the app the active app.
private final class PathPanel: RightKitOverlayPanel {
    init(contentRect: NSRect) {
        super.init(contentRect: contentRect, configuration: RightKitOverlayPanelConfiguration(
            level: .floating,
            collectionBehavior: [.canJoinAllSpaces, .fullScreenAuxiliary],
            canBecomeKey: true,
            becomesKeyOnlyIfNeeded: false,
            hasShadow: true,
            hidesOnDeactivate: false))
    }
}
#endif
