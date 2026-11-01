#if os(macOS)
import Foundation
import RightKitHub

/// The outcome of `Hub.registerFonts(services:)`.
public struct FontRegistration: Sendable, Equatable {
    /// The name `Font.custom` takes for Tanker, or nil when it could not be registered (the hub then
    /// draws titles in the system's heavy condensed face).
    public let tankerName: String?
    /// Why Tanker is not available ("unavailable: ..." when no bundle holds the file), or nil when it is.
    public let problem: String?

    public var tankerRegistered: Bool { tankerName != nil }
}

/// Whether Tanker is registered in this process, read by the theme and the footer wordmark.
final class FontState: @unchecked Sendable {
    static let shared = FontState()
    private let lock = NSLock()
    private var name: String?

    var tankerName: String? { lock.withLock { name } }

    func setTankerName(_ value: String) { lock.withLock { name = value } }
}

/// Files bundled with Hub (`Resources/`, declared in Package.swift).
///
/// SwiftPM's generated `Bundle.module` stops the process (a `fatalError`) when its resource bundle is
/// not where it looks, and there is no Windows story for that bundle yet. So the lookup first checks
/// that `PulseHub_PulseHub.bundle` exists beside the app or executable, and only then reads
/// `Bundle.module` (the Mac behaviour: the bundle sits in the app's Resources or next to the
/// executable). Without it, the file is looked up in `Bundle.main`; failing that the resource is
/// unavailable and the caller falls back.
enum Resources {
    /// The name SwiftPM gives the target's resource bundle: `<package>_<target>.bundle`.
    private static let bundleName = "PulseHub_PulseHub.bundle"

    /// Whether the resource bundle is in a place `Bundle.module` looks first, so touching it cannot stop the process.
    private static var moduleBundlePresent: Bool {
        var folders: [URL] = [Bundle.main.bundleURL]
        if let resources = Bundle.main.resourceURL { folders.append(resources) }
        if let executable = CommandLine.arguments.first, !executable.isEmpty {
            folders.append(URL(fileURLWithPath: executable).deletingLastPathComponent())
        }
        return folders.contains { FileManager.default.fileExists(atPath: $0.appendingPathComponent(bundleName).path) }
    }

    private static func read(_ name: String, _ ext: String, in bundle: Bundle) -> Data? {
        let url = bundle.url(forResource: name, withExtension: ext)
            ?? bundle.url(forResource: name, withExtension: ext, subdirectory: "Resources")
        return url.flatMap { try? Data(contentsOf: $0) }
    }

    /// The file's bytes, or nil when no bundle holds it.
    static func data(_ name: String, _ ext: String) -> Data? {
        if moduleBundlePresent, let data = read(name, ext, in: Bundle.module) { return data }
        return read(name, ext, in: Bundle.main)
    }
}

extension Hub {
    /// Registers Tanker (the face of page titles and the wordmark; `Resources/Tanker-400.woff2`, the
    /// file the Tauri hub ships) through the platform services. Call it once at startup, before the
    /// first `HubContext` is built from `Hub.theme` and before any view is drawn: the pulse-hub-render
    /// tool and the notch host both do. Calling it again after success returns the same result; after a
    /// failure it tries again.
    ///
    /// Tanker's name table, as the file ships, names the family, full name and PostScript name
    /// "false", so that is the name `Font.custom` takes; the registration result carries whatever name
    /// the platform reports. When registration fails, `Hub.theme` and `SidebarFooter` keep the
    /// system condensed fallback, and `problem` says why.
    @discardableResult
    public static func registerFonts(services: any HubPlatformServices) -> FontRegistration {
        if let name = FontState.shared.tankerName {
            return FontRegistration(tankerName: name, problem: nil)
        }
        guard let data = Resources.data("Tanker-400", "woff2") else {
            return FontRegistration(
                tankerName: nil,
                problem: "unavailable: Tanker-400.woff2 is in neither the PulseHub resource bundle nor the main bundle")
        }
        do {
            let name = try services.registerFont(data)
            FontState.shared.setTankerName(name)
            return FontRegistration(tankerName: name, problem: nil)
        } catch {
            return FontRegistration(tankerName: nil, problem: "\(error)")
        }
    }

    /// The page-title style: Tanker at 18 (`.rk-top__title` in the Tauri hub), which draws capitals
    /// itself, so the shell does not uppercase. Tracking (`letter-spacing: 0.01em` there) is not available on
    /// Windows and is skipped. Without Tanker: the system's heavy condensed face, in capitals.
    static var titleStyle: HubTitleStyle {
        if let name = FontState.shared.tankerName {
            return HubTitleStyle(size: 18, weight: .regular, fontName: name)
        }
        return HubTitleStyle(size: 18, weight: .heavy, condensed: true, uppercase: true)
    }

    /// The sidebar wordmark style: Tanker at 20 (`.wordmark`), or the same fallback.
    static var wordmarkStyle: HubTitleStyle {
        if let name = FontState.shared.tankerName {
            return HubTitleStyle(size: 20, weight: .regular, fontName: name)
        }
        return HubTitleStyle(size: 20, weight: .heavy, condensed: true, uppercase: true)
    }
}
#endif
