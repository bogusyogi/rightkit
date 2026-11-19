// Client of the authenticated action channel (macOS only; empty elsewhere).
//
// An ACTION (toggle recording, quit, open a window, any Shortcuts intent) is carried out by the
// main app only after the app has identified the sender: the app listens on a Unix-domain socket
// and checks the kernel's audit token of the connecting process against a code requirement (the
// product's Team ID plus an allowed signing identifier). This file is the sending half. It owns
// no authority; it cannot make the app trust it, it can only present itself over a connection
// the operating system will vouch for.
//
// It also checks the OTHER end: before sending anything it verifies that the process listening on
// the socket satisfies `serverRequirement` (the main app's signing identity), so a process that
// squatted the socket path in the per-user temp directory receives nothing.
//
// Protocol (newline-delimited JSON, each line at most 2048 bytes):
//   server -> client  {"v":1,"nonce":"<64 hex>"}                         once, after the app verified us
//   client -> server  {"nonce":"<echo>","seq":1,"id":"<uuid>","action":"<name>"}
//   server -> client  {"seq":1,"ok":true}  or  {"seq":1,"ok":false,"reason":"<code>"}
// `seq` starts at 1 and advances by one per frame on a connection. The server refuses a wrong
// nonce, an out-of-order sequence, a repeated id and an action the sender class may not send, and
// closes the connection on the first refusal.
#if os(macOS)
import Darwin
import Foundation
import Security

public enum ActionChannelError: Error, CustomStringConvertible, Sendable {
    /// Nothing is listening (app not running, or not yet set up).
    case unavailable
    /// Something is listening but is not the app: nothing was sent.
    case serverNotVerified
    /// The app answered and refused. `reason` is a short code.
    case refused(String)
    /// A frame could not be read or written.
    case io(String)

    /// Neutral wording; a product that shows the text to a user calls ``describe(appName:)``.
    public var description: String { describe(appName: "the app") }

    /// Render the error with the host's display name.
    public func describe(appName: String) -> String {
        switch self {
        case .unavailable: return "\(appName) is not accepting actions"
        case .serverNotVerified: return "the listener is not \(appName)"
        case .refused(let reason): return "\(appName) refused the action (\(reason))"
        case .io(let detail): return "action channel error (\(detail))"
        }
    }
}

/// `SOL_LOCAL` and `LOCAL_PEERTOKEN` from <sys/un.h>, written out so the build does not depend on
/// the SDK exposing them to Swift.
private let kSolLocal: Int32 = 0
private let kLocalPeerToken: Int32 = 0x006
private let kMaxFrameBytes = 2048

/// One connection to the app's action socket. Not thread-safe: use one instance from one queue.
public final class ActionChannelConnection: @unchecked Sendable {
    private let socketPath: String
    private let serverRequirement: String
    private let ioTimeout: TimeInterval
    private var fd: Int32 = -1
    private var nonce = ""
    private var seq: UInt64 = 0
    private var pending = [UInt8]()
    /// Frame ids whose frame this object has already written to a socket. A retry of one of them
    /// that the app refuses as a replay means the first copy was accepted; see `sendOnce`.
    private var written = Set<String>()

    /// - Parameters:
    ///   - socketName: the stem the app was configured with (`<per-user temp dir>/<socketName>.sock`).
    ///   - serverRequirement: code requirement the listening process must satisfy.
    ///   - ioTimeout: per read/write timeout in seconds.
    public init(socketName: String, serverRequirement: String, ioTimeout: TimeInterval = 5) {
        self.socketPath = ActionChannelConnection.userTempDirectory() + socketName + ".sock"
        self.serverRequirement = serverRequirement
        self.ioTimeout = ioTimeout
    }

    deinit { close() }

    public var isConnected: Bool { fd >= 0 }

    public func close() {
        if fd >= 0 { Darwin.close(fd) }
        fd = -1
        nonce = ""
        seq = 0
        pending.removeAll()
    }

