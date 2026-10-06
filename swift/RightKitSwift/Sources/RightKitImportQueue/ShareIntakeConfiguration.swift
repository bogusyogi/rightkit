import Foundation
import RightKitStore

/// Everything an app injects to bind share intake to its App Group.
public struct ShareIntakeConfiguration: @unchecked Sendable {
    public var groupID: String
    public var queueDirectoryName: String
    public var stagingDirectoryName: String
    /// Replaces the App Group container as the storage root (tests; unentitled tools).
    public var rootOverride: URL?
    public var defaultsOverride: UserDefaults?
    /// Pre-queue `UserDefaults` blobs to migrate into the file queue. Empty for new apps.
    public var legacyQueueKey: String?
    public var legacyPayloadKey: String?
    /// File bytes are always staged on disk, never read into memory; this bounds container storage.
    public var maxFileBytes: Int
    /// Text is the only content materialised in the extension's memory budget, so it is capped hard.
    public var maxTextCharacters: Int

    public init(
        groupID: String,
        queueDirectoryName: String = "PendingShareImports",
        stagingDirectoryName: String = "SharedImport",
        rootOverride: URL? = nil,
        defaultsOverride: UserDefaults? = nil,
        legacyQueueKey: String? = nil,
        legacyPayloadKey: String? = nil,
        maxFileBytes: Int = 200 * 1024 * 1024,
        maxTextCharacters: Int = 200_000
    ) {
        self.groupID = groupID
        self.queueDirectoryName = queueDirectoryName
        self.stagingDirectoryName = stagingDirectoryName
        self.rootOverride = rootOverride
        self.defaultsOverride = defaultsOverride
        self.legacyQueueKey = legacyQueueKey
        self.legacyPayloadKey = legacyPayloadKey
        self.maxFileBytes = maxFileBytes
        self.maxTextCharacters = maxTextCharacters
    }

    var containerRoot: URL? {
        rootOverride ?? FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: groupID)
    }

    var defaults: UserDefaults? { defaultsOverride ?? UserDefaults(suiteName: groupID) }
}
