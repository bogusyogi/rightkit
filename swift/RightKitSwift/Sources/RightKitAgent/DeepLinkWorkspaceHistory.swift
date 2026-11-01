import Foundation

/// Persistent workspace record used by the AppRouter route model. Holds no
/// credential: workspace tokens are not stored here.
public struct DeepLinkWorkspaceEntry: Codable, Identifiable, Sendable, Equatable {
    public let workspaceId: String
    public var name: String
    public var lastUsed: Date
    public var appURL: String?
    public var apiURL: String?

    public var id: String { workspaceId }

    public init(
        workspaceId: String,
        name: String,
        lastUsed: Date = Date(),
        appURL: String? = nil,
        apiURL: String? = nil
    ) {
        self.workspaceId = workspaceId
        self.name = name
        self.lastUsed = lastUsed
        self.appURL = appURL
        self.apiURL = apiURL
    }

    public var displayName: String {
        if !name.isEmpty && name != workspaceId {
            return name
        }
        return String(workspaceId.prefix(8))
    }
}

@MainActor
public final class DeepLinkWorkspaceHistory {
    public static let shared = DeepLinkWorkspaceHistory()

    private let defaultsKey = "coderight.deepLinks.workspaceHistory"
    private let currentKey = "coderight.deepLinks.currentWorkspace"
    private let maxEntries = 10

    public init() {}

    public func entries() -> [DeepLinkWorkspaceEntry] {
        guard let data = UserDefaults.standard.data(forKey: defaultsKey),
              let decoded = try? JSONDecoder().decode(
                [DeepLinkWorkspaceEntry].self,
                from: data
              ) else {
            return []
        }
        return decoded.sorted { $0.lastUsed > $1.lastUsed }
    }

    public func touch(
        workspaceId: String,
        name: String,
        appURL: String? = nil,
        apiURL: String? = nil
    ) {
        let existing = entries().first { $0.workspaceId == workspaceId }
        var all = entries().filter { $0.workspaceId != workspaceId }
        all.insert(
            DeepLinkWorkspaceEntry(
                workspaceId: workspaceId,
                name: name,
                lastUsed: Date(),
                appURL: appURL ?? existing?.appURL,
                apiURL: apiURL ?? existing?.apiURL
            ),
            at: 0
        )
        let trimmed = Array(all.prefix(maxEntries))
        if let data = try? JSONEncoder().encode(trimmed) {
            UserDefaults.standard.set(data, forKey: defaultsKey)
        }
    }

    public func current() -> DeepLinkWorkspaceEntry? {
        guard let data = UserDefaults.standard.data(forKey: currentKey),
              let decoded = try? JSONDecoder().decode(
                DeepLinkWorkspaceEntry.self,
                from: data
              ) else {
            return nil
        }
        return decoded
    }

    public func setCurrent(_ entry: DeepLinkWorkspaceEntry?) {
        if let entry, let data = try? JSONEncoder().encode(entry) {
            UserDefaults.standard.set(data, forKey: currentKey)
        } else {
            UserDefaults.standard.removeObject(forKey: currentKey)
        }
    }
}
