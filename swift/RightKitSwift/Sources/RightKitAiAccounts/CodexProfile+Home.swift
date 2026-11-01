import RightKitNotchCore
import RightKitNotch
import Foundation

/// The Mac-only lookups `CodexProfile` needs: the real home folder, the account home it is shown
/// relative to, and the no-argument entry points that use them. Kept out of CodexProfile.swift so
/// that file (in the shared face) takes its home as a parameter.
extension CodexProfile {
    static var homeDirectory: URL { URL(fileURLWithPath: Runtime.realHomePath("the Codex profile search")) }

    static func `default`() -> CodexProfile { `default`(home: homeDirectory) }

    static func discover(fileManager: FileManager = .default) -> [CodexProfile] {
        discover(home: homeDirectory, fileManager: fileManager)
    }

    var displayPath: String {
        let home = AccountHome.path
        let path = configDirectory.path
        return path.hasPrefix(home + "/") ? "~" + path.dropFirst(home.count) : path
    }

    var sourceName: String { slug == nil ? "Codex" : "Codex in \(displayPath)" }
}
