import Foundation

#if canImport(Darwin)
import Darwin

/// Errors from the cross-process wake.
public enum DaemonWakeError: Error, Equatable, Sendable {
    /// The name is not a valid wake (service) name: lowercase letters, digits and `-`, 1 to 48 bytes.
    case invalidName(String)
    /// The wake directory is not a private directory of this user (not a directory, another owner,
    /// or group/world writable). The call fails closed.
    case insecureDirectory(String)
    /// The socket path would not fit in a `sockaddr_un`.
    case pathTooLong(String)
    case system(operation: String, errno: Int32)
}

/// The wire contract of `rightkit-daemon`'s `wake` module (rightkit#404), Unix side.
///
/// A wake carries no data: it says "look again". Posts coalesce. A Rust `wake::post` wakes a Swift
/// `DaemonWakeListener` and the reverse, because both use the same directory layout: a 0700
/// directory `rkwake-<euid>-<fnv1a64 hex of "<suiteRoot>|<name>">` under the per-user temporary
/// directory, one datagram socket `<pid>-<n>.sock` per listener, one byte sent to every socket.
public enum DaemonWake {
    /// Longest wake name, as `rightkit-daemon` `MAX_SERVICE_NAME_LEN`.
    public static let maxNameLength = 48

    public static func isValidName(_ name: String) -> Bool {
        !name.isEmpty && name.utf8.count <= maxNameLength
            && name.utf8.allSatisfy { ($0 >= 0x61 && $0 <= 0x7a) || ($0 >= 0x30 && $0 <= 0x39) || $0 == 0x2d }
    }

    /// 64-bit FNV-1a as 16 lower-case hex digits (a short stable directory name, not a security property).
    static func hashHex(_ text: String) -> String {
        var h: UInt64 = 0xcbf2_9ce4_8422_2325
        for b in text.utf8 {
            h ^= UInt64(b)
            h = h &* 0x0000_0100_0000_01b3
        }
        return String(format: "%016llx", h)
    }

    /// The wake directory for `name` in the suite whose root is `suiteRoot`. `suiteRoot` must be the
    /// same path text the posting and listening processes use (the daemon hashes its display form).
    public static func directory(suiteRoot: String, name: String) -> URL {
        let key = "\(suiteRoot)|\(name)"
        let temp = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
        return temp.appendingPathComponent("rkwake-\(geteuid())-\(hashHex(key))", isDirectory: true)
    }

    /// Wake every listener on `name`. Returns how many were signalled; zero when nobody listens.
    @discardableResult
    public static func post(suiteRoot: String, name: String) throws -> Int {
        guard isValidName(name) else { throw DaemonWakeError.invalidName(name) }
        let dir = directory(suiteRoot: suiteRoot, name: name)
        let entries: [String]
        do {
            entries = try FileManager.default.contentsOfDirectory(atPath: dir.path)
        } catch let error as NSError where error.domain == NSCocoaErrorDomain
            && (error.code == NSFileReadNoSuchFileError || error.code == NSFileNoSuchFileError) {
            return 0
        }
        try ensureDirectory(dir)
        let fd = socket(AF_UNIX, SOCK_DGRAM, 0)
        guard fd >= 0 else { throw DaemonWakeError.system(operation: "socket", errno: errno) }
        defer { close(fd) }
        _ = fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)
        var woken = 0
        var byte: UInt8 = 1
        for entry in entries where entry.hasSuffix(".sock") {
            let path = dir.appendingPathComponent(entry).path
            let sent: Int = try withSockaddr(path) { addr, length in
                sendto(fd, &byte, 1, 0, addr, length)
            }
            if sent >= 0 {
                woken += 1
                continue
            }
            switch errno {
            // A full queue is a pending wake already: the listener will look.
            case EAGAIN, ENOBUFS: woken += 1
            // Nobody listens on this socket any more: remove the stale endpoint.
            case ECONNREFUSED, ENOENT: unlink(path)
            default: break
            }
        }
        return woken
    }

    /// Listen for wakes on `name`. Dropping the listener removes its endpoint.
    public static func listen(suiteRoot: String, name: String) throws -> DaemonWakeListener {
        guard isValidName(name) else { throw DaemonWakeError.invalidName(name) }
        let dir = directory(suiteRoot: suiteRoot, name: name)
        try ensureDirectory(dir)
        let path = dir.appendingPathComponent("\(getpid())-\(nextSerial()).sock").path
        return try DaemonWakeListener(path: path)
    }

    // MARK: internals

    private static let serialLock = NSLock()
    nonisolated(unsafe) private static var serial: UInt64 = 0

    private static func nextSerial() -> UInt64 {
        serialLock.lock()
        defer { serialLock.unlock() }
        serial += 1
        return serial
    }

    /// Created 0700 when missing; refused unless a real directory owned by this user that nobody
    /// else can write.
    static func ensureDirectory(_ dir: URL) throws {
        if mkdir(dir.path, 0o700) != 0, errno != EEXIST {
            throw DaemonWakeError.system(operation: "mkdir", errno: errno)
        }
        var st = stat()
        guard lstat(dir.path, &st) == 0 else { throw DaemonWakeError.system(operation: "lstat", errno: errno) }
        guard (st.st_mode & S_IFMT) == S_IFDIR, st.st_uid == geteuid(), st.st_mode & 0o022 == 0 else {
            throw DaemonWakeError.insecureDirectory(dir.path)
        }
    }

    static func withSockaddr<T>(
        _ path: String, _ body: (UnsafePointer<sockaddr>, socklen_t) throws -> T
    ) throws -> T {
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        let bytes = Array(path.utf8)
        guard bytes.count < capacity else { throw DaemonWakeError.pathTooLong(path) }
        withUnsafeMutableBytes(of: &addr.sun_path) { raw in
            for (i, b) in bytes.enumerated() { raw[i] = b }
            raw[bytes.count] = 0
        }
        addr.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let length = socklen_t(MemoryLayout<sockaddr_un>.size)
        return try withUnsafePointer(to: &addr) { pointer in
            try pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { try body($0, length) }
        }
    }
}

