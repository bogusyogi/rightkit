// Ported from `repos/apps/opencodex` commit `1d34372bb51372b7cd61b716021654c9019dfb94`.
// Upstream sources: `codex-worker-ios/Sources/CodexWorker/Dependencies/FileSystemStore.swift`
// and `codex-worker-ios/Sources/CodexWorker/Models/FileSystem.swift`.
//
// OpenCodex's viewer is remote-worker backed. CodeRight keeps its reducer and
// revision semantics, but routes every file operation through this seam so a
// local workspace, a remote workspace, and deterministic tests can provide
// their own implementation without leaking transport types into the view.

import ComposableArchitecture
import CryptoKit
import Foundation

/// Async counterpart to `Result(catching:)`. OpenCodex uses this shape in
/// feature effects; keeping conversion here makes each reducer's transport
/// seam concise while preserving typed stale/binary/path failures.
func filePreviewResult<Value>(
    _ operation: @escaping @Sendable () async throws -> Value
) async -> Result<Value, FilePreviewError> {
    do {
        return .success(try await operation())
    } catch let error as FilePreviewError {
        return .failure(error)
    } catch {
        return .failure(.io(error.localizedDescription))
    }
}

/// File-preview failures are deliberately typed so a stale write can be
/// rendered differently from a missing file or an unsupported binary file.
public enum FilePreviewError: Error, Equatable, LocalizedError, Sendable {
    case invalidRoot
    case pathOutsideRoot
    case fileNotFound
    case directoryExpected
    case fileExpected
    case binaryFile
    case fileTooLarge(limit: Int)
    case staleRevision(expected: String, actual: String)
    case invalidReference
    case io(String)

    public var errorDescription: String? {
        switch self {
        case .invalidRoot:
            return "Workspace root is unavailable."
        case .pathOutsideRoot:
            return "That path is outside the selected workspace."
        case .fileNotFound:
            return "File or directory was not found."
        case .directoryExpected:
            return "A directory is required for this operation."
        case .fileExpected:
            return "A regular file is required for this operation."
        case .binaryFile:
            return "Binary file preview is unavailable."
        case let .fileTooLarge(limit):
            return "File is larger than \(ByteCountFormatter.string(fromByteCount: Int64(limit), countStyle: .file))."
        case let .staleRevision(expected, actual):
            let expectedText = expected.isEmpty ? "none" : expected
            let actualText = actual.isEmpty ? "none" : actual
            return "File changed since it was opened (expected \(expectedText), found \(actualText)). Reload before saving."
        case .invalidReference:
            return "That file reference could not be resolved."
        case let .io(message):
            return message
        }
    }
}

/// Storage boundary consumed by the transplanted viewer/browser reducers.
///
/// The protocol intentionally speaks in the donor's file payloads because
/// those values carry language, line counts, etags, and pagination metadata
/// needed by the existing state machine. Implementations may be backed by
/// app-owned Foundation I/O, a daemon, or an in-memory test fixture.
public protocol FilePreviewFileSystem: Sendable {
    func loadTreeCache(rootPath: String, path: String) async throws -> FileTreeResponse?
    func saveTreeCache(rootPath: String, path: String, response: FileTreeResponse) async throws

    func listDirectory(
        rootPath: String,
        path: String,
        cursor: Int?,
        limit: Int
    ) async throws -> FileTreeResponse

    func search(
        rootPath: String,
        query: String,
        cursor: Int?,
        limit: Int
    ) async throws -> FileSearchResponse

    func resolveReference(rootPath: String, reference: String) async throws -> FileResolveResponse

    func readFile(
        rootPath: String,
        path: String,
        fromLine: Int,
        maxBytes: Int
    ) async throws -> FileContentResponse

    func writeFile(rootPath: String, request: FileWriteRequest) async throws -> FileWriteResponse

    func loadLatestFileChunkCache(rootPath: String, path: String) async throws -> FileContentPayload?
    func saveFileChunkCache(rootPath: String, payload: FileContentPayload) async throws

    func saveRevision(path: String, etag: String, content: String) async throws
    func listRevisions(path: String, limit: Int) async throws -> [CachedFileRevision]
}

