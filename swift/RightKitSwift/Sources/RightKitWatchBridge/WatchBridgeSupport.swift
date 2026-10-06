import Foundation

/// Wire keys for the messages both sides of a bridge exchange. Neutral defaults; an app that must
/// stay wire-compatible with already-shipped builds overrides them.
public struct WatchBridgeKeys: Sendable {
    public var acknowledgement = "watchTransferAck"
    public var rollback = "watchTransferRollback"
    public var rollbackReason = "watchTransferRollbackReason"
    public var receiptQuery = "watchTransferReceiptQuery"
    public var receipts = "watchTransferReceipts"
    public var wakeHint = "watchTransferPhase"
    public init() {}
}

/// The slice of `WCSession` the bridge uses, so delivery logic is testable without a paired device.
public protocol WatchMessageTransport {
    var isReachable: Bool { get }
    func sendMessage(_ message: [String: Any], onError: @escaping (Error) -> Void)
    func transferUserInfo(_ userInfo: [String: Any])
}

/// Immediate-plus-durable delivery. `sendMessage` clears receiver UI at once while both apps are
/// live; `transferUserInfo` is the durable fallback that wakes a suspended or unreachable peer.
public struct DualPathSender {
    private let transport: WatchMessageTransport
    public init(transport: WatchMessageTransport) { self.transport = transport }

    /// Returns whether the immediate path was attempted.
    @discardableResult
    public func send(_ payload: [String: Any], onImmediateFailure: @escaping (Error) -> Void = { _ in }) -> Bool {
        let immediate = transport.isReachable
        if immediate { transport.sendMessage(payload, onError: onImmediateFailure) }
        transport.transferUserInfo(payload)
        return immediate
    }
}

public enum WatchReceipts {
    public static let queryLimit = 100

    /// Only ids the receiver durably holds are confirmed, preserving request order.
    public static func confirmed(requested: [String], received: Set<String>) -> [String] {
        requested.filter { received.contains($0) }
    }

    /// Queries are chunked so a large backlog never exceeds message size limits.
    public static func queryChunks(_ ids: [String]) -> [[String]] {
        stride(from: 0, to: ids.count, by: queryLimit).map { Array(ids[$0..<min($0 + queryLimit, ids.count)]) }
    }
}

public enum RelocatableFile {
    /// Stable file names survive sandbox relocation; absolute URLs do not.
    public static func url(fileName: String, in directory: URL) -> URL {
        directory.appendingPathComponent(URL(fileURLWithPath: fileName).lastPathComponent)
    }
}

/// Bounded most-recent-first set of strings in `UserDefaults`: acknowledged file names awaiting
/// orphan sweep, or fingerprints of results already shown (dedupe).
public struct BoundedStringLog: @unchecked Sendable {
    private let defaults: UserDefaults
    private let key: String
    private let limit: Int

    public init(defaults: UserDefaults = .standard, key: String, limit: Int = 250) {
        self.defaults = defaults
        self.key = key
        self.limit = limit
    }

    public var all: [String] { defaults.stringArray(forKey: key) ?? [] }
    public func contains(_ value: String) -> Bool { all.contains(value) }

    public func remember(_ value: String) {
        guard !value.isEmpty else { return }
        var values = all
        values.removeAll { $0 == value }
        values.insert(value, at: 0)
        defaults.set(Array(values.prefix(limit)), forKey: key)
    }

    /// True the first time `value` is seen (and records it); false for a duplicate.
    public func insertIfNew(_ value: String) -> Bool {
        guard !contains(value) else { return false }
        remember(value)
        return true
    }
}

#if canImport(WatchConnectivity)
import WatchConnectivity

/// Adapter binding `WCSession` to the bridge transport protocol.
public struct WCSessionTransport: WatchMessageTransport {
    public let session: WCSession
    public init(session: WCSession = .default) { self.session = session }
    public var isReachable: Bool { session.isReachable }
    public func sendMessage(_ message: [String: Any], onError: @escaping (Error) -> Void) {
        session.sendMessage(message, replyHandler: nil, errorHandler: onError)
    }
    public func transferUserInfo(_ userInfo: [String: Any]) { _ = session.transferUserInfo(userInfo) }
}
#endif
