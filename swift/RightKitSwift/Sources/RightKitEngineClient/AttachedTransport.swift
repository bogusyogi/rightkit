import Foundation

#if os(macOS)
import Darwin
#endif

// The wire side of `AttachedEngine`: one connection to the resident engine's `rightkit-daemon`
// endpoint. Frames are the engine contract's JSON lines (`request`, `event`, `response`), preceded
// by the daemon's client `hello`:
//
//     client -> {"type":"hello","app":"<label>","protocol_major":1}
//     host   -> {"type":"hello","ok":true,"service":"<engine service name>","methods":[...],...}
//               or {"type":"hello","ok":false,"error":{code,message,retryable}}
//     client -> {"type":"request","id":N,"method":"<name>","params":{...}}
//     host   -> {"type":"event","id":N,"event":{...}}*  then  {"type":"response","id":N,"ok":true,"result":...}
//               or {"type":"response","id":N,"ok":false,"error":{code,message,retryable}}
//
// One request is in flight per connection (the host serves a connection in lock step), so the
// engine keeps a small pool and opens a connection per concurrent call.

/// The facts of the daemon's wire protocol this client speaks.
enum AttachedWire {
    /// `rightkit-daemon`'s `PROTOCOL_MAJOR`.
    static let protocolMajor = 1
    /// `rightkit-daemon`'s `DEFAULT_FRAME_LIMIT`: a longer line is refused.
    static let frameLimit = 16 * 1024 * 1024
    /// How long connecting, verifying the host and the `hello` may take together.
    static let helloBudget: TimeInterval = 10
    /// The longest one frame write may block (a host that stopped reading).
    static let writeBudget: Int = 30
}

/// The `EngineError`s this transport raises itself. Codes are stable; branch on them.
enum AttachedFailure {
    /// No engine is listening yet (or it answers "starting"): retry, never start one.
    static func starting(_ detail: String, engineName: String) -> EngineError {
        EngineError(code: "engine_starting", message: "\(engineName) is starting (\(detail))", retryable: true)
    }

    /// The connection ended before the answer arrived: whether the engine ran the request is unknown.
    static func deliveryUnknown() -> EngineError {
        EngineError(
            code: "delivery_unknown",
            message: "the engine connection closed before the answer arrived",
            retryable: false
        )
    }

    static func timeout(_ what: String) -> EngineError {
        EngineError(code: "timeout", message: "\(what) timed out", retryable: true)
    }

    /// The process on the other end is not the engine this client expects. Never retryable and
    /// nothing was written to it.
    static func hostIdentity(_ detail: String) -> EngineError {
        EngineError(code: "host_identity", message: "refused the engine: \(detail)", retryable: false)
    }

    static func io(_ detail: String) -> EngineError {
        EngineError(code: "io", message: detail, retryable: false)
    }

    static func protocolError(_ detail: String) -> EngineError {
        EngineError(code: "protocol", message: detail, retryable: false)
    }

    static let unsupportedPlatform = EngineError(
        code: "unsupported_platform",
        message: "attaching to the resident engine is not implemented on this platform; "
            + "a client never connects without verifying the host",
        retryable: false
    )

    /// An error frame `{code, message, retryable}` from the host.
    static func fromFrame(_ value: Any?) -> EngineError {
        let object = value as? [String: Any] ?? [:]
        return EngineError(
            code: object["code"] as? String ?? "error",
            message: object["message"] as? String ?? "",
            retryable: object["retryable"] as? Bool ?? false
        )
    }
}

/// JSON helpers shared by the transport.
enum AttachedJSON {
    /// One compact JSON line (no raw newline can appear in `JSONSerialization` output).
    static func line(_ object: [String: Any]) throws -> Data {
        guard JSONSerialization.isValidJSONObject(object) else {
            throw AttachedFailure.protocolError("the request is not valid JSON")
        }
        var data = try JSONSerialization.data(withJSONObject: object, options: [])
        data.append(0x0A)
        return data
    }

    static func object(_ line: Data) throws -> [String: Any] {
        guard let object = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else {
            throw AttachedFailure.protocolError("the engine sent a frame that is not a JSON object")
        }
        return object
    }