/// TCA dependency wrapper. `FileViewerFeature` and `FileBrowserFeature` use
/// this value rather than constructing a concrete filesystem implementation.
public struct FilePreviewFileSystemClient: DependencyKey, Sendable {
    public let backend: any FilePreviewFileSystem

    public init(backend: any FilePreviewFileSystem) {
        self.backend = backend
    }

    public static var liveValue: Self {
        Self(backend: LocalFilePreviewFileSystem())
    }

    public static let testValue = Self(backend: UnimplementedFilePreviewFileSystem())
    public static let previewValue = Self(backend: UnimplementedFilePreviewFileSystem())

    public func listDirectory(
        rootPath: String,
        path: String,
        cursor: Int?,
        limit: Int
    ) async throws -> FileTreeResponse {
        try await backend.listDirectory(rootPath: rootPath, path: path, cursor: cursor, limit: limit)
    }

    public func loadTreeCache(rootPath: String, path: String) async throws -> FileTreeResponse? {
        try await backend.loadTreeCache(rootPath: rootPath, path: path)
    }

    public func saveTreeCache(rootPath: String, path: String, response: FileTreeResponse) async throws {
        try await backend.saveTreeCache(rootPath: rootPath, path: path, response: response)
    }

    public func search(
        rootPath: String,
        query: String,
        cursor: Int?,
        limit: Int
    ) async throws -> FileSearchResponse {
        try await backend.search(rootPath: rootPath, query: query, cursor: cursor, limit: limit)
    }

    public func resolveReference(rootPath: String, reference: String) async throws -> FileResolveResponse {
        try await backend.resolveReference(rootPath: rootPath, reference: reference)
    }

    public func readFile(
        rootPath: String,
        path: String,
        fromLine: Int,
        maxBytes: Int
    ) async throws -> FileContentResponse {
        try await backend.readFile(rootPath: rootPath, path: path, fromLine: fromLine, maxBytes: maxBytes)
    }

    public func writeFile(rootPath: String, request: FileWriteRequest) async throws -> FileWriteResponse {
        try await backend.writeFile(rootPath: rootPath, request: request)
    }

    public func loadLatestFileChunkCache(rootPath: String, path: String) async throws -> FileContentPayload? {
        try await backend.loadLatestFileChunkCache(rootPath: rootPath, path: path)
    }

    public func saveFileChunkCache(rootPath: String, payload: FileContentPayload) async throws {
        try await backend.saveFileChunkCache(rootPath: rootPath, payload: payload)
    }

    public func saveRevision(path: String, etag: String, content: String) async throws {
        try await backend.saveRevision(path: path, etag: etag, content: content)
    }

    public func listRevisions(path: String, limit: Int) async throws -> [CachedFileRevision] {
        try await backend.listRevisions(path: path, limit: limit)
    }
}

extension DependencyValues {
    public var filePreviewFileSystem: FilePreviewFileSystemClient {
        get { self[FilePreviewFileSystemClient.self] }
        set { self[FilePreviewFileSystemClient.self] = newValue }
    }
}

