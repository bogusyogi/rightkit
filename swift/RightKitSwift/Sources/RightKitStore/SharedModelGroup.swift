import Foundation

public enum SharedModelGroupError: Error, Equatable, Sendable {
    /// The App Group container is unavailable: the entitlement is missing or the group ID is not allowed.
    case containerUnavailable(String)
    /// The directory name is empty, has a path separator, or is a relative escape such as `..`.
    case invalidName(String)
}

/// The App Group that holds the Right Suite's shared models (rightkit#762, ScrapeRight port).
///
/// Apps read and write the shared model stores through this helper, so every app uses the same group ID and
/// the same directory rules. The group must be listed in each app's entitlements. Nothing here shares keychain
/// items: credentials stay with their creating binary.
public enum SharedModelGroup {
    /// The suite's shared models group.
    public static let identifier = "group.app.rightsuite.models"

    /// The shared container, or `nil` when the entitlement is missing.
    public static func containerURL(fileManager: FileManager = .default) -> URL? {
        fileManager.containerURL(forSecurityApplicationGroupIdentifier: identifier)
    }

    /// Shared defaults for the group, or `nil` when the group is not available.
    public static func defaults() -> UserDefaults? {
        UserDefaults(suiteName: identifier)
    }

    /// A named directory inside the shared container, created if absent.
    public static func directory(named name: String, fileManager: FileManager = .default) throws -> URL {
        guard isValidName(name) else { throw SharedModelGroupError.invalidName(name) }
        guard let root = containerURL(fileManager: fileManager) else {
            throw SharedModelGroupError.containerUnavailable(identifier)
        }
        let directory = root.appendingPathComponent(name, isDirectory: true)
        try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }

    static func isValidName(_ name: String) -> Bool {
        !name.isEmpty
            && !name.hasPrefix(".")
            && !name.contains("/")
            && !name.contains("\\")
            && !name.contains("\0")
            && name.count <= 64
    }
}
