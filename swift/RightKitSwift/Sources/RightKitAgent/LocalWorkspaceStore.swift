import Foundation
import Observation

/// A working tree living in this app's own container.
public struct LocalWorkspace: Codable, Identifiable, Equatable, Sendable {
    public var id: String
    public var name: String
    /// Path relative to the workspaces root. Stored relative, not absolute,
    /// because iOS rewrites the container path on reinstall and on some
    /// restores — an absolute path recorded today can be wrong tomorrow.
    public var relativePath: String
    public var createdAt: Date
    /// Original import label or Git remote. Informational only; runtime always
    /// uses app-owned `relativePath`.
    public var source: String?

    public init(
        id: String = UUID().uuidString,
        name: String,
        relativePath: String,
        createdAt: Date = Date(),
        source: String? = nil
    ) {
        self.id = id
        self.name = name
        self.relativePath = relativePath
        self.createdAt = createdAt
        self.source = source
    }
}

/// Creates and lists on-device working trees.
///
/// Trees live under `<app group container>/Orthic Labs/CodeRight/Workspaces`,
/// the layout RightKitSwift's `RightKitAppRoot` owns (see
/// `SharedWorkspacesLocation`), not in Files or iCloud directly: an agent
/// performs thousands of small reads and writes, and coordinated-access or
/// bookmark-scoped I/O is too slow and too failure-prone for that. Import and
/// export to Files stay deliberate user actions rather than the substrate.
/// The File Provider extension resolves the same location through the same
/// `SharedWorkspacesLocation` (that file is compiled into both targets), so the
/// two cannot disagree and Files sees these trees without a copy step.
///
/// There is no fallback location: if the App Group container cannot be
/// resolved the store reports `isRootAvailable == false` with `lastError`
/// set, and every operation fails closed.
@MainActor
@Observable
public final class LocalWorkspaceStore {
    private static let defaultsKey = "app.coderight.localWorkspaces"

    /// Never creatable: used as `root` only when the shared container is unavailable, so a
    /// misconfigured build fails every file operation instead of writing somewhere other
    /// processes cannot see.
    private static let unavailableRoot = URL(fileURLWithPath: "/dev/null/coderight-workspaces-unavailable", isDirectory: true)

    public private(set) var workspaces: [LocalWorkspace] = []
    public var lastError: String?
    /// Directory holding every working tree.
    public let root: URL
    /// False when the shared App Group container could not be resolved.
    public let isRootAvailable: Bool

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        let resolved: URL?
        let failure: String?
        do {
            resolved = try SharedWorkspacesLocation.workspacesRoot()
            failure = nil
        } catch {
            resolved = nil
            failure = "Shared workspace storage is unavailable: \(error)"
        }
        root = resolved ?? Self.unavailableRoot
        isRootAvailable = resolved != nil
        lastError = failure
        load()
    }

    public func url(for workspace: LocalWorkspace) -> URL {
        root.appendingPathComponent(workspace.relativePath, isDirectory: true)
    }

    private func load() {
        guard let data = defaults.data(forKey: Self.defaultsKey),
              let decoded = try? JSONDecoder().decode([LocalWorkspace].self, from: data)
        else { return }
        workspaces = decoded
    }

    private func persist() {
        guard let data = try? JSONEncoder().encode(workspaces) else { return }
        defaults.set(data, forKey: Self.defaultsKey)
    }

    /// Create an empty working tree. Returns nil and sets `lastError` on failure
    /// rather than throwing, because every caller is a SwiftUI button.
    @discardableResult
    public func create(named name: String) -> LocalWorkspace? {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            lastError = "Give the workspace a name."
            return nil
        }

        // The folder name is derived, never the raw input: a name with `/` or
        // `..` in it must not be able to place a directory outside the root.
        let slug = trimmed
            .replacingOccurrences(of: "[^A-Za-z0-9._-]", with: "-", options: .regularExpression)
            .trimmingCharacters(in: CharacterSet(charactersIn: "-."))
        let folder = slug.isEmpty ? UUID().uuidString : "\(slug)-\(UUID().uuidString.prefix(8))"

        let target = root.appendingPathComponent(folder, isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
            try excludeFromBackup(target)
        } catch {
            lastError = "Could not create the workspace: \(error.localizedDescription)"
            return nil
        }

        let workspace = LocalWorkspace(name: trimmed, relativePath: folder)
        workspaces.append(workspace)
        persist()
        return workspace
    }

    /// Copy a Files-selected folder into app-owned storage. Security-scoped
    /// access is opened by caller for duration of this synchronous copy.
    @discardableResult
    public func importFolder(_ sourceURL: URL) -> LocalWorkspace? {
        let name = sourceURL.lastPathComponent.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else {
            lastError = "Selected folder has no usable name."
            return nil
        }
        let folder = "import-\(UUID().uuidString)"
        let target = root.appendingPathComponent(folder, isDirectory: true)
        do {
            try FileManager.default.createDirectory(
                at: root,
                withIntermediateDirectories: true
            )
            try FileManager.default.copyItem(at: sourceURL, to: target)
            try excludeFromBackup(target)
        } catch {
            try? FileManager.default.removeItem(at: target)
            lastError = "Could not import folder: \(error.localizedDescription)"
            return nil
        }
        let workspace = LocalWorkspace(
            name: name,
            relativePath: folder,
            source: sourceURL.lastPathComponent
        )
        workspaces.append(workspace)
        persist()
        lastError = nil
        return workspace
    }

    /// Delete the tree and forget it. Irreversible, so callers confirm first.
    public func delete(_ workspace: LocalWorkspace) {
        try? FileManager.default.removeItem(at: url(for: workspace))
        workspaces.removeAll { $0.id == workspace.id }
        persist()
    }

    public func setSource(_ source: String, for workspace: LocalWorkspace) {
        guard let index = workspaces.firstIndex(where: { $0.id == workspace.id }) else { return }
        workspaces[index].source = source
        persist()
    }

    /// Bytes on disk, for the storage row in settings. Nil when it cannot be
    /// determined — shown as "—" rather than a wrong number.
    public func sizeOnDisk(_ workspace: LocalWorkspace) -> Int64? {
        let root = url(for: workspace)
        guard let walker = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: [.totalFileAllocatedSizeKey, .isRegularFileKey]
        ) else { return nil }

        var total: Int64 = 0
        for case let fileURL as URL in walker {
            let values = try? fileURL.resourceValues(forKeys: [.totalFileAllocatedSizeKey, .isRegularFileKey])
            guard values?.isRegularFile == true, let size = values?.totalFileAllocatedSize else { continue }
            total += Int64(size)
        }
        return total
    }

    /// A checked-out repository can be gigabytes and is reconstructible from
    /// its remote. Letting it into iCloud backup is a support incident.
    private func excludeFromBackup(_ url: URL) throws {
        var url = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try url.setResourceValues(values)
    }
}
