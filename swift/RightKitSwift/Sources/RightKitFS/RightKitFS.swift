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
    /// `realpath` of `root`, the string every containment check compares against.
    private let realRoot: String

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
        self.realRoot = RKContainment.realPath(self.root.path) ?? self.root.path
        self.fileProtection = fileProtection
    }

    /// An app whose data folder is exactly `root`, with no `Orthic Labs/<App>` appended. For an app
    /// that keeps state somewhere it already owns (Pulse: Application Support/Pulse). `root` is
    /// created (0700) if absent, symlinks resolved. All path, atomic-write and verified-copy rules of
    /// the standard initializer apply under it.
    public init(
        root: URL,
        name: String,
        fileProtection: FileProtectionType = .completeUntilFirstUserAuthentication,
        fileManager: FileManager = .default
    ) throws {
        guard Self.isValidAppName(name) else { throw RightKitFSError.invalidName(name) }
        guard root.isFileURL else { throw RightKitFSError.pathEscape(root.absoluteString) }
        try Self.makeDirs(root, fileManager: fileManager)
        self.name = name
        self.root = root.resolvingSymlinksInPath()
        self.realRoot = RKContainment.realPath(self.root.path) ?? self.root.path
        self.fileProtection = fileProtection
    }

    // MARK: Paths

    /// Resolve `rel` under the root without creating anything. Rejects empty, absolute, `..`, NUL,
    /// and any path whose existing prefix is a symlink resolving outside the root (a dangling or
    /// looping link is refused too, never treated as absent).
    public func path(_ rel: String) throws -> URL {
        try RKContainment.resolve(root: root, realRoot: realRoot, rel: rel, allowRoot: false)
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
        try files.writeFile(data, to: dest)
    }

    /// Copy the file at `src` (outside or inside the root) to `rel`, verifying the destination's
    /// SHA-256 against the source's (and `expectedSHA256` when given) before publishing.
    @discardableResult
    public func copyVerified(from src: URL, to rel: String, expectedSHA256: String? = nil) throws -> String {
        // `replace: true` always yields a digest.
        try copyVerifiedImpl(from: src, to: rel, expectedSHA256: expectedSHA256, replace: true) ?? ""
    }

    /// `nil` only when `replace` is false and the destination appeared first (nothing was changed).
    private func copyVerifiedImpl(from src: URL, to rel: String, expectedSHA256: String?, replace: Bool) throws -> String? {
        var st = stat()
        guard lstat(src.path, &st) == 0 else { throw RightKitFSError.io(path: src.path, errno: errno) }
        if (st.st_mode & S_IFMT) == S_IFLNK { throw RightKitFSError.symlink(src.path) }
        guard (st.st_mode & S_IFMT) == S_IFREG else { throw RightKitFSError.notAFile(src.path) }
        let dest = try path(rel)
        try Self.makeDirs(dest.deletingLastPathComponent(), fileManager: .default)
        _ = try path(rel)
        let digest = try RKFiles.sha256(of: src)
        if let e = expectedSHA256, e.lowercased() != digest {
            throw RightKitFSError.digestMismatch(path: src.path, expected: e.lowercased(), actual: digest)
        }
        let temp = try files.stageTemp(beside: dest)
        do {
            try FileManager.default.copyItem(at: src, to: temp)
            let written = try RKFiles.sha256(of: temp)
            guard written == digest else { throw RightKitFSError.verifyFailed(dest.path) }
            try files.harden(temp)
            try files.fsyncFile(at: temp)
            guard try files.publish(temp, to: dest, replace: replace) else {
                try? FileManager.default.removeItem(at: temp)
                return nil
            }
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
        try RKFiles.writeAll(fd, line, path: dest.path)
        guard fcntl(fd, F_FULLFSYNC) == 0 || fsync(fd) == 0 else {
            throw RightKitFSError.io(path: dest.path, errno: errno)
        }
        try? files.harden(dest)
    }

    // MARK: Read, exists, remove, list

    /// The bytes of the regular file at `rel`, or `nil` when it does not exist. A symlink at the
    /// final name is refused (`symlink`), as is anything that is not a regular file.
    public func readData(_ rel: String) throws -> Data? {
        try RKFiles.readData(at: try path(rel))
    }

    /// True when something exists at `rel` inside the root. A path that would escape is `false`.
    public func exists(_ rel: String) -> Bool {
        guard let u = try? path(rel) else { return false }
        var st = stat()
        return lstat(u.path, &st) == 0
    }

    /// Delete the file (or directory tree) at `rel`. An absent path is success. A final-name symlink
    /// is removed as the link itself, never its target, and only when it resolves inside the root.
    public func remove(_ rel: String) throws {
        let u = try path(rel)
        var st = stat()
        guard lstat(u.path, &st) == 0 else {
            if errno == ENOENT { return }
            throw RightKitFSError.io(path: u.path, errno: errno)
        }
        do { try FileManager.default.removeItem(at: u) } catch {
            throw RightKitFSError.io(path: u.path, errno: Int32(truncatingIfNeeded: (error as NSError).code))
        }
    }

    /// Names (not paths) directly inside the directory `dir`, sorted; `""` or `"."` lists the root.
    /// Internal temp files of an in-flight atomic write are not listed. An absent directory is `[]`.
    public func list(_ dir: String = "") throws -> [String] {
        let u = try RKContainment.resolve(root: root, realRoot: realRoot, rel: dir, allowRoot: true)
        var st = stat()
        guard lstat(u.path, &st) == 0 || stat(u.path, &st) == 0 else {
            if errno == ENOENT { return [] }
            throw RightKitFSError.io(path: u.path, errno: errno)
        }
        do {
            return try FileManager.default.contentsOfDirectory(atPath: u.path)
                .filter { !($0.hasPrefix(".") && $0.hasSuffix(".rk-tmp")) }
                .sorted()
        } catch {
            throw RightKitFSError.io(path: u.path, errno: Int32(truncatingIfNeeded: (error as NSError).code))
        }
    }

    // MARK: One-shot legacy migration

    /// Move the file at `legacy` to `rel`. Does nothing and returns `false` when the destination
    /// already exists or the legacy file is absent. Otherwise the file is copied beside the
    /// destination, its SHA-256 compared with the source's, published only if the destination is
    /// still free (no overwrite, even against a racing process), re-read and re-verified, and only
    /// then is the source deleted. Any failure leaves the source in place. Returns `true` when the
    /// file was migrated. A source that is a symlink or not a regular file throws.
    @discardableResult
    public func migrate(from legacy: URL, to rel: String) throws -> Bool {
        let dest = try path(rel)
        var st = stat()
        if lstat(dest.path, &st) == 0 { return false }
        guard lstat(legacy.path, &st) == 0 else {
            if errno == ENOENT { return false }
            throw RightKitFSError.io(path: legacy.path, errno: errno)
        }
        guard (st.st_mode & S_IFMT) != S_IFLNK else { throw RightKitFSError.symlink(legacy.path) }
        guard (st.st_mode & S_IFMT) == S_IFREG else { throw RightKitFSError.notAFile(legacy.path) }
        guard try copyVerifiedImpl(from: legacy, to: rel, expectedSHA256: nil, replace: false) != nil else {
            return false
        }
        let digest = try RKFiles.sha256(of: legacy)
        guard try RKFiles.sha256(of: dest) == digest else { throw RightKitFSError.verifyFailed(dest.path) }
        do { try FileManager.default.removeItem(at: legacy) } catch {
            throw RightKitFSError.io(path: legacy.path, errno: Int32(truncatingIfNeeded: (error as NSError).code))
        }
        return true
    }

    // MARK: Internals

    private var files: RKFiles { RKFiles(fileProtection: fileProtection) }

    private static func makeDirs(_ url: URL, fileManager: FileManager) throws {
        try RKFiles.makeDirs(url, fileManager: fileManager)
    }

    /// Lowercase hex SHA-256 of a file, streamed.
    public static func sha256(of url: URL) throws -> String { try RKFiles.sha256(of: url) }
}
