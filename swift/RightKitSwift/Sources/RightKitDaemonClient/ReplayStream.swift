import Foundation

/// What a ``ReplayStream`` reports (mirror of `rightkit-daemon` `ReplayEvent`, rightkit#1120).
public enum ReplayEvent: Equatable, Sendable {
    /// The stream is open (also after each reconnect).
    case connected
    /// One event, admitted: its number is above the cursor, which now equals it. `data` is the
    /// frame's data text (JSON for the daemon's own topics).
    case event(seq: UInt64, topic: String, data: String)
    /// The host could not continue from the cursor (a restart, or events dropped): re-read the
    /// state. The cursor did not move; `next` is where the stream continues.
    case resync(next: UInt64?)
    /// The connection ended; a reconnect follows after `retryIn` seconds.
    case disconnected(error: String, retryIn: TimeInterval)
}

/// The client cursor rule: an event is admitted only above the cursor, and then it is the cursor.
public struct ReplayCursor: Equatable, Sendable {
    public private(set) var last: UInt64?

    public init(last: UInt64? = nil) { self.last = last }

    /// Admit `seq` (true, the cursor moves) or drop it as a duplicate or replay (false).
    public mutating func admit(_ seq: UInt64) -> Bool {
        if let last, seq <= last { return false }
        last = seq
        return true
    }
}

public struct ReplayConfiguration: Sendable {
    /// The daemon's loopback port (the URL is always `http://127.0.0.1:<port>`).
    public var port: UInt16
    /// The bearer token.
    public var token: String
    /// The stream path, with any query, for example `/sessions/42/events`. Starts with `/`.
    public var path: String
    /// The cursor to resume from; `nil` starts live.
    public var cursor: UInt64?
    /// First reconnect delay, seconds; doubles per consecutive failure.
    public var backoffInitial: TimeInterval
    /// The reconnect delay never exceeds this, seconds.
    public var backoffMax: TimeInterval
    /// A connected stream silent this long (the host sends a keepalive every 15 s) is treated as
    /// dead, seconds.
    public var idleTimeout: TimeInterval

    public init(
        port: UInt16, token: String, path: String, cursor: UInt64? = nil,
        backoffInitial: TimeInterval = 0.25, backoffMax: TimeInterval = 10, idleTimeout: TimeInterval = 45
    ) {
        self.port = port
        self.token = token
        self.path = path
        self.cursor = cursor
        self.backoffInitial = backoffInitial
        self.backoffMax = backoffMax
        self.idleTimeout = idleTimeout
    }

    /// The delay after `failures` consecutive failures: initial doubled per failure (at most 2^10),
    /// capped at the maximum, never below 1 ms.
    public func backoff(afterFailures failures: Int) -> TimeInterval {
        let doubled = backoffInitial * Double(1 << min(max(failures, 0), 10))
        return max(0.001, min(doubled, backoffMax))
    }
}

public enum ReplayStreamError: Error, Equatable, Sendable {
    case invalidPath(String)
    case status(Int)
    case notHTTP
}

/// Refuses every redirect, so the bearer token never leaves the loopback address.
private final class NoRedirectDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(
        _ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }
}

/// An event stream that resumes from the last sequence seen (rightkit#1120), the Swift half of
/// `rightkit-daemon` `loopback/replay.rs`.
///
/// It sends `Last-Event-ID: <cursor>` (none when the cursor is `nil`: live only), reconnects with
/// bounded exponential backoff from its own cursor, admits an event only when its number is above
/// the cursor (a replayed or duplicated frame is dropped), reports a `resync` frame, which has no
/// `id`, without moving the cursor, and ignores keepalive comments. `cursor` is the last admitted
/// number; persist it if the app resumes across launches.
public final class ReplayStream: @unchecked Sendable {
    private let configuration: ReplayConfiguration
    private let lock = NSLock()
    private var cursorState: ReplayCursor

    public init(_ configuration: ReplayConfiguration) {
        self.configuration = configuration
        self.cursorState = ReplayCursor(last: configuration.cursor)
    }

    /// The last admitted sequence number.
    public var cursor: UInt64? {
        lock.lock()
        defer { lock.unlock() }
        return cursorState.last
    }

    private func admit(_ seq: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return cursorState.admit(seq)
    }

    /// Run the stream until the consumer stops iterating. Call once per `ReplayStream`.
    public func events() -> AsyncStream<ReplayEvent> {
        AsyncStream { continuation in
            let task = Task.detached { [self] in
                var failures = 0
                while !Task.isCancelled {
                    var message = "the stream ended"
                    do {
                        try await streamOnce(continuation: continuation, failures: &failures)
                    } catch is CancellationError {
                        break
                    } catch {
                        if Task.isCancelled { break }
                        message = String(describing: error)
                    }
                    let delay = configuration.backoff(afterFailures: failures)
                    failures += 1
                    continuation.yield(.disconnected(error: message, retryIn: delay))
                    do {
                        try await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
                    } catch {
                        break
                    }
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private func streamOnce(
        continuation: AsyncStream<ReplayEvent>.Continuation, failures: inout Int
    ) async throws {
        guard configuration.path.hasPrefix("/"),
              let url = URL(string: "http://127.0.0.1:\(configuration.port)\(configuration.path)")
        else { throw ReplayStreamError.invalidPath(configuration.path) }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.timeoutInterval = configuration.idleTimeout
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        request.setValue("Bearer \(configuration.token)", forHTTPHeaderField: "Authorization")
        if let last = cursor { request.setValue(String(last), forHTTPHeaderField: "Last-Event-ID") }

        let sessionConfiguration = URLSessionConfiguration.ephemeral
        sessionConfiguration.timeoutIntervalForRequest = configuration.idleTimeout
        sessionConfiguration.timeoutIntervalForResource = .greatestFiniteMagnitude
        let session = URLSession(configuration: sessionConfiguration)
        defer { session.finishTasksAndInvalidate() }

        let (bytes, response) = try await session.bytes(for: request, delegate: NoRedirectDelegate())
        guard let http = response as? HTTPURLResponse else { throw ReplayStreamError.notHTTP }
        guard http.statusCode == 200 else { throw ReplayStreamError.status(http.statusCode) }
        failures = 0
        continuation.yield(.connected)

        var parser = SSEParser()
        for try await byte in bytes {
            try Task.checkCancellation()
            for frame in try parser.feed(CollectionOfOne(byte)) { deliver(frame, to: continuation) }
        }
    }

    private struct ResyncBody: Decodable { var next: UInt64? }

    private func deliver(_ frame: SSEFrame, to continuation: AsyncStream<ReplayEvent>.Continuation) {
        if frame.event == "resync" {
            // A resync carries no id and never moves the cursor.
            let next = (try? JSONDecoder().decode(ResyncBody.self, from: Data(frame.data.utf8)))?.next
            continuation.yield(.resync(next: next))
            return
        }
        guard let seq = frame.id, admit(seq) else { return }
        continuation.yield(.event(seq: seq, topic: frame.event, data: frame.data))
    }
}
