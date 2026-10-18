import Foundation

#if os(macOS)
import AppKit
import ApplicationServices
import CoreGraphics
import Darwin

public enum RightKitPermissionStatus: String, Sendable, Equatable {
    case granted
    case needsApproval
    case off
    case unknown
}

/// System Settings privacy panes, by the id macOS uses after `?`.
public enum RightKitPrivacyPane: String, Sendable {
    case accessibility = "Privacy_Accessibility"
    case screenRecording = "Privacy_ScreenCapture"
    case fullDiskAccess = "Privacy_AllFiles"
    case inputMonitoring = "Privacy_ListenEvent"
    case automation = "Privacy_Automation"
    case localNetwork = "Privacy_LocalNetwork"

    /// The modern and the older URL for this pane, in the order to try them. Pure.
    public var urls: [URL] {
        [
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?\(rawValue)",
            "x-apple.systempreferences:com.apple.preference.security?\(rawValue)",
        ].compactMap(URL.init(string:))
    }
}

/// Silent permission status checks, and the standard system prompts for the grants that have one.
///
/// Status reads never prompt and never change a grant. The `request...` calls use only the
/// documented system prompts (Accessibility trust, Screen Recording, Automation consent); macOS shows
/// each at most once, after which the switch is in System Settings (`open(_:)`). TCC grants belong to
/// the app that asks: a helper process needs its own. Probes can stall while the machine is swapping
/// (they ask launchd or tccd), so call them off the main thread.
public enum RightKitPermissionProbes {
    // MARK: Status (never prompts)

    public static func accessibility() -> RightKitPermissionStatus {
        AXIsProcessTrusted() ? .granted : .needsApproval
    }

    public static func screenRecording() -> RightKitPermissionStatus {
        CGPreflightScreenCaptureAccess() ? .granted : .needsApproval
    }

    /// Whether this process can read the TCC-protected files that prove Full Disk Access. There is no
    /// API for the grant, so this opens each probe file read-only and reads one byte. `home` is the real
    /// home folder; under test pass a temporary one (see `RightKitRuntime`).
    public static func fullDiskAccess(
        home: URL = FileManager.default.homeDirectoryForCurrentUser,
        probePaths: [String] = ["Library/Safari/Bookmarks.plist", "Library/Application Support/com.apple.TCC/TCC.db"],
    ) -> RightKitPermissionStatus {
        for path in probePaths {
            let descriptor = Darwin.open(home.appendingPathComponent(path).path, O_RDONLY | O_CLOEXEC)
            guard descriptor >= 0 else {
                if let denied = classify(errno: errno) { return denied }
                continue
            }
            var byte: UInt8 = 0
            let count = Darwin.read(descriptor, &byte, 1)
            let failure = errno
            Darwin.close(descriptor)
            if count >= 0 { return .granted }
            if let denied = classify(errno: failure) { return denied }
        }
        return .unknown
    }

    /// `needsApproval` for a permission failure, nil for anything else (a missing file says nothing). Pure.
    public static func classify(errno code: Int32) -> RightKitPermissionStatus? {
        code == EACCES || code == EPERM ? .needsApproval : nil
    }

    /// Automation consent for apple events to `bundleIdentifier`. With `ask` true macOS shows its
    /// consent prompt (and the call blocks until answered); the target must already be running.
    public static func automation(bundleIdentifier: String, ask: Bool) -> RightKitPermissionStatus {
        let target = NSAppleEventDescriptor(bundleIdentifier: bundleIdentifier)
        guard let address = target.aeDesc else { return .unknown }
        let result = AEDeterminePermissionToAutomateTarget(
            address, AEEventClass(typeWildCard), AEEventID(typeWildCard), ask)
        switch result {
        case noErr: return .granted
        case OSStatus(errAEEventNotPermitted), OSStatus(errAEEventWouldRequireUserConsent): return .needsApproval
        default: return .unknown
        }
    }

    // MARK: Standard system prompts

    /// The Accessibility prompt (once). Returns the trust state right now.
    @discardableResult
    public static func requestAccessibility() -> Bool {
        AXIsProcessTrustedWithOptions(["AXTrustedCheckOptionPrompt": true] as CFDictionary)
    }

    /// The Screen Recording prompt (once). Returns whether access is granted right now.
    @discardableResult
    public static func requestScreenRecording() -> Bool {
        CGRequestScreenCaptureAccess()
    }

    // MARK: Finder Sync and app extensions (pluginkit)

    /// `pluginkit -m -i <id>` prints a leading "+" for an enabled extension, "-" for a disabled one and
    /// nothing when it is not registered. Not listed is `unknown`, not `off`: macOS never registered it
    /// (a signed but not notarized build gets this) and no switch can turn it on. Pure over the output.
    public static func parseExtensionStatus(pluginkitOutput: String, identifier: String) -> RightKitPermissionStatus {
        let line = pluginkitOutput.split(separator: "\n").first { $0.contains(identifier) }
        guard let line else { return .unknown }
        return line.drop { $0 == " " }.first == "+" ? .granted : .off
    }

    public static func extensionStatus(identifier: String) -> RightKitPermissionStatus {
        let result = RightKitBoundedProcess.run(
            executable: URL(fileURLWithPath: "/usr/bin/pluginkit"), arguments: ["-m", "-i", identifier], timeout: 10)
        guard result.launchError == nil, !result.timedOut else { return .unknown }
        return parseExtensionStatus(pluginkitOutput: result.stdout, identifier: identifier)
    }

    /// `pluginkit -e use -i <id>` enables the extension, as its switch in System Settings does.
    /// Returns whether the command ran and exited 0.
    @discardableResult
    public static func enableExtension(identifier: String) -> Bool {
        RightKitBoundedProcess.run(
            executable: URL(fileURLWithPath: "/usr/bin/pluginkit"), arguments: ["-e", "use", "-i", identifier],
            timeout: 10).succeeded
    }

    // MARK: Login item

    /// A login item's registration as a permission status.
    public static func status(of login: RightKitLoginItemStatus) -> RightKitPermissionStatus {
        switch login {
        case .enabled: .granted
        case .requiresApproval: .needsApproval
        case .notRegistered: .off
        case .notFound: .unknown
        }
    }

    // MARK: System Settings

    /// Opens a privacy pane, the modern URL first and the older one when that is refused.
    @MainActor
    @discardableResult
    public static func open(_ pane: RightKitPrivacyPane) -> Bool {
        for url in pane.urls where NSWorkspace.shared.open(url) { return true }
        return false
    }

    /// Opens Login Items & Extensions, falling back to the older Extensions pane id.
    @MainActor
    @discardableResult
    public static func openExtensionsPane() -> Bool {
        for scheme in [
            "x-apple.systempreferences:com.apple.ExtensionsPreferences",
            "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
        ] {
            if let url = URL(string: scheme), NSWorkspace.shared.open(url) { return true }
        }
        return false
    }

    /// Reveals `appURL` in Finder so it can be dragged into a privacy list or added with the + button
    /// (for a helper app that has its own grant).
    @MainActor
    public static func reveal(_ appURL: URL) {
        guard FileManager.default.fileExists(atPath: appURL.path) else { return }
        NSWorkspace.shared.activateFileViewerSelecting([appURL])
    }
}
#endif
