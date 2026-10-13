import CryptoKit
import Foundation

/// Typed failures of the shared file owner. Mirrors the Rust owners' `AppRootError` / `FsError`.
public enum RightKitFSError: Error, Equatable, Sendable {
    case invalidName(String)
    case pathEscape(String)
    case containerUnavailable(group: String)
    case symlink(String)
    case notAFile(String)
    case digestMismatch(path: String, expected: String, actual: String)
    case verifyFailed(String)
    case invalidRecord(String)
    case io(path: String, errno: Int32)
}

/// Where an app's data lives on Apple platforms: `Orthic Labs/<App>` under the App Group container
/// (when an access group is given) or Application Support. Same name rules as the Rust
/// `Suite::app_root`; same escape rules as `AppRoot::data_dir`. Foundation + CryptoKit only.
public struct RightKitAppRoot: Sendable {
    public static let maxNameLength = 64
    public static let suiteDirectory = "Orthic Labs"
    private static let reserved: Set<String> = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ]

    /// Mirrors `valid_app_name` in rightkit-daemon: `[A-Za-z0-9._-]`, alphanumeric at both ends,
    /// no `..`, at most 64 bytes, not `shared`, not a Windows reserved device stem.
    public static func isValidAppName(_ s: String) -> Bool {
        let b = Array(s.utf8)
        guard !b.isEmpty, b.count <= maxNameLength else { return false }
        func alnum(_ c: UInt8) -> Bool {
            (48...57).contains(c) || (65...90).contains(c) || (97...122).contains(c)
        }
        guard b.allSatisfy({ alnum($0) || $0 == 45 || $0 == 95 || $0 == 46 }) else { return false }
        guard alnum(b[0]), alnum(b[b.count - 1]), !s.contains("..") else { return false }
        if s.lowercased() == "shared" { return false }
        let stem = (s.split(separator: ".", omittingEmptySubsequences: false).first.map(String.init) ?? s)
            .uppercased()
        return !reserved.contains(stem)
    }

    public let name: String
    /// The app root, symlinks in the base already resolved.
    public let root: URL
    /// Protection applied to files written through this root (iOS only; macOS ignores it).
    public let fileProtection: FileProtectionType

    /// - Parameters:
    ///   - accessGroup: App Group id. When given, its container is the base and a missing container
    ///     throws `containerUnavailable` (no silent fallback onto a location other processes cannot see).
    ///   - baseOverride: replaces the resolved base (tests, or an app that owns its own base).
    public init(
        app: String,
        accessGroup: String? = nil,
        baseOverride: URL? = nil,
        fileProtection: FileProtectionType = .completeUntilFirstUserAuthentication,
        fileManager: FileManager = .default
    ) throws {
        guard Self.isValidAppName(app) else { throw RightKitFSError.invalidName(app) }
        let base: URL
        if let baseOverride {
            base = baseOverride
        } else if let group = accessGroup {
            guard let c = fileManager.containerURL(forSecurityApplicationGroupIdentifier: group) else {
                throw RightKitFSError.containerUnavailable(group: group)
            }
            base = c
        } else {
            guard let s = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else {
                throw RightKitFSError.containerUnavailable(group: "applicationSupport")
            }
            base = s
        }
        let dir = base.appendingPathComponent(Self.suiteDirectory, isDirectory: true)
            .appendingPathComponent(app, isDirectory: true)
        try Self.makeDirs(dir, fileManager: fileManager)
        self.name = app
        self.root = dir.resolvingSymlinksInPath()
        self.fileProtection = fileProtection
    }

    // MARK: Paths

    /// Resolve `rel` under the root without creating anything. Rejects empty, absolute, `..`, NUL,
    /// and any path whose existing prefix is a symlink resolving outside the root.
    public func path(_ rel: String) throws -> URL {
        let bad = RightKitFSError.pathEscape(rel)
        guard !rel.isEmpty, !rel.hasPrefix("/"), !rel.contains("\0") else { throw bad }
        let parts = rel.split(separator: "/", omittingEmptySubsequences: true)
        guard !parts.isEmpty, !parts.contains("..") else { throw bad }
        var url = root
        for p in parts where p != "." { url.appendPathComponent(String(p)) }
        // Walk to the deepest existing ancestor and require its real path to stay inside the root.
        var probe = url
        while !FileManager.default.fileExists(atPath: probe.path)
            && (try? probe.checkResourceIsReachable()) == nil
            && !isSymlink(probe) {
            probe.deleteLastPathComponent()
            if probe.path.count < root.path.count { throw bad }
        }
        let real = probe.resolvingSymlinksInPath().path
        guard real == root.path || real.hasPrefix(root.path + "/") else { throw bad }
        return url
    }

    /// A private directory at `<root>/<rel>`, created if absent, re-checked after creation.
    @discardableResult
    public func dataDir(_ rel: String) throws -> URL {
        let d = try path(rel)
        try Self.makeDirs(d, fileManager: .default)
        _ = try path(rel)
        return d
    }

    // MARK: Atomic write

    /// Write `data` to `rel` (relative to the root) via temp file + `replaceItemAt`, fsynced, mode
    /// 0600, file protection applied. Parents are created. Never leaves a partial destination.
    public func atomicWrite(_ data: Data, to rel: String) throws {
        let dest = try path(rel)
        try Self.makeDirs(dest.deletingLastPathComponent(), fileManager: .default)
        _ = try path(rel)
        let temp = try stageTemp(beside: dest)
        do {
            try writeAndSync(data, to: temp)
            try publish(temp, to: dest)
        } catch {
            try? FileManager.default.removeItem(at: temp)
            throw error
        }
    }

    /// Copy the file at `src` (outside or inside the root) to `rel`, verifying the destination's
    /// SHA-256 against the source's (and `expectedSHA256` when given) before publishing.
    @discardableResult
    public func copyVerified(from src: URL, to rel: String, expectedSHA256: String? = nil) throws -> String {
        var st = stat()
        guard lstat(src.path, &st) == 0 else { throw RightKitFSError.io(path: src.path, errno: errno) }
        if (st.st_mode & S_IFMT) == S_IFLNK { throw RightKitFSError.symlink(src.path) }
        guard (st.st_mode & S_IFMT) == S_IFREG else { throw RightKitFSError.notAFile(src.path) }
        let dest = try path(rel)
        try Self.makeDirs(dest.deletingLastPathComponent(), fileManager: .default)
        _ = try path(rel)
        let digest = try Self.sha256(of: src)
        if let e = expectedSHA256, e.lowercased() != digest {
            throw RightKitFSError.digestMismatch(path: src.path, expected: e.lowercased(), actual: digest)
        }
        let temp = try stageTemp(beside: dest)
        do {
            try FileManager.default.copyItem(at: src, to: temp)
            let written = try Self.sha256(of: temp)
            guard written == digest else { throw RightKitFSError.verifyFailed(dest.path) }
            try harden(temp)
            try fsyncFile(at: temp)
            try publish(temp, to: dest)
        } catch {
            try? FileManager.default.removeItem(at: temp)
            throw error
        }
        return digest
    }

    // MARK: Append

    /// Append one line-delimited record (`record` + `\n`) to `rel`, creating it 0600, then fsync.
    /// The record must not contain a newline. A crash can leave a torn final line; readers drop it.
    public func appendRecord(_ record: Data, to rel: String) throws {
        guard !record.contains(0x0A) else { throw RightKitFSError.invalidRecord("record contains a newline") }
        let dest = try path(rel)
        try Self.makeDirs(dest.deletingLastPathComponent(), fileManager: .default)
        _ = try path(rel)
        let fd = open(dest.path, O_WRONLY | O_APPEND | O_CREAT | O_NOFOLLOW, 0o600)
        guard fd >= 0 else {
            if errno == ELOOP { throw RightKitFSError.symlink(dest.path) }
            throw RightKitFSError.io(path: dest.path, errno: errno)
        }
        defer { close(fd) }
        var st = stat()
        guard fstat(fd, &st) == 0, (st.st_mode & S_IFMT) == S_IFREG else {
            throw RightKitFSError.notAFile(dest.path)
        }
        var line = record
        line.append(0x0A)
        try Self.writeAll(fd, line, path: dest.path)
        guard fcntl(fd, F_FULLFSYNC) == 0 || fsync(fd) == 0 else {
            throw RightKitFSError.io(path: dest.path, errno: errno)
        }
        try? harden(dest)
    }

    // MARK: Internals

    private func isSymlink(_ u: URL) -> Bool {
        var st = stat()
        return lstat(u.path, &st) == 0 && (st.st_mode & S_IFMT) == S_IFLNK
    }

    private func stageTemp(beside dest: URL) throws -> URL {
        if isSymlink(dest) { throw RightKitFSError.symlink(dest.path) }
        var st = stat()
        if lstat(dest.path, &st) == 0, (st.st_mode & S_IFMT) != S_IFREG {
            throw RightKitFSError.notAFile(dest.path)
        }
        return dest.deletingLastPathComponent()
            .appendingPathComponent(".\(dest.lastPathComponent).\(UUID().uuidString).rk-tmp")
    }

    private func writeAndSync(_ data: Data, to temp: URL) throws {
        let fd = open(temp.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
        guard fd >= 0 else { throw RightKitFSError.io(path: temp.path, errno: errno) }
        defer { close(fd) }
        try Self.writeAll(fd, data, path: temp.path)
        guard fcntl(fd, F_FULLFSYNC) == 0 || fsync(fd) == 0 else {
            throw RightKitFSError.io(path: temp.path, errno: errno)
        }
        try harden(temp)
    }

    private func fsyncFile(at url: URL) throws {
        let fd = open(url.path, O_RDONLY)
        guard fd >= 0 else { throw RightKitFSError.io(path: url.path, errno: errno) }
        defer { close(fd) }
        guard fcntl(fd, F_FULLFSYNC) == 0 || fsync(fd) == 0 else {
            throw RightKitFSError.io(path: url.path, errno: errno)
        }
    }

    private func harden(_ url: URL) throws {
        chmod(url.path, 0o600)
        #if os(iOS) || os(tvOS) || os(watchOS) || os(visionOS)
        try? FileManager.default.setAttributes([.protectionKey: fileProtection], ofItemAtPath: url.path)
        #endif
    }

    private func publish(_ temp: URL, to dest: URL) throws {
        let fm = FileManager.default
        do {
            if fm.fileExists(atPath: dest.path) {
                _ = try fm.replaceItemAt(dest, withItemAt: temp)
            } else {
                try fm.moveItem(at: temp, to: dest)
            }
        } catch {
            throw RightKitFSError.io(path: dest.path, errno: Int32(truncatingIfNeeded: (error as NSError).code))
        }
        let dirFD = open(dest.deletingLastPathComponent().path, O_RDONLY)
        if dirFD >= 0 { _ = fsync(dirFD); close(dirFD) }
    }

    private static func makeDirs(_ url: URL, fileManager: FileManager) throws {
        do {
            try fileManager.createDirectory(
                at: url, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        } catch {
            throw RightKitFSError.io(path: url.path, errno: Int32(truncatingIfNeeded: (error as NSError).code))
        }
    }

    private static func writeAll(_ fd: Int32, _ data: Data, path: String) throws {
        try data.withUnsafeBytes { raw in
            var off = 0
            while off < raw.count {
                let n = write(fd, raw.baseAddress! + off, raw.count - off)
                if n < 0 {
                    if errno == EINTR { continue }
                    throw RightKitFSError.io(path: path, errno: errno)
                }
                off += n
            }
        }
    }

    /// Lowercase hex SHA-256 of a file, streamed.
    public static func sha256(of url: URL) throws -> String {
        guard let h = try? FileHandle(forReadingFrom: url) else {
            throw RightKitFSError.io(path: url.path, errno: errno)
        }
        defer { try? h.close() }
        var hasher = SHA256()
        while let chunk = try? h.read(upToCount: 64 * 1024), !chunk.isEmpty { hasher.update(data: chunk) }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }
}