    /// Any JSON value (a result may be a string or a number) as undecoded JSON.
    static func value(_ value: Any?) throws -> Data {
        guard let value, !(value is NSNull) else { return Data("null".utf8) }
        return try JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed])
    }

    static func requestID(_ frame: [String: Any]) -> UInt64? {
        (frame["id"] as? NSNumber)?.uint64Value
    }
}

#if os(macOS)

/// One verified, hello'd connection. Owns its descriptor and closes it when released.
///
/// Sendable because the descriptor is a constant and the two mutable pieces (bytes read past the
/// last newline, the interrupted flag) live in `Locked`. Use from one thread at a time: the pool
/// hands a connection to one call at a time.
final class AttachedConnection: Sendable {
    private let fd: Int32
    private let engineName: String
    private let pending = Locked<[UInt8]>([])
    private let interrupted = Locked<Bool>(false)

    private init(fd: Int32, engineName: String) {
        self.fd = fd
        self.engineName = engineName
    }

    deinit {
        Darwin.close(fd)
    }

    // MARK: Open

    /// Connects to the socket at `path`, verifies that its listener is one of
    /// `expectedExecutables` running as this user, and only then sends the `hello`.
    static func open(
        path: String,
        app: String,
        engineName: String,
        expectedExecutables: [String]
    ) throws -> AttachedConnection {
        let deadline = Date().addingTimeInterval(AttachedWire.helloBudget)
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw AttachedFailure.io("socket: \(String(cString: strerror(errno)))") }
        // From here the connection owns the descriptor: any failure just drops it.
        let connection = AttachedConnection(fd: fd, engineName: engineName)
        var one: Int32 = 1
        _ = setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
        _ = fcntl(fd, F_SETFD, FD_CLOEXEC)
        var limit = timeval(tv_sec: AttachedWire.writeBudget, tv_usec: 0)
        _ = setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &limit, socklen_t(MemoryLayout<timeval>.size))
        try connection.connect(path: path)
        try connection.verifyHost(expectedExecutables: expectedExecutables)
        try connection.hello(app: app, deadline: deadline)
        return connection
    }

    private func connect(path: String) throws {
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        guard !bytes.isEmpty, bytes.count < capacity else {
            throw AttachedFailure.io("the engine endpoint path does not fit a socket address")
        }
        withUnsafeMutableBytes(of: &address.sun_path) { raw in
            raw.copyBytes(from: bytes)
            raw[bytes.count] = 0
        }
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let result = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { generic in
                Darwin.connect(fd, generic, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard result == 0 else {
            let code = errno
            // No socket, or nobody behind it: the engine is not up (yet). Anything else is real.
            if code == ENOENT || code == ECONNREFUSED || code == ENOTSOCK {
                throw AttachedFailure.starting("no engine is listening", engineName: engineName)
            }
            throw AttachedFailure.io("connect: \(String(cString: strerror(code)))")
        }
    }

    // MARK: Host verification

    /// What the kernel says about the listener, before any byte is written: the same user (by
    /// `getpeereid`), and a process whose executable is one of `expectedExecutables`. The pid
    /// (`LOCAL_PEERPID`) is proven to name one process: read before and after the path lookup,
    /// with the process start time (`proc_pidinfo`) equal both times.
    private func verifyHost(expectedExecutables: [String]) throws {
        var uid: uid_t = 0
        var gid: gid_t = 0
        guard getpeereid(fd, &uid, &gid) == 0 else {
            throw AttachedFailure.hostIdentity("the kernel would not name the engine's user")
        }
        guard uid == geteuid() else {
            throw AttachedFailure.hostIdentity("the engine runs as another user")
        }
        guard !expectedExecutables.isEmpty else {
            throw AttachedFailure.hostIdentity("no engine executable is named to compare with")
        }
        guard let first = peerPID(), let start = startTime(of: first) else {
            throw AttachedFailure.hostIdentity("the engine process could not be identified")
        }
        guard let path = executablePath(of: first) else {
            throw AttachedFailure.hostIdentity("the engine's executable could not be read")
        }
        guard startTime(of: first) == start, peerPID() == first else {
            throw AttachedFailure.hostIdentity("the engine process changed while it was checked")
        }
        guard expectedExecutables.contains(where: { Self.sameFile(path, $0) }) else {
            throw AttachedFailure.hostIdentity("the listener is not the \(engineName) engine")
        }
    }

    private func peerPID() -> pid_t? {
        // SOL_LOCAL is 0 and LOCAL_PEERPID is 0x002 in <sys/un.h>.
        var pid: pid_t = 0
        var length = socklen_t(MemoryLayout<pid_t>.size)
        guard getsockopt(fd, 0, 0x002, &pid, &length) == 0, pid > 0 else { return nil }
        return pid
    }

    private struct StartTime: Equatable {
        let seconds: UInt64
        let microseconds: UInt64
    }

    private func startTime(of pid: pid_t) -> StartTime? {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.size)
        // PROC_PIDTBSDINFO is 3 in <sys/proc_info.h>.
        guard proc_pidinfo(pid, 3, 0, &info, size) == size else { return nil }
        return StartTime(seconds: info.pbi_start_tvsec, microseconds: info.pbi_start_tvusec)
    }

    private func executablePath(of pid: pid_t) -> String? {
        var buffer = [CChar](repeating: 0, count: 4096)
        guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { return nil }
        return String(cString: buffer)
    }

    /// The same file: equal after resolving links, or the same device and inode (volumes can be
    /// case-insensitive, so two spellings can name one file; a hard link to the engine is the engine).
    private static func sameFile(_ found: String, _ expected: String) -> Bool {
        var a = stat()
        var b = stat()
        guard stat(found, &a) == 0, stat(expected, &b) == 0 else { return false }
        return a.st_dev == b.st_dev && a.st_ino == b.st_ino
    }

    // MARK: Frames

    private func hello(app: String, deadline: Date) throws {
        try write(
            AttachedJSON.line([
                "type": "hello", "app": app, "protocol_major": AttachedWire.protocolMajor,
            ])
        )
        let reply = try AttachedJSON.object(readLine(deadline: deadline))
        guard reply["type"] as? String == "hello" else {
            throw AttachedFailure.protocolError("the engine's first frame was not a hello")
        }
        guard reply["ok"] as? Bool == true else {
            throw AttachedFailure.fromFrame(reply["error"])
        }
    }

    /// Runs one request. A transport failure (the connection ended, a timeout, a frame that makes
    /// no sense) throws, and the caller must drop this connection; an error frame from the engine
    /// comes back as `.failure` and leaves the connection usable.
    func call(
        id: UInt64,
        method: String,
        params: Data,
        deadline: Date,
        onEvent: (@Sendable (Data) -> Void)?
    ) throws -> Result<Data, EngineError> {
        let parsed: Any =
            params.isEmpty
            ? [String: Any]()
            : try JSONSerialization.jsonObject(with: params, options: [.fragmentsAllowed])
        try write(
            AttachedJSON.line([
                "type": "request", "id": id, "method": method, "params": parsed,
            ])
        )
        while true {
            let frame = try AttachedJSON.object(readLine(deadline: deadline))
            switch frame["type"] as? String {
            case "event":
                if AttachedJSON.requestID(frame) == id {
                    onEvent?(try AttachedJSON.value(frame["event"]))
                }
            case "response":
                let responseID = AttachedJSON.requestID(frame)
                if responseID == id {
                    if frame["ok"] as? Bool == true {
                        return .success(try AttachedJSON.value(frame["result"]))
                    }
                    return .failure(AttachedFailure.fromFrame(frame["error"]))
                }
                if responseID == 0, frame["ok"] as? Bool == false {
                    // The host could not read the request frame at all.
                    throw AttachedFailure.fromFrame(frame["error"])
                }
                throw AttachedFailure.protocolError("a response for another request arrived")
            default:
                throw AttachedFailure.protocolError("an unexpected frame arrived")
            }
        }
    }

    private func write(_ data: Data) throws {
        try data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            guard let base = raw.baseAddress else { return }
            var offset = 0
            while offset < raw.count {
                let written = send(fd, base + offset, raw.count - offset, 0)
                if written > 0 {
                    offset += written
                } else if written < 0 && errno == EINTR {
                    continue
                } else if written < 0 && (errno == EAGAIN || errno == EWOULDBLOCK) {
                    throw AttachedFailure.timeout("writing to the engine")
                } else {
                    throw AttachedFailure.deliveryUnknown()
                }
            }
        }
    }

    /// One line (without its newline) before `deadline`. Waits in slices of at most one second
    /// so an `interrupt` is always noticed; the slices only run while a call is in flight.
    private func readLine(deadline: Date) throws -> Data {
        while true {
            if let line = takeLine() { return line }
            if interrupted.withLock({ $0 }) { throw CancellationError() }
            let remaining = deadline.timeIntervalSinceNow
            guard remaining > 0 else { throw AttachedFailure.timeout("the engine's answer") }
            var poller = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
            let milliseconds = Int32(min(max(remaining * 1000, 1), 1000))
            let ready = poll(&poller, 1, milliseconds)
            if ready < 0 {
                if errno == EINTR { continue }
                throw AttachedFailure.io("poll: \(String(cString: strerror(errno)))")
            }
            if ready == 0 { continue }
            try fill()
        }
    }

    private func fill() throws {
        var chunk = [UInt8](repeating: 0, count: 64 * 1024)
        let count = chunk.withUnsafeMutableBytes { read(fd, $0.baseAddress, $0.count) }
        if count > 0 {
            let overflow = pending.withLock { buffer -> Bool in
                buffer.append(contentsOf: chunk[0..<count])
                return buffer.count > AttachedWire.frameLimit && !buffer.contains(0x0A)
            }
            if overflow { throw AttachedFailure.protocolError("a frame is larger than the frame limit") }
        } else if count == 0 {
            throw AttachedFailure.deliveryUnknown()
        } else if errno != EINTR && errno != EAGAIN {
            throw AttachedFailure.deliveryUnknown()
        }
    }

    private func takeLine() -> Data? {
        pending.withLock { buffer -> Data? in
            guard let newline = buffer.firstIndex(of: 0x0A) else { return nil }
            let line = Data(buffer[0..<newline])
            buffer.removeSubrange(0...newline)
            return line
        }
    }

    // MARK: Control

    /// Wakes whatever is blocked on this connection and makes it fail. The descriptor stays open
    /// (so it cannot be reused under a blocked thread); it closes when the connection is released.
    func interrupt() {
        interrupted.withLock { $0 = true }
        _ = shutdown(fd, SHUT_RDWR)
    }

    /// Whether an idle connection may be used again: nothing buffered, no hang-up, no stray
    /// bytes (an idle connection has nothing to read; readable means closed or confused).
    var isReusable: Bool {
        if interrupted.withLock({ $0 }) { return false }
        if !pending.withLock({ $0.isEmpty }) { return false }
        var poller = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
        return poll(&poller, 1, 0) == 0
    }

    /// Blocks until the host closes the connection or `interrupt` is called. An idle connection
    /// has nothing to read, so this costs one wake-up per `slice` seconds and none otherwise.
    func waitUntilClosed(slice: Int32 = 30) {
        while !interrupted.withLock({ $0 }) {
            var poller = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
            let ready = poll(&poller, 1, slice * 1000)
            if ready > 0 { return }
            if ready < 0 && errno != EINTR { return }
        }
    }
}

#else

/// Other platforms: no attached transport yet. Every open fails closed, naming the reason.
final class AttachedConnection: Sendable {
    private init() {}

    static func open(path: String, app: String, engineName: String, expectedExecutables: [String]) throws -> AttachedConnection {
        throw AttachedFailure.unsupportedPlatform
    }

    func call(
        id: UInt64,
        method: String,
        params: Data,
        deadline: Date,
        onEvent: (@Sendable (Data) -> Void)?
    ) throws -> Result<Data, EngineError> {
        throw AttachedFailure.unsupportedPlatform
    }

    func interrupt() {}
    var isReusable: Bool { false }
    func waitUntilClosed(slice: Int32 = 30) {}
}

#endif
