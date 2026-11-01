#if os(macOS)
import RightKitNotchCore
import AppKit
import ApplicationServices
import Carbon.HIToolbox
import CoreServices
import Foundation
import RightKitAccessibility

/// Copying, pasting and opening on the launcher's behalf.
@MainActor
enum LauncherActions {
    static func copy(_ text: String) {
        Runtime.refuseUnderTest("the pasteboard")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    /// Puts text on the clipboard, then pastes it into the app that was in front.
    static func paste(_ text: String, into app: NSRunningApplication?, clipboard: LauncherClipboard) {
        copy(text)
        clipboard.noteOwnWrite()
        send(into: app)
    }

    /// Activates `app` and sends Command-V. Needs Accessibility; without it the
    /// text simply stays on the clipboard.
    static func send(into app: NSRunningApplication?) {
        guard RightKitAXTrust.isTrusted() else { return }
        app?.activate()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.12) {
            let source = CGEventSource(stateID: .combinedSessionState)
            for down in [true, false] {
                let event = CGEvent(keyboardEventSource: source, virtualKey: CGKeyCode(kVK_ANSI_V), keyDown: down)
                event?.flags = .maskCommand
                event?.post(tap: .cghidEventTap)
            }
        }
    }

    /// A web address, a deeplink, or a file path (with `~` expanded).
    static func open(_ target: String) {
        if let url = URL(string: target), let scheme = url.scheme, scheme.count > 1 {
            NSWorkspace.shared.open(url)
        } else {
            NSWorkspace.shared.open(URL(fileURLWithPath: (target as NSString).expandingTildeInPath))
        }
    }

    /// Percent-encodes a value for a URL query: only unreserved characters pass.
    static func encode(_ text: String) -> String {
        LauncherURLText.encode(text)
    }
}

/// Fills `{name}` placeholders in one pass, so inserted text is never re-read.
enum LauncherTemplate {
    static func fill(_ template: String, _ values: [String: String]) -> String {
        var output = ""
        var rest = Substring(template)
        while let open = rest.firstIndex(of: "{") {
            output += rest[..<open]
            let afterOpen = rest.index(after: open)
            guard let close = rest[afterOpen...].firstIndex(of: "}") else {
                output += rest[open...]
                rest = ""
                break
            }
            let name = String(rest[afterOpen..<close])
            if let value = values[name] {
                output += value
            } else {
                output += rest[open...close]
            }
            rest = rest[rest.index(after: close)...]
        }
        output += rest
        return output
    }

    static func today() -> String {
        Date().formatted(date: .abbreviated, time: .omitted)
    }

    @MainActor
    static func clipboardText() -> String {
        Runtime.refuseUnderTest("the pasteboard")
        return NSPasteboard.general.string(forType: .string) ?? ""
    }
}

/// Definitions from the Mac's own dictionaries (DictionaryServices).
enum LauncherDictionary {
    static func definition(of word: String) -> String? {
        guard let unmanaged = DCSCopyTextDefinition(nil, word as CFString,
                                                    CFRange(location: 0, length: (word as NSString).length))
        else { return nil }
        let text = unmanaged.takeRetainedValue() as String
        return text.isEmpty ? nil : text
    }

    /// Opens the word in Dictionary.app.
    static func open(_ word: String) {
        guard let encoded = word.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed),
              let url = URL(string: "dict://" + encoded) else { return }
        NSWorkspace.shared.open(url)
    }
}

/// Runs a command with the login shell and returns its output, stdout then stderr, each
/// capped at 64 KB and stopped after `timeout` seconds. Each stream is read as it arrives
/// on a handler of its own, the first 64 KB kept and the rest drained so the child never
/// blocks on a full pipe, and reading stops 2 s after the shell exits, so a grandchild
/// that holds a pipe open cannot hold the result (the Windows notch does the same).
enum LauncherShell {
    private static let outputCap = 64 * 1024
    private static let drainWait: TimeInterval = 2

    /// One pipe's kept bytes; `finished` is signalled at end of stream.
    private final class Capture: @unchecked Sendable {
        private let lock = NSLock()
        private var kept = Data()
        let finished = DispatchSemaphore(value: 0)

        func append(_ chunk: Data) {
            lock.lock(); defer { lock.unlock() }
            kept.append(chunk.prefix(max(0, LauncherShell.outputCap - kept.count)))
        }

        var text: String {
            lock.lock(); defer { lock.unlock() }
            return String(decoding: kept, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        }

        func attach(to pipe: Pipe) {
            pipe.fileHandleForReading.readabilityHandler = { [self] handle in
                let chunk = handle.availableData
                if chunk.isEmpty {
                    handle.readabilityHandler = nil
                    finished.signal()
                } else {
                    append(chunk)
                }
            }
        }
    }