/// Foundation-backed implementation for CodeRight's app-owned workspaces.
/// The actor serializes cache/revision mutations; each write also re-reads the
/// file and verifies its etag immediately before replacing it.
public actor LocalFilePreviewFileSystem: FilePreviewFileSystem {
    private var treeCache: [String: FileTreeResponse] = [:]
    private var latestCache: [String: FileContentPayload] = [:]
    private var revisions: [String: [CachedFileRevision]] = [:]
    private let persistenceURL: URL
    private var didLoadPersistence = false

    public init(storageURL: URL? = nil) {
        persistenceURL = storageURL ?? Self.defaultPersistenceURL()
    }

    private struct Snapshot: Codable {
        var treeCache: [String: FileTreeResponse]
        var latestCache: [String: FileContentPayload]
        var revisions: [String: [CachedFileRevision]]
    }

    public func loadTreeCache(rootPath: String, path: String) async throws -> FileTreeResponse? {
        loadPersistenceIfNeeded()
        return treeCache[Self.cacheKey(rootPath: rootPath, path: path)]
    }

    public func saveTreeCache(rootPath: String, path: String, response: FileTreeResponse) async throws {
        loadPersistenceIfNeeded()
        treeCache[Self.cacheKey(rootPath: rootPath, path: path)] = response
        if treeCache.count > 500 {
            treeCache.removeValue(forKey: treeCache.keys.sorted().first ?? "")
        }
        persist()
    }

    public func listDirectory(
        rootPath: String,
        path: String,
        cursor: Int?,
        limit: Int
    ) async throws -> FileTreeResponse {
        let root = try validatedRoot(rootPath)
        let directory = try resolve(path, beneath: root)
        let values: Set<URLResourceKey> = [
            .isDirectoryKey,
            .isRegularFileKey,
            .isSymbolicLinkKey,
            .fileSizeKey,
            .contentModificationDateKey,
        ]
        let urls: [URL]
        do {
            urls = try FileManager.default.contentsOfDirectory(
                at: directory,
                includingPropertiesForKeys: Array(values),
                options: []
            )
        } catch let error as CocoaError where error.code == .fileNoSuchFile {
            throw FilePreviewError.fileNotFound
        } catch {
            throw FilePreviewError.io("Could not read directory: \(error.localizedDescription)")
        }

        var entries: [FileTreeEntry] = []
        entries.reserveCapacity(urls.count)
        for url in urls {
            guard isContained(url, beneath: root) else { continue }
            let resourceValues = try? url.resourceValues(forKeys: values)
            guard resourceValues?.isSymbolicLink != true else { continue }
            let isDirectory = resourceValues?.isDirectory == true
            let kind: FileSystemEntryKind = isDirectory
                ? .directory
                : (resourceValues?.isRegularFile == true ? .file : .other)
            let size = isDirectory ? nil : resourceValues?.fileSize
            entries.append(
                FileTreeEntry(
                    name: url.lastPathComponent,
                    path: url.path,
                    kind: kind,
                    size: size,
                    modifiedAt: Self.timestamp(resourceValues?.contentModificationDate)
                )
            )
        }

        entries.sort { lhs, rhs in
            if lhs.isDirectory != rhs.isDirectory { return lhs.isDirectory }
            return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
        }

        let normalizedLimit = max(1, min(limit, 500))
        let offset = max(0, min(cursor ?? 0, entries.count))
        let page = Array(entries.dropFirst(offset).prefix(normalizedLimit))
        let nextOffset = offset + page.count
        return FileTreeResponse(
            data: page,
            nextCursor: nextOffset < entries.count ? nextOffset : nil,
            hasMore: nextOffset < entries.count,
            total: entries.count
        )
    }

    public func search(
        rootPath: String,
        query: String,
        cursor: Int?,
        limit: Int
    ) async throws -> FileSearchResponse {
        let root = try validatedRoot(rootPath)
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else {
            return FileSearchResponse(data: [], nextCursor: nil, hasMore: false, total: 0)
        }

        let normalizedNeedle = needle.localizedLowercase
        var matches = try collectSearchMatches(root: root, normalizedNeedle: normalizedNeedle)

        matches.sort {
            if $0.path != $1.path { return $0.path.localizedStandardCompare($1.path) == .orderedAscending }
            return $0.line < $1.line
        }
        let normalizedLimit = max(1, min(limit, 200))
        let offset = max(0, min(cursor ?? 0, matches.count))
        let page = Array(matches.dropFirst(offset).prefix(normalizedLimit))
        let nextOffset = offset + page.count
        return FileSearchResponse(
            data: page,
            nextCursor: nextOffset < matches.count ? nextOffset : nil,
            hasMore: nextOffset < matches.count,
            total: matches.count
        )
    }

    public func resolveReference(rootPath: String, reference: String) async throws -> FileResolveResponse {
        let root = try validatedRoot(rootPath)
        var raw = reference.trimmingCharacters(in: .whitespacesAndNewlines)
        if let components = URLComponents(string: raw),
           components.scheme?.lowercased() == "codexfs",
           let ref = components.queryItems?.first(where: { $0.name == "ref" })?.value {
            raw = ref.removingPercentEncoding ?? ref
        }
        raw = raw.trimmingCharacters(in: CharacterSet(charactersIn: "`<>"))

        var line: Int?
        var column: Int?
        if let match = raw.range(of: #":(\d+)(?::(\d+))?$"#, options: .regularExpression) {
            let suffix = String(raw[match]).dropFirst()
            let parts = suffix.split(separator: ":")
            line = Int(parts.first ?? "")
            column = parts.count > 1 ? Int(parts[1]) : nil
            raw = String(raw[..<match.lowerBound])
        }

        let candidate = try resolve(raw, beneath: root)
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: candidate.path, isDirectory: &isDirectory), !isDirectory.boolValue else {
            throw FilePreviewError.invalidReference
        }
        return FileResolveResponse(
            data: FileReferenceResolution(
                resolved: true,
                ref: reference,
                path: candidate.path,
                line: line,
                column: column,
                rootId: root.path
            )
        )
    }

    public func readFile(
        rootPath: String,
        path: String,
        fromLine: Int,
        maxBytes: Int
    ) async throws -> FileContentResponse {
        let root = try validatedRoot(rootPath)
        let file = try resolve(path, beneath: root)
        let values = try file.resourceValues(forKeys: [.isDirectoryKey, .isRegularFileKey, .fileSizeKey])
        guard values.isRegularFile == true else {
            if values.isDirectory == true { throw FilePreviewError.directoryExpected }
            throw FilePreviewError.fileExpected
        }
        let limit = max(1, maxBytes)
        guard (values.fileSize ?? 0) <= limit else { throw FilePreviewError.fileTooLarge(limit: limit) }
        let data: Data
        do {
            data = try Data(contentsOf: file, options: .mappedIfSafe)
        } catch {
            throw FilePreviewError.io("Could not read file: \(error.localizedDescription)")
        }
        guard let text = String(data: data, encoding: .utf8) else { throw FilePreviewError.binaryFile }

        let allLines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        let start = max(1, fromLine)
        let selected = start <= allLines.count ? Array(allLines.dropFirst(start - 1)) : []
        let payload = FileContentPayload(
            path: file.path,
            language: Self.language(for: file.path),
            etag: Self.etag(data),
            totalLines: allLines.count,
            fromLine: start,
            toLine: start + max(0, selected.count - 1),
            truncated: false,
            lines: selected.enumerated().map { FileContentLine(line: start + $0.offset, text: $0.element) }
        )
        return FileContentResponse(data: payload)
    }

    // ENG-109: `expectedEtag` is the version-guard token — the etag
    // observed when the caller last loaded this file (see
    // `FileViewerFeature.State.etag`, durably mirrored in
    // `SaveRevisionCache`). A mismatch here means the file changed on disk
    // since it was opened, so the write is refused with a typed
    // `staleRevision` error instead of silently clobbering whatever
    // changed it (an agent edit, most likely). Passing `nil`/empty is an
    // explicit, caller-confirmed bypass (see
    // `FileViewerFeature.overwriteAnywayTapped`), not the default path.
    public func writeFile(rootPath: String, request: FileWriteRequest) async throws -> FileWriteResponse {
        let root = try validatedRoot(rootPath)
        let file = try resolve(request.path, beneath: root)
        let values = try file.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
        guard values.isRegularFile == true else { throw FilePreviewError.fileExpected }
        let existingData = try Data(contentsOf: file, options: .mappedIfSafe)
        let actual = Self.etag(existingData)
        if let expected = request.expectedEtag, !expected.isEmpty, expected != actual {
            throw FilePreviewError.staleRevision(expected: expected, actual: actual)
        }
        let data = Data(request.content.utf8)
        guard data.count <= 2_000_000 else { throw FilePreviewError.fileTooLarge(limit: 2_000_000) }
        do {
            // CodeRight-owned: atomic write to an arbitrary URL with a protection class (no shared consumer; RightKit issue withdrawn 2026-10-18)
            try data.write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        } catch {
            throw FilePreviewError.io("Could not save file: \(error.localizedDescription)")
        }
        let writeValues = try? file.resourceValues(forKeys: [.contentModificationDateKey])
        return FileWriteResponse(
            data: FileWritePayload(
                path: file.path,
                size: data.count,
                modifiedAt: Self.timestamp(writeValues?.contentModificationDate) ?? "",
                etag: Self.etag(data)
            )
        )
    }

    public func loadLatestFileChunkCache(rootPath: String, path: String) async throws -> FileContentPayload? {
        loadPersistenceIfNeeded()
        return latestCache[Self.cacheKey(rootPath: rootPath, path: path)]
    }

    public func saveFileChunkCache(rootPath: String, payload: FileContentPayload) async throws {
        loadPersistenceIfNeeded()
        latestCache[Self.cacheKey(rootPath: rootPath, path: payload.path)] = payload
        if latestCache.count > 2_000 {
            latestCache.removeValue(forKey: latestCache.keys.sorted().first ?? "")
        }
        persist()
    }

    public func saveRevision(path: String, etag: String, content: String) async throws {
        loadPersistenceIfNeeded()
        var values = revisions[path, default: []]
        guard !values.contains(where: { $0.etag == etag }) else { return }
        values.insert(
            CachedFileRevision(
                path: path,
                etag: etag,
                content: content,
                fetchedAtMs: Self.nowMs()
            ),
            at: 0
        )
        revisions[path] = Array(values.prefix(20))
        persist()
    }

    public func listRevisions(path: String, limit: Int) async throws -> [CachedFileRevision] {
        loadPersistenceIfNeeded()
        return Array(revisions[path, default: []].prefix(max(1, min(limit, 30))))
    }

    private func collectSearchMatches(root: URL, normalizedNeedle: String) throws -> [FileSearchMatch] {
        let keys: Set<URLResourceKey> = [.isDirectoryKey, .isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey]
        guard let enumerator = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: Array(keys),
            options: []
        ) else {
            throw FilePreviewError.io("Could not enumerate workspace.")
        }

        var matches: [FileSearchMatch] = []
        var visited = 0
        for case let url as URL in enumerator {
            visited += 1
            if visited > 20_000 { break }
            guard isContained(url, beneath: root) else {
                enumerator.skipDescendants()
                continue
            }
            let values = try? url.resourceValues(forKeys: keys)
            if values?.isSymbolicLink == true {
                enumerator.skipDescendants()
                continue
            }
            guard values?.isRegularFile == true,
                  let size = values?.fileSize,
                  size <= 2_000_000,
                  let data = try? Data(contentsOf: url, options: .mappedIfSafe),
                  let text = String(data: data, encoding: .utf8)
            else { continue }

            let lines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
            for (index, line) in lines.enumerated() where line.localizedLowercase.contains(normalizedNeedle) {
                matches.append(FileSearchMatch(path: url.path, line: index + 1, snippet: line))
                if matches.count >= 2_000 { break }
            }
            if matches.count >= 2_000 { break }
        }
        return matches
    }

    private func loadPersistenceIfNeeded() {
        guard !didLoadPersistence else { return }
        didLoadPersistence = true
        guard let data = try? Data(contentsOf: persistenceURL),
              let snapshot = try? JSONDecoder().decode(Snapshot.self, from: data)
        else { return }
        treeCache = snapshot.treeCache
        latestCache = snapshot.latestCache
        revisions = snapshot.revisions
    }

    private func persist() {
        let snapshot = Snapshot(treeCache: treeCache, latestCache: latestCache, revisions: revisions)
        guard let data = try? JSONEncoder().encode(snapshot) else { return }
        let parent = persistenceURL.deletingLastPathComponent()
        do {
            try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
            // CodeRight-owned: atomic write to an arbitrary URL with a protection class (no shared consumer; RightKit issue withdrawn 2026-10-18)
            try data.write(
                to: persistenceURL,
                options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication]
            )
        } catch {
            // Cache/revision writes must not prevent the source file operation
            // itself from succeeding. A future durable-store adapter may log
            // this through its own diagnostics channel.
        }
    }

    private func validatedRoot(_ rootPath: String) throws -> URL {
        guard !rootPath.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw FilePreviewError.invalidRoot
        }
        let root = URL(fileURLWithPath: rootPath).resolvingSymlinksInPath().standardizedFileURL
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: root.path, isDirectory: &isDirectory), isDirectory.boolValue else {
            throw FilePreviewError.invalidRoot
        }
        return root
    }

    private func resolve(_ path: String, beneath root: URL) throws -> URL {
        let trimmed = path.trimmingCharacters(in: .whitespacesAndNewlines)
        let raw = trimmed.isEmpty || trimmed == "."
            ? root
            : (trimmed.hasPrefix("/")
                ? URL(fileURLWithPath: trimmed)
                : root.appendingPathComponent(trimmed))
        let candidate = raw.standardizedFileURL
        let resolved = candidate.resolvingSymlinksInPath().standardizedFileURL
        guard isContained(resolved, beneath: root) else { throw FilePreviewError.pathOutsideRoot }
        return resolved
    }

    private func isContained(_ url: URL, beneath root: URL) -> Bool {
        let rootPath = root.resolvingSymlinksInPath().standardizedFileURL.path
        let path = url.resolvingSymlinksInPath().standardizedFileURL.path
        return path == rootPath || (rootPath == "/" ? path.hasPrefix("/") : path.hasPrefix(rootPath + "/"))
    }

    private static func cacheKey(rootPath: String, path: String) -> String {
        "\(rootPath)\u{0}\(path)"
    }

    private static func defaultPersistenceURL() -> URL {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return support
            .appendingPathComponent("CodeRight", isDirectory: true)
            .appendingPathComponent("FilePreview", isDirectory: true)
            .appendingPathComponent("cache.json", isDirectory: false)
    }

    private static func etag(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    private static func language(for path: String) -> String {
        switch URL(fileURLWithPath: path).pathExtension.lowercased() {
        case "swift": return "swift"
        case "js", "mjs", "cjs", "jsx": return "javascript"
        case "ts": return "typescript"
        case "tsx": return "tsx"
        case "json": return "json"
        case "yaml", "yml": return "yaml"
        case "toml": return "toml"
        case "md", "markdown", "mdx": return "markdown"
        case "sh", "bash", "zsh": return "bash"
        case "py": return "python"
        case "css", "scss": return "css"
        case "html", "htm": return "html"
        default: return "text"
        }
    }

    private static func timestamp(_ date: Date?) -> String? {
        guard let date else { return nil }
        return ISO8601DateFormatter().string(from: date)
    }

    private static func nowMs() -> Int64 {
        Int64(Date().timeIntervalSince1970 * 1000)
    }
}

