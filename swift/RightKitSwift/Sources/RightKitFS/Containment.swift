import CryptoKit
import Foundation

/// Path containment shared by `RightKitAppRoot` and `RightKitContainedRoot`; mirrors the Rust
/// `rightkit-fs` `contain_lenient` rules (refuse `..`, absolute paths and NUL up front; resolve the
/// deepest existing ancestor through links and require it to stay inside the real root; a dangling
/// or looping link is an error, never "absent"; whole components are compared, so `/a/root2` is not
/// inside `/a/root`).
enum RKContainment {
    static func realPath(_ path: String) -> String? {
        var buffer = [CChar](repeating: 0, count: Int(PATH_MAX) + 1)
        guard realpath(path, &buffer) != nil else { return nil }
        return String(cString: buffer)
    }

    static func isInside(_ real: String, root realRoot: String) -> Bool {
        if real == realRoot { return true }
        let prefix = realRoot.hasSuffix("/") ? realRoot : realRoot + "/"
        return real.hasPrefix(prefix)
    }

    /// - Parameter allowRoot: when true an empty path or `"."` names the root itself.
    static func resolve(root: URL, realRoot: String, rel: String, allowRoot: Bool) throws -> URL {
        let bad = RightKitFSError.pathEscape(rel)
        guard !rel.contains("\0"), !rel.hasPrefix("/") else { throw bad }
        let parts = rel.split(separator: "/", omittingEmptySubsequences: true)
        guard !parts.contains("..") else { throw bad }
        var url = root
        var named = 0
        for p in parts where p != "." {
            url.appendPathComponent(String(p))
            named += 1
        }
        if named == 0 && !allowRoot { throw bad }

        // Walk to the deepest existing ancestor; a link that does not resolve is refused.
        var probe = url.path
        let floor = root.path.count
        while true {
            var st = stat()
            if lstat(probe, &st) == 0 {
                if (st.st_mode & S_IFMT) == S_IFLNK {
                    var target = stat()
                    guard stat(probe, &target) == 0 else { throw bad }
                }
                break
            }
            let err = errno
            guard err == ENOENT else { throw RightKitFSError.io(path: probe, errno: err) }
            probe = (probe as NSString).deletingLastPathComponent
            if probe.count < floor { throw bad }
        }
        guard let real = realPath(probe), isInside(real, root: realRoot) else { throw bad }
        return url
    }
}

/// Low-level file operations shared by the roots: private temp file beside the target, fsync,
/// mode 0600, file protection, publish.
struct RKFiles: Sendable {
    let fileProtection: FileProtectionType

    func isSymlink(_ u: URL) -> Bool {
        var st = stat()
        return lstat(u.path, &st) == 0 && (st.st_mode & S_IFMT) == S_IFLNK
    }

    func stageTemp(beside dest: URL) throws -> URL {
        if isSymlink(dest) { throw RightKitFSError.symlink(dest.path) }
        var st = stat()
        if lstat(dest.path, &st) == 0, (st.st_mode & S_IFMT) != S_IFREG {
            throw RightKitFSError.notAFile(dest.path)
        }
        return dest.deletingLastPathComponent()
            .appendingPathComponent(".\(dest.lastPathComponent).\(UUID().uuidString).rk-tmp")
    }

    /// Stage, write, fsync and publish `data` at `dest` (parents must exist). Temp cleaned on failure.
    func writeFile(_ data: Data, to dest: URL) throws {
        let temp = try stageTemp(beside: dest)
        do {
            try writeAndSync(data, to: temp)
            _ = try publish(temp, to: dest, replace: true)
        } catch {
            try? FileManager.default.removeItem(at: temp)
            throw error
        }
    }

