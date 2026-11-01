// Ported from `repos/apps/opencodex` commit `1d34372bb51372b7cd61b716021654c9019dfb94`.
// Upstream source: `codex-worker-ios/Sources/CodexWorker/Models/FileSystem.swift`.
// Kept inside CodeRight's file-preview port so model transport types stay behind
// the local filesystem protocol declared in FilePreviewFileSystem.swift.
//
//  FileSystem.swift
//  CodexWorker
//
//  文件系统相关模型
//

import Foundation

/// Minimal session identity needed by the donor browser reducer. The target
/// app can construct this from either a local session or a paired daemon
/// session without importing the donor's `Thread` model.
public struct FilePreviewThread: Equatable, Sendable {
    public let threadId: String
    public let cwd: String?

    public init(threadId: String, cwd: String?) {
        self.threadId = threadId
        self.cwd = cwd
    }
}

public enum FileSystemEntryKind: String, Codable, Sendable {
    case directory
    case file
    case other
}

public struct FileSystemRoot: Codable, Equatable, Identifiable, Sendable {
    public let rootId: String
    public let rootPath: String
    public let displayName: String

    public var id: String { rootId }

    public init(rootId: String, rootPath: String, displayName: String) {
        self.rootId = rootId
        self.rootPath = rootPath
        self.displayName = displayName
    }
}

public struct FileSystemRootsResponse: Codable, Sendable {
    public let data: [FileSystemRoot]

    public init(data: [FileSystemRoot]) {
        self.data = data
    }
}

public struct FileReferenceResolution: Codable, Equatable, Sendable {
    public let resolved: Bool
    public let ref: String
    public let path: String?
    public let line: Int?
    public let column: Int?
    public let rootId: String?

    public init(
        resolved: Bool,
        ref: String,
        path: String?,
        line: Int?,
        column: Int?,
        rootId: String?
    ) {
        self.resolved = resolved
        self.ref = ref
        self.path = path
        self.line = line
        self.column = column
        self.rootId = rootId
    }
}

public struct FileResolveResponse: Codable, Sendable {
    public let data: FileReferenceResolution

    public init(data: FileReferenceResolution) {
        self.data = data
    }
}

public struct FileTreeEntry: Codable, Equatable, Identifiable, Sendable {
    public let name: String
    public let path: String
    public let kind: FileSystemEntryKind
    public let size: Int?
    public let modifiedAt: String?

    public var id: String { path }

    public var isDirectory: Bool {
        kind == .directory
    }

    public init(
        name: String,
        path: String,
        kind: FileSystemEntryKind,
        size: Int?,
        modifiedAt: String?
    ) {
        self.name = name
        self.path = path
        self.kind = kind
        self.size = size
        self.modifiedAt = modifiedAt
    }
}

public struct FileTreeResponse: Codable, Sendable {
    public let data: [FileTreeEntry]
    public let nextCursor: Int?
    public let hasMore: Bool
    public let total: Int

    public init(data: [FileTreeEntry], nextCursor: Int?, hasMore: Bool, total: Int) {
        self.data = data
        self.nextCursor = nextCursor
        self.hasMore = hasMore
        self.total = total
    }
}

public struct FileContentLine: Codable, Equatable, Sendable {
    public let line: Int
    public let text: String

    public init(line: Int, text: String) {
        self.line = line
        self.text = text
    }
}

public struct FileContentPayload: Codable, Equatable, Sendable {
    public let path: String
    public let language: String
    public let etag: String
    public let totalLines: Int
    public let fromLine: Int
    public let toLine: Int
    public let truncated: Bool
    public let lines: [FileContentLine]

    public init(
        path: String,
        language: String,
        etag: String,
        totalLines: Int,
        fromLine: Int,
        toLine: Int,
        truncated: Bool,
        lines: [FileContentLine]
    ) {
        self.path = path
        self.language = language
        self.etag = etag
        self.totalLines = totalLines
        self.fromLine = fromLine
        self.toLine = toLine
        self.truncated = truncated
        self.lines = lines
    }

    public var fullText: String {
        lines.map(\.text).joined(separator: "\n")
    }
}

public struct FileContentResponse: Codable, Sendable {
    public let data: FileContentPayload

    public init(data: FileContentPayload) {
        self.data = data
    }
}

public struct FileStatPayload: Codable, Equatable, Sendable {
    public let path: String
    public let kind: FileSystemEntryKind
    public let size: Int
    public let isDirectory: Bool
    public let isFile: Bool
    public let modifiedAt: String
    public let createdAt: String
    public let etag: String

    public init(
        path: String,
        kind: FileSystemEntryKind,
        size: Int,
        isDirectory: Bool,
        isFile: Bool,
        modifiedAt: String,
        createdAt: String,
        etag: String
    ) {
        self.path = path
        self.kind = kind
        self.size = size
        self.isDirectory = isDirectory
        self.isFile = isFile
        self.modifiedAt = modifiedAt
        self.createdAt = createdAt
        self.etag = etag
    }
}

public struct FileStatResponse: Codable, Sendable {
    public let data: FileStatPayload

    public init(data: FileStatPayload) {
        self.data = data
    }
}

public struct FileSearchMatch: Codable, Equatable, Identifiable, Sendable {
    public let path: String
    public let line: Int
    public let snippet: String

    public var id: String { "\(path):\(line):\(snippet)" }

    public init(path: String, line: Int, snippet: String) {
        self.path = path
        self.line = line
        self.snippet = snippet
    }
}

public struct FileSearchResponse: Codable, Sendable {
    public let data: [FileSearchMatch]
    public let nextCursor: Int?
    public let hasMore: Bool
    public let total: Int

    public init(data: [FileSearchMatch], nextCursor: Int?, hasMore: Bool, total: Int) {
        self.data = data
        self.nextCursor = nextCursor
        self.hasMore = hasMore
        self.total = total
    }
}

public struct FileWriteRequest: Codable, Sendable {
    public var path: String
    public var content: String
    public var expectedEtag: String?

    public init(path: String, content: String, expectedEtag: String?) {
        self.path = path
        self.content = content
        self.expectedEtag = expectedEtag
    }
}

public struct FileWritePayload: Codable, Equatable, Sendable {
    public let path: String
    public let size: Int
    public let modifiedAt: String
    public let etag: String

    public init(path: String, size: Int, modifiedAt: String, etag: String) {
        self.path = path
        self.size = size
        self.modifiedAt = modifiedAt
        self.etag = etag
    }
}

public struct FileWriteResponse: Codable, Sendable {
    public let data: FileWritePayload

    public init(data: FileWritePayload) {
        self.data = data
    }
}

/// A previously fetched text snapshot kept for revision recovery.
///
/// This value mirrors OpenCodex's `CachedFileRevision` without importing its
/// GRDB-backed store. `FilePreviewFileSystem` owns persistence so the caller
/// can provide an app-container implementation, an in-memory test double, or
/// a future SQLite adapter.
public struct CachedFileRevision: Codable, Equatable, Sendable {
    public let path: String
    public let etag: String
    public let content: String
    public let fetchedAtMs: Int64

    public init(path: String, etag: String, content: String, fetchedAtMs: Int64) {
        self.path = path
        self.etag = etag
        self.content = content
        self.fetchedAtMs = fetchedAtMs
    }
}