    static func run(_ command: String, timeout: TimeInterval = 30, completion: @escaping (String) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/bin/zsh")
            process.arguments = ["-lc", command]
            let outPipe = Pipe()
            let errPipe = Pipe()
            process.standardOutput = outPipe
            process.standardError = errPipe
            process.standardInput = FileHandle.nullDevice
            let out = Capture()
            let err = Capture()
            out.attach(to: outPipe)
            err.attach(to: errPipe)
            do {
                try process.run()
            } catch {
                outPipe.fileHandleForReading.readabilityHandler = nil
                errPipe.fileHandleForReading.readabilityHandler = nil
                DispatchQueue.main.async {
                    completion("Could not start the shell: \(error.localizedDescription)")
                }
                return
            }
            let stop = DispatchWorkItem {
                if process.isRunning { process.terminate() }
            }
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: stop)
            process.waitUntilExit()
            stop.cancel()
            // The shell is gone: give the pipes two seconds to reach their end, then let go
            // of whatever a grandchild still holds.
            let deadline = DispatchTime.now() + drainWait
            _ = out.finished.wait(timeout: deadline)
            _ = err.finished.wait(timeout: deadline)
            outPipe.fileHandleForReading.readabilityHandler = nil
            errPipe.fileHandleForReading.readabilityHandler = nil
            try? outPipe.fileHandleForReading.close()
            try? errPipe.fileHandleForReading.close()
            var text = [out.text, err.text].filter { !$0.isEmpty }.joined(separator: "\n\n")
            if process.terminationStatus != 0 {
                text += (text.isEmpty ? "" : "\n\n") + "Exited with status \(process.terminationStatus)."
            }
            if text.isEmpty { text = "Done. No output." }
            let result = text
            DispatchQueue.main.async { completion(result) }
        }
    }
}

/// Apple Shortcuts, through the `shortcuts` command. The list is read in the
/// background and kept for ten minutes.
@MainActor
final class LauncherShortcuts {
    private(set) var names: [String] = []
    private var fetchedAt: Date?
    private var fetching = false
    var onUpdate: (() -> Void)?

    func refreshIfStale() {
        if fetching { return }
        if let fetchedAt, Date().timeIntervalSince(fetchedAt) < 600 { return }
        fetching = true
        DispatchQueue.global(qos: .utility).async {
            let found = LauncherShortcuts.list()
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.fetching = false
                self.fetchedAt = Date()
                if found != self.names {
                    self.names = found
                    self.onUpdate?()
                }
            }
        }
    }

#if PULSE_QA
    /// The QA journey's fixture Shortcuts (App/ViewShots.swift): a name list held in memory, so the
    /// launcher's search, alias and run rows are asserted without reading the account's own shortcuts.
    func installFixtureNames(_ fixture: [String]) {
        names = fixture
        fetchedAt = Date()
    }
#endif

    nonisolated static func list() -> [String] {
        // The user's own shortcuts are real user data: none are listed under test (the
        // picture-rendering run reaches this while it builds the launcher).
        if Runtime.isUnderTest { return [] }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/shortcuts")
        process.arguments = ["list"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            return []
        }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return String(decoding: data, as: UTF8.self)
            .split(separator: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    /// Runs a shortcut by name, without waiting for it.
    nonisolated static func run(_ name: String) {
        if Runtime.isUnderTest { return }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/shortcuts")
        process.arguments = ["run", name]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try? process.run()
    }
}

/// Regular running apps, and the actions on them.
@MainActor
enum LauncherApps {
    /// Running apps that show in the Dock, sorted by name. Pulse is left out.
    static func running() -> [NSRunningApplication] {
        let me = ProcessInfo.processInfo.processIdentifier
        return NSWorkspace.shared.runningApplications
            .filter { $0.activationPolicy == .regular && $0.processIdentifier != me && $0.bundleURL != nil }
            .sorted { ($0.localizedName ?? "") < ($1.localizedName ?? "") }
    }

    /// Hides the app when it is in front; otherwise brings it forward, or launches it.
    static func toggle(_ url: URL) {
        let target = url.resolvingSymlinksInPath().path
        guard let app = NSWorkspace.shared.runningApplications.first(where: {
            $0.bundleURL?.resolvingSymlinksInPath().path == target
        }) else {
            NSWorkspace.shared.open(url)
            return
        }
        if app.isActive {
            app.hide()
        } else {
            app.unhide()
            app.activate()
        }
    }

    /// Asks every running app to quit, except Pulse. Apps with unsaved work may ask first.
    static func quitAll() {
        for app in running() { app.terminate() }
    }
}
#endif
