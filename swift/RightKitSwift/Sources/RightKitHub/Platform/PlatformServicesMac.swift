#if os(macOS)
import AppKit
import ApplicationServices
import CoreGraphics
import CoreText
import Darwin
import Foundation
import ImageIO
import ServiceManagement
import UserNotifications

/// The macOS implementation of `HubPlatformServices`. This file is the only place in the hub that
/// imports AppKit; views never do.
public final class MacPlatformServices: HubPlatformServices {
    private let activations: AsyncStream<Void>
    private let activationSignal: AsyncStream<Void>.Continuation
    private let claims = MacInstanceClaims()

    public init() {
        let pair = AsyncStream<Void>.makeStream()
        activations = pair.stream
        activationSignal = pair.continuation
    }

    public func revealFile(_ path: String) async throws {
        let url = URL(fileURLWithPath: path)
        await MainActor.run {
            NSWorkspace.shared.activateFileViewerSelecting([url])
        }
    }

    /// Opens a URL in the default handler. Only `http`, `https` and `mailto` are opened; any other
    /// scheme (file, custom app schemes, javascript) is refused.
    public func openURL(_ url: URL) async throws {
        let allowed: Set<String> = ["http", "https", "mailto"]
        guard let scheme = url.scheme?.lowercased(), allowed.contains(scheme) else {
            throw HubPlatformError.unsupported("openURL scheme \(url.scheme ?? "none")")
        }
        let opened = await MainActor.run { NSWorkspace.shared.open(url) }
        if !opened { throw HubPlatformError.failed("no handler opened \(url.absoluteString)") }
    }

    /// Moves a file or folder to the Trash. The path must be absolute and already standardized
    /// (no `.`, `..`, doubled or trailing slashes); empty, relative and root paths are refused.
    public func moveToTrash(_ path: String) async throws {
        let url = URL(fileURLWithPath: path)
        guard !path.isEmpty, path.hasPrefix("/"), path != "/", url.standardized.path == path else {
            throw HubPlatformError.failed("moveToTrash needs an absolute, standardized path, got \"\(path)\"")
        }
        do {
            try FileManager.default.trashItem(at: url, resultingItemURL: nil)
        } catch {
            throw HubPlatformError.failed("trash \(path): \(error.localizedDescription)")
        }
    }

    public func permissionStatus(_ id: String) async -> HubPermissionStatus {
        switch id {
        case HubPermission.notifications:
            let settings = await UNUserNotificationCenter.current().notificationSettings()
            switch settings.authorizationStatus {
            case .authorized, .provisional: return .granted
            case .denied: return .denied
            case .notDetermined: return .notDetermined
            default: return .unknown
            }
        case HubPermission.accessibility:
            // The system cannot say "not asked yet"; untrusted covers both.
            return AXIsProcessTrusted() ? .granted : .denied
        case HubPermission.screenRecording:
            return CGPreflightScreenCaptureAccess() ? .granted : .denied
        default:
            return .unsupported
        }
    }

    public func requestPermission(_ id: String) async throws -> HubPermissionStatus {
        switch id {
        case HubPermission.notifications:
            do {
                _ = try await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
            } catch {
                throw HubPlatformError.failed("notification authorization: \(error.localizedDescription)")
            }
        case HubPermission.accessibility:
            try await openSettingsPage("Privacy_Accessibility")
        case HubPermission.screenRecording:
            _ = CGRequestScreenCaptureAccess()
        default:
            throw HubPlatformError.unsupported("permission \(id)")
        }
        return await permissionStatus(id)
    }

    public func readClipboardText() async throws -> String? {
        await MainActor.run { NSPasteboard.general.string(forType: .string) }
    }

    public func writeClipboardText(_ text: String) async throws {
        await MainActor.run {
            NSPasteboard.general.clearContents()
            _ = NSPasteboard.general.setString(text, forType: .string)
        }
    }

    public func postNotification(_ notification: HubNotification) async throws {
        let content = UNMutableNotificationContent()
        content.title = notification.title
        content.body = notification.body
        let request = UNNotificationRequest(identifier: notification.id, content: content, trigger: nil)
        do {
            try await UNUserNotificationCenter.current().add(request)
        } catch {
            throw HubPlatformError.failed("notification: \(error.localizedDescription)")
        }
    }

    public func setTrayPresence(_ presence: HubTrayPresence?) async throws {
        let signal = activationSignal
        await MainActor.run {
            MacTrayController.shared.apply(presence) { signal.yield() }
        }
    }

    public var trayActivations: AsyncStream<Void> { activations }

    public func isLaunchAtLoginEnabled() async throws -> Bool {
        SMAppService.mainApp.status == .enabled
    }