/// One listener on a wake name. `wait` blocks the calling thread, so call it off the main thread, or
/// use `wakes()`.
public final class DaemonWakeListener: @unchecked Sendable {
    private let fd: Int32
    private let path: String
    private let lock = NSLock()

    init(path: String) throws {
        let fd = socket(AF_UNIX, SOCK_DGRAM, 0)
        guard fd >= 0 else { throw DaemonWakeError.system(operation: "socket", errno: errno) }
        _ = fcntl(fd, F_SETFD, FD_CLOEXEC)
        unlink(path)
        do {
            let bound: Int32 = try DaemonWake.withSockaddr(path) { addr, length in bind(fd, addr, length) }
            guard bound == 0 else { throw DaemonWakeError.system(operation: "bind", errno: errno) }
            guard chmod(path, 0o600) == 0 else { throw DaemonWakeError.system(operation: "chmod", errno: errno) }
        } catch {
            close(fd)
            unlink(path)
            throw error
        }
        self.fd = fd
        self.path = path
    }

    deinit {
        close(fd)
        unlink(path)
    }

    /// Wait up to `timeout` seconds for a wake: `true` when woken (every post since the last call
    /// counts once), `false` at the timeout.
    public func wait(timeout: TimeInterval) throws -> Bool {
        lock.lock()
        defer { lock.unlock() }
        var pfd = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
        let ms = Int32(min(max(timeout * 1000, 1), Double(Int32.max - 1)))
        let ready = poll(&pfd, 1, ms)
        if ready < 0 {
            if errno == EINTR { return false }
            throw DaemonWakeError.system(operation: "poll", errno: errno)
        }
        if ready == 0 { return false }
        // Coalesce: everything already queued is the same wake.
        var buffer = [UInt8](repeating: 0, count: 16)
        var received = false
        while recv(fd, &buffer, buffer.count, MSG_DONTWAIT) >= 0 { received = true }
        return received
    }

    /// Wakes as an async stream. The stream ends, and the polling task stops, when the consumer
    /// stops iterating; a listener failure finishes it with the error.
    public func wakes(pollInterval: TimeInterval = 1) -> AsyncThrowingStream<Void, Error> {
        AsyncThrowingStream { continuation in
            let task = Task.detached { [self] in
                do {
                    while !Task.isCancelled {
                        if try wait(timeout: pollInterval) { continuation.yield(()) }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}

/// A Darwin notification observer (`notify_register_dispatch`): a system-wide, data-free name any
/// process of the machine may post with `notify_post`. Use it for names other Apple-side components
/// post; use `DaemonWake` to talk to `rightkit-daemon` processes, which do not post Darwin
/// notifications (rightkit#404 decision: the daemon stays portable). A Darwin notification is not
/// scoped to the user or the suite, so never treat it as authorization.
public final class DarwinNotifyObserver: @unchecked Sendable {
    private var token: Int32 = NOTIFY_TOKEN_INVALID
    private let lock = NSLock()

    /// Register for `name`; `handler` runs on `queue` for each post (posts may coalesce).
    public init(name: String, queue: DispatchQueue = .main, handler: @escaping @Sendable () -> Void) throws {
        var newToken: Int32 = NOTIFY_TOKEN_INVALID
        let status = notify_register_dispatch(name, &newToken, queue) { _ in handler() }
        guard status == UInt32(NOTIFY_STATUS_OK) else {
            throw DaemonWakeError.system(operation: "notify_register_dispatch", errno: Int32(truncatingIfNeeded: status))
        }
        token = newToken
    }

    /// Stop observing. Safe to call more than once.
    public func cancel() {
        lock.lock()
        defer { lock.unlock() }
        guard token != NOTIFY_TOKEN_INVALID else { return }
        notify_cancel(token)
        token = NOTIFY_TOKEN_INVALID
    }

    deinit { cancel() }

    /// Post a Darwin notification for `name`.
    public static func post(_ name: String) throws {
        let status = notify_post(name)
        guard status == UInt32(NOTIFY_STATUS_OK) else {
            throw DaemonWakeError.system(operation: "notify_post", errno: Int32(truncatingIfNeeded: status))
        }
    }
}
#endif
