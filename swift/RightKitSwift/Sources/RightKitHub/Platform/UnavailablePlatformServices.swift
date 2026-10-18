import Foundation

/// Platform services for a host that provides none: previews, tests and platforms not yet ported.
/// Every action throws `HubPlatformError.unsupported`; reads return "unsupported".
public struct UnavailablePlatformServices: HubPlatformServices {
    public init() {}

    public func revealFile(_ path: String) async throws {
        throw HubPlatformError.unsupported("revealFile")
    }

    public func openURL(_ url: URL) async throws {
        throw HubPlatformError.unsupported("openURL")
    }

    public func moveToTrash(_ path: String) async throws {
        throw HubPlatformError.unsupported("moveToTrash")
    }

    public func permissionStatus(_ id: String) async -> HubPermissionStatus {
        .unsupported
    }

    public func requestPermission(_ id: String) async throws -> HubPermissionStatus {
        throw HubPlatformError.unsupported("requestPermission")
    }

    public func readClipboardText() async throws -> String? {
        throw HubPlatformError.unsupported("readClipboardText")
    }

    public func writeClipboardText(_ text: String) async throws {
        throw HubPlatformError.unsupported("writeClipboardText")
    }

    public func postNotification(_ notification: HubNotification) async throws {
        throw HubPlatformError.unsupported("postNotification")
    }

    public func setTrayPresence(_ presence: HubTrayPresence?) async throws {
        throw HubPlatformError.unsupported("setTrayPresence")
    }

    public var trayActivations: AsyncStream<Void> {
        AsyncStream<Void> { continuation in
            continuation.finish()
        }
    }

    public func isLaunchAtLoginEnabled() async throws -> Bool {
        throw HubPlatformError.unsupported("isLaunchAtLoginEnabled")
    }

    public func setLaunchAtLogin(_ enabled: Bool) async throws {
        throw HubPlatformError.unsupported("setLaunchAtLogin")
    }

    public func claimSingleInstance(_ id: String) async throws -> Bool {
        throw HubPlatformError.unsupported("claimSingleInstance")
    }

    public func chooseFolder(prompt: String, startingAt: String?) async throws -> String? {
        throw HubPlatformError.unsupported("chooseFolder")
    }

    public func decodeImage(_ data: Data, maxPixel: Int) async -> HubImage? {
        nil
    }

    public func registerFont(_ data: Data) throws -> String {
        throw HubPlatformError.unsupported("registerFont")
    }
}