    func writeAndSync(_ data: Data, to temp: URL) throws {
        let fd = open(temp.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
        guard fd >= 0 else { throw RightKitFSError.io(path: temp.path, errno: errno) }
        defer { close(fd) }
        try Self.writeAll(fd, data, path: temp.path)
        guard fcntl(fd, F_FULLFSYNC) == 0 || fsync(fd) == 0 else {
            throw RightKitFSError.io(path: temp.path, errno: errno)
        }
        try harden(temp)
    }

    func fsyncFile(at url: URL) throws {
        let fd = open(url.path, O_RDONLY)
        guard fd >= 0 else { throw RightKitFSError.io(path: url.path, errno: errno) }
        defer { close(fd) }
        guard fcntl(fd, F_FULLFSYNC) == 0 || fsync(fd) == 0 else {
            throw RightKitFSError.io(path: url.path, errno: errno)
        }
    }

    func harden(_ url: URL) throws {
        chmod(url.path, 0o600)
        #if os(iOS) || os(tvOS) || os(watchOS) || os(visionOS)
        try? FileManager.default.setAttributes([.protectionKey: fileProtection], ofItemAtPath: url.path)
        #endif
    }

    /// Move `temp` onto `dest`. With `replace == false` the move fails closed when `dest` exists
    /// (atomic `RENAME_EXCL`, so a racing writer is never overwritten) and `false` is returned.
    func publish(_ temp: URL, to dest: URL, replace: Bool) throws -> Bool {
        let fm = FileManager.default
        if replace {
            do {
                if fm.fileExists(atPath: dest.path) {
                    _ = try fm.replaceItemAt(dest, withItemAt: temp)
                } else {
                    try fm.moveItem(at: temp, to: dest)
                }
            } catch {
                throw RightKitFSError.io(path: dest.path, errno: Int32(truncatingIfNeeded: (error as NSError).code))
            }
        } else if renamex_np(temp.path, dest.path, UInt32(RENAME_EXCL)) != 0 {
            if errno == EEXIST { return false }
            throw RightKitFSError.io(path: dest.path, errno: errno)
        }
        let dirFD = open(dest.deletingLastPathComponent().path, O_RDONLY)
        if dirFD >= 0 { _ = fsync(dirFD); close(dirFD) }
        return true
    }

    static func readData(at url: URL) throws -> Data? {
        let fd = open(url.path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else {
            switch errno {
            case ENOENT: return nil
            case ELOOP: throw RightKitFSError.symlink(url.path)
            default: throw RightKitFSError.io(path: url.path, errno: errno)
            }
        }
        defer { close(fd) }
        var st = stat()
        guard fstat(fd, &st) == 0, (st.st_mode & S_IFMT) == S_IFREG else {
            throw RightKitFSError.notAFile(url.path)
        }
        var out = Data()
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let n = read(fd, &buffer, buffer.count)
            if n < 0 {
                if errno == EINTR { continue }
                throw RightKitFSError.io(path: url.path, errno: errno)
            }
            if n == 0 { break }
            out.append(buffer, count: n)
        }
        return out
    }

    static func makeDirs(_ url: URL, fileManager: FileManager) throws {
        do {
            try fileManager.createDirectory(
                at: url, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        } catch {
            throw RightKitFSError.io(path: url.path, errno: Int32(truncatingIfNeeded: (error as NSError).code))
        }
    }

    static func writeAll(_ fd: Int32, _ data: Data, path: String) throws {
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

    static func sha256(of url: URL) throws -> String {
        guard let h = try? FileHandle(forReadingFrom: url) else {
            throw RightKitFSError.io(path: url.path, errno: errno)
        }
        defer { try? h.close() }
        var hasher = SHA256()
        while let chunk = try? h.read(upToCount: 64 * 1024), !chunk.isEmpty { hasher.update(data: chunk) }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }
}

/// A contained path resolver (and atomic writer) under any base directory, for callers whose
/// layout is not `Orthic Labs/<app>` (a File Provider's workspaces folder, an export directory).
/// Same containment rules as `RightKitAppRoot.path`, except an empty path may name the base itself.
public struct RightKitContainedRoot: Sendable {
    /// The base, symlinks resolved.
    public let base: URL
    public let fileProtection: FileProtectionType
    private let realBase: String

    /// - Parameter createIfMissing: create `base` (0700) when absent; otherwise it must exist.
    public init(
        base: URL,
        createIfMissing: Bool = true,
        fileProtection: FileProtectionType = .completeUntilFirstUserAuthentication,
        fileManager: FileManager = .default
    ) throws {
        guard base.isFileURL, !base.path.contains("\0") else { throw RightKitFSError.pathEscape(base.absoluteString) }
        if createIfMissing { try RKFiles.makeDirs(base, fileManager: fileManager) }
        guard let real = RKContainment.realPath(base.path) else {
            throw RightKitFSError.io(path: base.path, errno: errno)
        }
        var st = stat()
        guard stat(real, &st) == 0, (st.st_mode & S_IFMT) == S_IFDIR else {
            throw RightKitFSError.notAFile(base.path)
        }
        self.base = URL(fileURLWithPath: real, isDirectory: true)
        self.realBase = real
        self.fileProtection = fileProtection
    }

    /// Resolve `rel` under the base without creating anything. Throws `pathEscape` for `..`, an
    /// absolute path, NUL, a symlink (also a dangling or looping one) leading outside the base, and,
    /// when `allowRoot` is false, an empty path or `"."`.
    public func resolve(_ rel: String, allowRoot: Bool = true) throws -> URL {
        try RKContainment.resolve(root: base, realRoot: realBase, rel: rel, allowRoot: allowRoot)
    }

    /// Write `data` to `rel` via a temp file beside the target and an atomic replace, fsynced, mode
    /// 0600. Parents are created, an existing file is replaced, the temp file is removed on failure,
    /// and a symlink or non-regular file at the target is refused.
    public func atomicWrite(_ data: Data, to rel: String) throws {
        let dest = try resolve(rel, allowRoot: false)
        try RKFiles.makeDirs(dest.deletingLastPathComponent(), fileManager: .default)
        _ = try resolve(rel, allowRoot: false)
        try RKFiles(fileProtection: fileProtection).writeFile(data, to: dest)
    }
}