    /// Connect, verify the server, and read the greeting. Idempotent while connected.
    public func connect() throws {
        if fd >= 0 { return }
        let s = socket(AF_UNIX, SOCK_STREAM, 0)
        guard s >= 0 else { throw ActionChannelError.io("socket") }
        // Never leak this descriptor into a child process: a child holding it could speak as us.
        _ = fcntl(s, F_SETFD, FD_CLOEXEC)
        var one: Int32 = 1
        _ = setsockopt(s, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
        var tv = timeval(tv_sec: Int(ioTimeout), tv_usec: 0)
        _ = setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        _ = setsockopt(s, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))

        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(socketPath.utf8CString)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        guard bytes.count <= capacity else {
            Darwin.close(s)
            throw ActionChannelError.io("socket path too long")
        }
        withUnsafeMutablePointer(to: &address.sun_path) { pointer in
            pointer.withMemoryRebound(to: CChar.self, capacity: capacity) { destination in
                for (index, byte) in bytes.enumerated() { destination[index] = byte }
            }
        }
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let connected = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(s, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else {
            Darwin.close(s)
            throw ActionChannelError.unavailable
        }

        // Verify WHO is listening before a single byte is sent.
        guard ActionChannelConnection.peerSatisfies(fd: s, requirement: serverRequirement) else {
            Darwin.close(s)
            throw ActionChannelError.serverNotVerified
        }
        fd = s
        pending.removeAll()
        // The server only greets a peer it verified; a closed connection here means it did not.
        guard let line = try readLine(), let greeting = ActionChannelConnection.object(line),
              let value = greeting["nonce"] as? String, (greeting["v"] as? Int) == 1
        else {
            close()
            throw ActionChannelError.refused("unverified")
        }
        nonce = value
        seq = 0
    }

    /// Send one action and wait for the app's acknowledgement. Reconnects once when the connection
    /// was stale. The ack means the app accepted the action from a verified sender; it does not
    /// mean the app's own policy then ran it. `id` names this one action: a caller that retries the
    /// same action after a failure passes the same `id`, so a copy the app already accepted is not
    /// run twice (the app refuses a repeated id; that refusal of a resend counts as delivered).
    public func send(_ action: String, id: String = UUID().uuidString.lowercased()) throws {
        do {
            try sendOnce(action, id: id)
        } catch ActionChannelError.io {
            close()
            try sendOnce(action, id: id)
        } catch ActionChannelError.unavailable {
            close()
            throw ActionChannelError.unavailable
        }
    }

    private func sendOnce(_ action: String, id: String) throws {
        try connect()
        seq += 1
        let isResend = written.contains(id)
        let frame: [String: Any] = [
            "nonce": nonce,
            "seq": seq,
            "id": id,
            "action": action,
        ]
        guard var data = try? JSONSerialization.data(withJSONObject: frame, options: [.sortedKeys]) else {
            throw ActionChannelError.io("encode")
        }
        data.append(0x0A)
        written.insert(id)
        try writeAll(data)
        guard let reply = try readLine(), let object = ActionChannelConnection.object(reply) else {
            close()
            throw ActionChannelError.io("no reply")
        }
        if (object["ok"] as? Bool) == true { return }
        let reason = (object["reason"] as? String) ?? "unknown"
        close()
        // The app refuses a repeated id. For an id this object already wrote, that means the first
        // copy reached the app and only its acknowledgement was lost: delivered, not refused.
        if isResend && reason == "replay" { return }
        throw ActionChannelError.refused(reason)
    }

    // MARK: socket I/O

    private func writeAll(_ data: Data) throws {
        var offset = 0
        let total = data.count
        try data.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) in
            while offset < total {
                let written = Darwin.write(fd, buffer.baseAddress! + offset, total - offset)
                if written < 0 {
                    if errno == EINTR { continue }
                    close()
                    throw ActionChannelError.io("write")
                }
                offset += written
            }
        }
    }

    private func readLine() throws -> [UInt8]? {
        while true {
            if let end = pending.firstIndex(of: 0x0A) {
                let line = Array(pending[..<end])
                pending.removeSubrange(...end)
                return line
            }
            if pending.count > kMaxFrameBytes {
                close()
                throw ActionChannelError.io("frame too long")
            }
            var chunk = [UInt8](repeating: 0, count: 512)
            let count = Darwin.read(fd, &chunk, chunk.count)
            if count == 0 { return nil }
            if count < 0 {
                if errno == EINTR { continue }
                close()
                throw ActionChannelError.io("read")
            }
            pending.append(contentsOf: chunk[..<count])
        }
    }

    private static func object(_ line: [UInt8]) -> [String: Any]? {
        (try? JSONSerialization.jsonObject(with: Data(line))) as? [String: Any]
    }

    // MARK: identity

    /// `confstr(_CS_DARWIN_USER_TEMP_DIR)`, the same per-user directory the app uses, independent of
    /// the TMPDIR environment of whoever launched this process. Ends with a slash.
    private static func userTempDirectory() -> String {
        var buffer = [CChar](repeating: 0, count: 1024)
        let written = confstr(_CS_DARWIN_USER_TEMP_DIR, &buffer, buffer.count)
        guard written > 0, written <= buffer.count else { return NSTemporaryDirectory() }
        return String(cString: buffer)
    }

    /// Does the process on the other end of `fd` (identified by the kernel's audit token captured
    /// when the connection was made) satisfy `requirement`?
    private static func peerSatisfies(fd: Int32, requirement: String) -> Bool {
        var token = [UInt32](repeating: 0, count: 8)
        var length = socklen_t(MemoryLayout<UInt32>.size * 8)
        guard getsockopt(fd, kSolLocal, kLocalPeerToken, &token, &length) == 0,
              Int(length) == MemoryLayout<UInt32>.size * 8
        else { return false }
        let tokenData = token.withUnsafeBytes { Data($0) }
        var code: SecCode?
        let attributes = [kSecGuestAttributeAudit: tokenData] as CFDictionary
        guard SecCodeCopyGuestWithAttributes(nil, attributes, [], &code) == errSecSuccess, let guest = code
        else { return false }
        var parsed: SecRequirement?
        guard SecRequirementCreateWithString(requirement as CFString, [], &parsed) == errSecSuccess,
              let compiled = parsed
        else { return false }
        return SecCodeCheckValidity(guest, [], compiled) == errSecSuccess
    }
}

/// The requirement for "signed by team `teamID` with identifier `identifier`, as a Developer ID
/// Application". The same predicate the app applies to each sender
/// (`rightkit_daemon::SigningIdentity::MacDeveloperId`): both directions use one rule.
public func actionChannelRequirement(teamID: String, identifier: String) -> String {
    "anchor apple generic and certificate leaf[subject.OU] = \"\(teamID)\" and identifier \"\(identifier)\" "
        + "and certificate 1[field.1.2.840.113635.100.6.2.6] "
        + "and certificate leaf[field.1.2.840.113635.100.6.1.13]"
}
#endif
