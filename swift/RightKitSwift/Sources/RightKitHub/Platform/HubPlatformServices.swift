import Foundation
#if os(Windows) && canImport(SwiftRightHost)
import SwiftRightHost
#endif

/// The state of one permission.
public enum HubPermissionStatus: String, Sendable, Equatable {
    /// The user has allowed it.
    case granted
    /// The user has refused it, or it is off in system settings.
    case denied
    /// The user has not been asked yet.
    case notDetermined
    /// The platform cannot tell. Never treat this as granted.
    case unknown
    /// This platform has no such permission or no way to ask for it.
    case unsupported
}

/// Permission ids the hub knows. The id is a plain string so a platform can add its own.
public enum HubPermission {
    public static let notifications = "notifications"
    public static let accessibility = "accessibility"
    public static let screenRecording = "screen-recording"
}

/// A user notification to post.
public struct HubNotification: Sendable, Equatable {
    /// A stable id; posting the same id again replaces the earlier notification.
    public var id: String
    public var title: String
    public var body: String

    public init(id: String = UUID().uuidString, title: String, body: String = "") {
        self.id = id
        self.title = title
        self.body = body
    }
}

/// The tray or menu-bar presence of the app.
public struct HubTrayPresence: Sendable, Equatable {
    /// An SF Symbol name for the icon.
    public var symbol: String
    public var tooltip: String

    public init(symbol: String, tooltip: String = "") {
        self.symbol = symbol
        self.tooltip = tooltip
    }
}

/// Why a platform service could not run.
public enum HubPlatformError: Error, Sendable, Equatable, CustomStringConvertible {
    /// This platform does not provide the service.
    case unsupported(String)
    /// The service exists and failed.
    case failed(String)

    public var description: String {
        switch self {
        case .unsupported(let what): return "unsupported: \(what)"
        case .failed(let what): return "failed: \(what)"
        }
    }
}

/// Everything the hub needs from the host operating system. Views never reach the system any other
/// way and never import AppKit or UIKit; the Mac implementation is `MacPlatformServices`, and
/// `UnavailablePlatformServices` answers "unsupported" for every member.
public protocol HubPlatformServices: Sendable {
    /// Shows a file or folder in the system file browser (Finder, Explorer).
    func revealFile(_ path: String) async throws
    /// Opens a URL in the default handler.
    func openURL(_ url: URL) async throws
    /// Moves a file or folder to the Trash or Recycle Bin. Never deletes permanently.
    func moveToTrash(_ path: String) async throws

    /// The current state of a permission, without asking.
    func permissionStatus(_ id: String) async -> HubPermissionStatus
    /// Asks for a permission, or opens the system page where the user can grant it, and returns the state afterwards.
    func requestPermission(_ id: String) async throws -> HubPermissionStatus

    /// The plain text on the clipboard, or nil when there is none.
    func readClipboardText() async throws -> String?
    /// Replaces the clipboard with plain text.
    func writeClipboardText(_ text: String) async throws

    /// Posts a user notification.
    func postNotification(_ notification: HubNotification) async throws

    /// Shows, updates or (with nil) removes the tray or menu-bar icon.
    func setTrayPresence(_ presence: HubTrayPresence?) async throws
    /// One value each time the tray icon is activated.
    var trayActivations: AsyncStream<Void> { get }

    /// Whether the app starts when the user logs in.
    func isLaunchAtLoginEnabled() async throws -> Bool
    /// Turns starting at login on or off.
    func setLaunchAtLogin(_ enabled: Bool) async throws

    /// Claims the single-instance slot named `id`. Returns true when this process now holds the claim,
    /// false when another process holds it (this one should quit). The claim is held until the process
    /// exits, however it exits; there is no release. Claiming the same `id` twice in one process is true.
    func claimSingleInstance(_ id: String) async throws -> Bool

    /// Asks the user to choose one existing folder and returns its absolute path, or nil when the
    /// user cancels. Throws `HubPlatformError.unsupported` where the platform has no folder chooser;
    /// a view that offers "Choose…" keeps a typed path as its fallback.
    /// - Parameters:
    ///   - prompt: One line saying what the folder is for, shown in the chooser.
    ///   - startingAt: A folder to open the chooser in, or nil for the platform's default.
    func chooseFolder(prompt: String, startingAt: String?) async throws -> String?

    /// Decodes image bytes (PNG, JPEG, GIF, WebP where the platform reads them) into an image the
    /// hub can draw, never larger than `maxPixel` on its longest side and never scaled up. Returns
    /// nil when the bytes are not an image the platform can read, or when it cannot decode at all.
    /// Views draw the result with `HubImageView`; they never decode bytes themselves.
    func decodeImage(_ data: Data, maxPixel: Int) async -> HubImage?

    /// Makes a font file's bytes (TTF, OTF, and WOFF2 where the platform reads it) usable by name
    /// for this process, and returns the name `Font.custom` takes (the PostScript name on Apple
    /// platforms). Registering the same font again returns the same name. Throws when the bytes are
    /// not a font the platform can read; the caller then keeps its fallback face.
    func registerFont(_ data: Data) throws -> String
}

/// Default answers, so a conformer written before these three members existed still compiles: no
/// folder chooser, no image decoder, no font registration.
public extension HubPlatformServices {
    func chooseFolder(prompt: String, startingAt: String?) async throws -> String? {
        throw HubPlatformError.unsupported("chooseFolder")
    }

    func decodeImage(_ data: Data, maxPixel: Int) async -> HubImage? {
        #if os(Windows) && canImport(SwiftRightHost)
        // SwiftRight's host decoder (PNG/ICO in Swift, or WIC with SWIFTRIGHT_IMAGE_DECODER=system).
        return (try? ImageDecoder.decode(data, maxPixel: maxPixel)).map(HubImage.init(rgba:))
        #else
        return nil
        #endif
    }

    func registerFont(_ data: Data) throws -> String {
        throw HubPlatformError.unsupported("registerFont")
    }
}
