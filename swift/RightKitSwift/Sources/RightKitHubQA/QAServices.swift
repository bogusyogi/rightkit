import Foundation
import RightKitHub
#if os(Windows) && canImport(SwiftRightHost)
import SwiftRightHost
#endif

/// Platform services that do nothing real: every call is recorded and answered from fields you set.
/// For QA runs that must not touch the user's desktop, files or clipboard.
public final class RecordingPlatformServices: HubPlatformServices, @unchecked Sendable {
    private let lock = NSLock()
    private var log: [String] = []
    private var clipboard: String?
    private var permissions: [String: HubPermissionStatus]
    private var launchAtLogin = false
    private let activations: AsyncStream<Void>
    private let activationSignal: AsyncStream<Void>.Continuation

    private let chosenFolder: String?
    #if os(macOS)
    /// Decoding an image and registering a font touch neither the desktop, nor the user's files, nor
    /// the clipboard, and a picture needs them to be real, so these two calls go to the Mac services.
    private let mac = MacPlatformServices()
    #endif

    /// - Parameters:
    ///   - permissions: The status to report per permission id; others report `.unsupported`.
    ///   - chosenFolder: The path `chooseFolder` answers without showing anything; nil answers as if
    ///     the user cancelled.
    public init(permissions: [String: HubPermissionStatus] = [:], chosenFolder: String? = "/Users/demo/Chosen") {
        self.permissions = permissions
        self.chosenFolder = chosenFolder
        let pair = AsyncStream<Void>.makeStream()
        activations = pair.stream
        activationSignal = pair.continuation
    }

    /// What was called, in order, for example `revealFile /tmp/a`.
    public func recordedCalls() -> [String] {
        lock.withLock { log }
    }

    /// Simulates a click on the tray icon.
    public func activateTray() {
        activationSignal.yield()
    }

    private func record(_ entry: String) {
        lock.withLock { log.append(entry) }
    }

    public func revealFile(_ path: String) async throws { record("revealFile \(path)") }
    public func openURL(_ url: URL) async throws { record("openURL \(url.absoluteString)") }
    public func moveToTrash(_ path: String) async throws { record("moveToTrash \(path)") }

    public func permissionStatus(_ id: String) async -> HubPermissionStatus {
        lock.withLock { permissions[id] ?? .unsupported }
    }

    public func requestPermission(_ id: String) async throws -> HubPermissionStatus {
        record("requestPermission \(id)")
        return lock.withLock { permissions[id] ?? .unsupported }
    }

    public func readClipboardText() async throws -> String? {
        lock.withLock { clipboard }
    }

    public func writeClipboardText(_ text: String) async throws {
        record("writeClipboardText")
        lock.withLock { clipboard = text }
    }

    public func postNotification(_ notification: HubNotification) async throws {
        record("postNotification \(notification.title)")
    }

    public func setTrayPresence(_ presence: HubTrayPresence?) async throws {
        record("setTrayPresence \(presence?.symbol ?? "none")")
    }

    public var trayActivations: AsyncStream<Void> { activations }

    public func isLaunchAtLoginEnabled() async throws -> Bool {
        lock.withLock { launchAtLogin }
    }

    public func setLaunchAtLogin(_ enabled: Bool) async throws {
        record("setLaunchAtLogin \(enabled)")
        lock.withLock { launchAtLogin = enabled }
    }

    public func claimSingleInstance(_ id: String) async throws -> Bool {
        record("claimSingleInstance \(id)")
        return true
    }

    public func chooseFolder(prompt: String, startingAt: String?) async throws -> String? {
        record("chooseFolder \(prompt) from \(startingAt ?? "default")")
        return chosenFolder
    }

    public func decodeImage(_ data: Data, maxPixel: Int) async -> HubImage? {
        record("decodeImage \(data.count) bytes max \(maxPixel)")
        #if os(macOS)
        return await mac.decodeImage(data, maxPixel: maxPixel)
        #elseif os(Windows) && canImport(SwiftRightHost)
        // The same decoder as the protocol default: an override that answered nil here hid it, so
        // every Windows render drew the neutral glyph (found by SwiftRight, f203e7a12).
        return (try? ImageDecoder.decode(data, maxPixel: maxPixel)).map(HubImage.init(rgba:))
        #else
        return nil
        #endif
    }

    public func registerFont(_ data: Data) throws -> String {
        record("registerFont \(data.count) bytes")
        #if os(macOS)
        return try mac.registerFont(data)
        #else
        throw HubPlatformError.unsupported("registerFont")
        #endif
    }
}

/// A settings store held in memory, with change notification, for QA runs and previews.
public final class InMemorySettingsStore: HubSettingsStore, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: HubSettingValue]
    private var watchers: [UUID: AsyncStream<[String]>.Continuation] = [:]

    public init(values: [String: HubSettingValue] = [:]) {
        self.values = values
    }

    public func get(_ keys: [String]) async throws -> [String: HubSettingValue] {
        lock.withLock { () -> [String: HubSettingValue] in
            if keys.isEmpty { return values }
            return values.filter { keys.contains($0.key) }
        }
    }

    public func set(_ key: String, _ value: HubSettingValue) async throws {
        let targets = lock.withLock { () -> [AsyncStream<[String]>.Continuation] in
            values[key] = value
            return Array(watchers.values)
        }
        for target in targets {
            target.yield([key])
        }
    }

    public func observe() -> AsyncStream<[String]> {
        AsyncStream<[String]> { continuation in
            let id = UUID()
            lock.withLock { watchers[id] = continuation }
            continuation.onTermination = { [weak self] _ in
                self?.removeWatcher(id)
            }
        }
    }

    private func removeWatcher(_ id: UUID) {
        lock.withLock { watchers[id] = nil }
    }
}