private struct UnimplementedFilePreviewFileSystem: FilePreviewFileSystem {
    private func unavailable<T>() throws -> T {
        throw FilePreviewError.io("File preview filesystem is not configured.")
    }

    func listDirectory(rootPath: String, path: String, cursor: Int?, limit: Int) async throws -> FileTreeResponse {
        try unavailable()
    }

    func loadTreeCache(rootPath: String, path: String) async throws -> FileTreeResponse? { nil }
    func saveTreeCache(rootPath: String, path: String, response: FileTreeResponse) async throws {}

    func search(rootPath: String, query: String, cursor: Int?, limit: Int) async throws -> FileSearchResponse {
        try unavailable()
    }

    func resolveReference(rootPath: String, reference: String) async throws -> FileResolveResponse {
        try unavailable()
    }

    func readFile(rootPath: String, path: String, fromLine: Int, maxBytes: Int) async throws -> FileContentResponse {
        try unavailable()
    }

    func writeFile(rootPath: String, request: FileWriteRequest) async throws -> FileWriteResponse {
        try unavailable()
    }

    func loadLatestFileChunkCache(rootPath: String, path: String) async throws -> FileContentPayload? {
        nil
    }

    func saveFileChunkCache(rootPath: String, payload: FileContentPayload) async throws {}
    func saveRevision(path: String, etag: String, content: String) async throws {}

    func listRevisions(path: String, limit: Int) async throws -> [CachedFileRevision] {
        []
    }
}