    public func setLaunchAtLogin(_ enabled: Bool) async throws {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try await SMAppService.mainApp.unregister()
            }
        } catch {
            throw HubPlatformError.failed("launch at login: \(error.localizedDescription)")
        }
    }

    /// Takes an exclusive, non-blocking `flock` on a lock file named by `id` in the user's Caches
    /// folder (`RightKitHub/<id>.lock`) and keeps the descriptor open for the life of the process.
    /// The kernel drops the lock when the process exits or crashes, so a stale claim cannot outlive
    /// its owner. Returns false when another process holds it; the running app is then brought
    /// forward when `id` is its bundle identifier (best effort).
    public func claimSingleInstance(_ id: String) async throws -> Bool {
        let name = String(id.map { $0.isLetter || $0.isNumber || $0 == "." || $0 == "-" || $0 == "_" ? $0 : "_" })
        guard !name.isEmpty else { throw HubPlatformError.failed("claimSingleInstance needs an id") }
        if claims.holds(name) { return true }
        guard let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first else {
            throw HubPlatformError.failed("no Caches folder for the single-instance lock")
        }
        let folder = caches.appendingPathComponent("RightKitHub", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        } catch {
            throw HubPlatformError.failed("single-instance lock folder: \(error.localizedDescription)")
        }
        let path = folder.appendingPathComponent("\(name).lock").path
        let descriptor = Darwin.open(path, O_CREAT | O_RDWR, 0o600)
        guard descriptor >= 0 else { throw HubPlatformError.failed("single-instance lock file \(path)") }
        if flock(descriptor, LOCK_EX | LOCK_NB) == 0 {
            claims.keep(name, descriptor)
            return true
        }
        let busy = errno == EWOULDBLOCK
        close(descriptor)
        guard busy else { throw HubPlatformError.failed("single-instance lock: errno \(errno)") }
        await MainActor.run {
            let me = ProcessInfo.processInfo.processIdentifier
            let other = NSRunningApplication.runningApplications(withBundleIdentifier: id)
                .first { $0.processIdentifier != me }
            _ = other?.activate()
        }
        return false
    }

    /// Shows an open panel that chooses one folder, app-modal, on the main actor. Returns the chosen
    /// folder's path, or nil when the user cancels. `startingAt` opens the panel there when it is an
    /// existing folder. The panel is shown only because the user pressed a button; nothing calls this
    /// on its own.
    public func chooseFolder(prompt: String, startingAt: String?) async throws -> String? {
        await MainActor.run { () -> String? in
            let panel = NSOpenPanel()
            panel.canChooseDirectories = true
            panel.canChooseFiles = false
            panel.allowsMultipleSelection = false
            panel.canCreateDirectories = true
            panel.resolvesAliases = true
            panel.message = prompt
            panel.prompt = "Choose"
            if let startingAt, !startingAt.isEmpty {
                var isFolder: ObjCBool = false
                if FileManager.default.fileExists(atPath: startingAt, isDirectory: &isFolder), isFolder.boolValue {
                    panel.directoryURL = URL(fileURLWithPath: startingAt, isDirectory: true)
                }
            }
            NSApp.activate()
            guard panel.runModal() == .OK, let url = panel.url else { return nil }
            return url.path
        }
    }

    /// Decodes with ImageIO, off the main thread: a thumbnail no larger than `maxPixel` on its longest
    /// side (a smaller image keeps its own size; nothing is scaled up), turned upright by its EXIF
    /// orientation, and fully decoded here so drawing it later costs nothing. The first frame of an
    /// animated image. Returns nil for bytes ImageIO cannot read.
    public func decodeImage(_ data: Data, maxPixel: Int) async -> HubImage? {
        guard !data.isEmpty, maxPixel > 0, let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        var limit = min(maxPixel, 8192)
        if let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
            let width = properties[kCGImagePropertyPixelWidth] as? Int,
            let height = properties[kCGImagePropertyPixelHeight] as? Int,
            width > 0, height > 0
        {
            limit = min(limit, max(width, height))
        }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: limit,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        return HubImage(cgImage: image)
    }

    /// Registers the font for this process with CoreText (`CTFontManagerRegisterGraphicsFont`) and
    /// returns its PostScript name. CoreText reads TTF and OTF, and WOFF2 on the macOS versions that
    /// support it; where it does not, this throws and the caller keeps its fallback face. A font
    /// already registered under the same name is not an error.
    public func registerFont(_ data: Data) throws -> String {
        guard !data.isEmpty, let provider = CGDataProvider(data: data as CFData), let font = CGFont(provider),
            let name = font.postScriptName as String?, !name.isEmpty
        else {
            throw HubPlatformError.failed("registerFont: CoreText cannot read these bytes as a font")
        }
        var error: Unmanaged<CFError>?
        if CTFontManagerRegisterGraphicsFont(font, &error) { return name }
        let failure = error?.takeRetainedValue()
        if let failure, CFErrorGetCode(failure) == CTFontManagerError.alreadyRegistered.rawValue {
            return name
        }
        let reason = failure.map { CFErrorCopyDescription($0) as String } ?? "unknown error"
        throw HubPlatformError.failed("registerFont \(name): \(reason)")
    }

    private func openSettingsPage(_ anchor: String) async throws {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)") else {
            throw HubPlatformError.failed("settings url for \(anchor)")
        }
        try await openURL(url)
    }
}

/// The single-instance lock descriptors this process holds. They are never closed: the claim lasts
/// until the process exits.
private final class MacInstanceClaims: @unchecked Sendable {
    private let lock = NSLock()
    private var held: [String: Int32] = [:]

    func holds(_ name: String) -> Bool {
        lock.withLock { held[name] != nil }
    }

    func keep(_ name: String, _ descriptor: Int32) {
        lock.withLock { held[name] = descriptor }
    }
}

/// Owns the status item. Main-actor only; the service reaches it through `MainActor.run`.
@MainActor
private final class MacTrayController: NSObject {
    static let shared = MacTrayController()

    private var item: NSStatusItem?
    private var onActivate: (@Sendable () -> Void)?

    func apply(_ presence: HubTrayPresence?, onActivate: @escaping @Sendable () -> Void) {
        self.onActivate = onActivate
        guard let presence else {
            if let item { NSStatusBar.system.removeStatusItem(item) }
            item = nil
            return
        }
        let statusItem = item ?? NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item = statusItem
        if let button = statusItem.button {
            button.image = NSImage(systemSymbolName: presence.symbol, accessibilityDescription: presence.tooltip)
            button.toolTip = presence.tooltip
            button.target = self
            button.action = #selector(activate(_:))
        }
    }

    @objc private func activate(_ sender: Any?) {
        onActivate?()
    }
}
#endif
