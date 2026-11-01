import Foundation

/// A desktop workspace path remembered by CodeRight. Paths are opaque strings: the phone
/// never attempts to inspect or browse the desktop filesystem.
public struct WorkspaceBookmark: Codable, Equatable, Identifiable, Sendable {
    public let root: String
    public var name: String
    public var lastUsed: Date

    public var id: String { root }

    public init(root: String, name: String = "", lastUsed: Date = Date()) {
        self.root = Self.normalizeRoot(root)
        self.name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        self.lastUsed = lastUsed
    }

    public var displayName: String {
        if !name.isEmpty { return name }
        if let component = root.split(separator: "/").last, !component.isEmpty {
            return String(component)
        }
        return root
    }

    /// Keep identity stable for the common `/repo/` versus `/repo` spelling without
    /// resolving symlinks or touching a path that exists only on the desktop.
    public static func normalizeRoot(_ raw: String) -> String {
        var value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while value.count > 1 && value.hasSuffix("/") { value.removeLast() }
        return value
    }
}

/// Pure mapping used by new-session creation and unit tests.
public struct WorkspaceSessionDraft: Equatable, Sendable {
    public let title: String
    public let workspaceRoot: String

    public init?(title: String, workspaceRoot: String) {
        let root = WorkspaceBookmark.normalizeRoot(workspaceRoot)
        guard !root.isEmpty else { return nil }
        let trimmedTitle = title.trimmingCharacters(in: .whitespacesAndNewlines)
        self.title = trimmedTitle.isEmpty ? "Session from iPhone" : trimmedTitle
        self.workspaceRoot = root
    }
}
