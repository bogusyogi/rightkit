import Foundation

/// Failures of the helper wire protocol. Messages are safe to put in a response `error` field
/// (they never echo request contents).
public enum HelperProtocolError: Error, Equatable, Sendable, CustomStringConvertible {
    case emptyRequest
    case invalidJSON
    case lineTooLong(limit: Int)
    case frameTooLarge(limit: Int)
    case truncatedFrame

    public var description: String {
        switch self {
        case .emptyRequest: return "no request on stdin"
        case .invalidJSON: return "invalid request JSON"
        case let .lineTooLong(limit): return "request line exceeds \(limit) bytes"
        case let .frameTooLarge(limit): return "frame exceeds \(limit) bytes"
        case .truncatedFrame: return "truncated frame"
        }
    }
}

/// The `{"ok":false,"error":"..."}` failure line every helper emits. Wire-compatible with the
/// reply shape `rightkit-credentials` (`apple_helper.rs`) reads: `ok`, `error`, `value`.
public struct HelperFailure: Codable, Equatable, Sendable {
    public let ok: Bool
    public let error: String
    public init(_ error: String) { self.ok = false; self.error = error }
}

/// One item produced by `HelperLineBuffer`.
public enum HelperLineEvent: Equatable, Sendable {
    /// A complete line, without its newline (a trailing CR is dropped).
    case line(String)
    /// A line longer than the limit; its bytes were discarded up to the next newline.
    case overflow(limit: Int)
}

/// Line-JSON and length-framed owner for Swift CLI helpers that a Rust host drives over
/// stdin/stdout (for example `ScrapeRightAppleCredentials`, spawned by `pro_bridge.rs`, and the
/// `rightkit-credentials` Apple helper).
///
/// Line protocol: one UTF-8 JSON object per line on stdin, one per line on stdout, no framing
/// beyond the newline. A spawn-per-request helper uses `runOnce`; a long-lived one uses `serve`.
/// Framed protocol (long-lived helpers, `rightkit-record`): 4-byte big-endian length, then that
/// many bytes of JSON.
public enum HelperProtocol {
    public static let defaultMaxLineBytes = 1 << 20
    public static let defaultMaxFrameBytes = 16 << 20

    // MARK: Line protocol

    /// Decodes one request line. Empty or whitespace-only lines are `.emptyRequest`.
    public static func decodeRequestLine<R: Decodable>(
        _ line: String?,
        as type: R.Type = R.self,
        maxBytes: Int = HelperProtocol.defaultMaxLineBytes
    ) -> Result<R, HelperProtocolError> {
        guard let line, !line.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return .failure(.emptyRequest)
        }
        let data = Data(line.utf8)
        guard data.count <= maxBytes else { return .failure(.lineTooLong(limit: maxBytes)) }
        guard let value = try? JSONDecoder().decode(R.self, from: data) else { return .failure(.invalidJSON) }
        return .success(value)
    }

    /// Encodes one response as a single line (compact, sorted keys, no trailing newline).
    /// A value that cannot be encoded degrades to the failure line rather than crashing.
    public static func encodeResponseLine<T: Encodable>(_ value: T) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        if let data = try? encoder.encode(value), let line = String(data: data, encoding: .utf8) {
            return line.replacingOccurrences(of: "\n", with: "")
        }
        return #"{"error":"failed to encode response","ok":false}"#
    }

    /// Reads one request line, runs `handler`, and emits exactly one response line. Returns the
    /// process exit code a spawn-per-request helper should use (0 handled, 1 protocol failure;
    /// handler-level failures are normal `ok:false` responses and exit 0).
    @discardableResult
    public static func runOnce<R: Decodable, T: Encodable>(
        readLine: () -> String?,
        write: (String) -> Void,
        handler: (R) -> T
    ) -> Int32 {
        switch decodeRequestLine(readLine(), as: R.self) {
        case let .success(request):
            write(encodeResponseLine(handler(request)))
            return 0
        case let .failure(error):
            write(encodeResponseLine(HelperFailure(error.description)))
            return 1
        }
    }

    // MARK: Request/response loop

    /// Serves a line-delimited JSON request/response loop until `read` reports EOF.
    ///
    /// - `read` returns the next chunk of input bytes; an empty `Data` is EOF.
    /// - `write` emits one complete response line (no newline; the caller's sink appends it) and
    ///   must flush it before returning. If it throws (for example the peer closed the pipe), the
    ///   loop stops and returns 1.
    /// - `handler` runs once per well-formed request and yields the response value.
    ///
    /// Blank lines are ignored. A malformed line or an oversized line yields one
    /// `{"ok":false,"error":...}` response and the loop continues with the next line; the loop
    /// never crashes on hostile input. Returns 0 on clean EOF. A final line with no trailing
    /// newline is still handled.
    @discardableResult
    public static func serve<R: Decodable, T: Encodable>(
        maxLineBytes: Int = HelperProtocol.defaultMaxLineBytes,
        read: () throws -> Data,
        write: (String) throws -> Void,
        handler: (R) -> T
    ) -> Int32 {
        var buffer = HelperLineBuffer(maxLineBytes: maxLineBytes)

        func respond(_ event: HelperLineEvent) throws {
            switch event {
            case let .overflow(limit):
                try write(encodeResponseLine(HelperFailure(HelperProtocolError.lineTooLong(limit: limit).description)))
            case let .line(line):
                if line.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return }
                switch decodeRequestLine(line, as: R.self, maxBytes: maxLineBytes) {
                case let .success(request): try write(encodeResponseLine(handler(request)))
                case let .failure(error): try write(encodeResponseLine(HelperFailure(error.description)))
                }
            }
        }

        do {
            while true {
                let chunk = try read()
                if chunk.isEmpty { break }
                for event in buffer.append(chunk) { try respond(event) }
            }
            for event in buffer.finish() { try respond(event) }
        } catch {
            return 1
        }
        return 0
    }

    /// `serve` over file handles (typically `.standardInput` and `.standardOutput`, passed by
    /// the caller; this module holds no global handles). Each response is written with its
    /// newline in one `write`, which is unbuffered, so it is flushed per line.
    @discardableResult
    public static func serve<R: Decodable, T: Encodable>(
        input: FileHandle,
        output: FileHandle,
        maxLineBytes: Int = HelperProtocol.defaultMaxLineBytes,
        handler: (R) -> T
    ) -> Int32 {
        serve(
            maxLineBytes: maxLineBytes,
            read: { try input.read(upToCount: 64 * 1024) ?? Data() },
            write: { try output.write(contentsOf: Data(($0 + "\n").utf8)) },
            handler: handler)
    }

    // MARK: Framed protocol

    /// 4-byte big-endian length prefix followed by `payload`.
    public static func frame(
        _ payload: Data,
        maxBytes: Int = HelperProtocol.defaultMaxFrameBytes
    ) throws -> Data {
        guard payload.count <= maxBytes else { throw HelperProtocolError.frameTooLarge(limit: maxBytes) }
        var length = UInt32(payload.count).bigEndian
        var out = Data(bytes: &length, count: 4)
        out.append(payload)
        return out
    }

    /// Extracts one complete frame from the front of `buffer`. Returns nil while more bytes are
    /// needed; throws on an oversized declared length. On success the frame is removed from `buffer`.
    public static func nextFrame(
        from buffer: inout Data,
        maxBytes: Int = HelperProtocol.defaultMaxFrameBytes
    ) throws -> Data? {
        guard buffer.count >= 4 else { return nil }
        let base = buffer.startIndex
        let length = buffer[base..<base + 4].reduce(0) { ($0 << 8) | Int($1) }
        guard length <= maxBytes else { throw HelperProtocolError.frameTooLarge(limit: maxBytes) }
        guard buffer.count >= 4 + length else { return nil }
        let payload = Data(buffer[base + 4..<base + 4 + length])
        buffer = Data(buffer[(base + 4 + length)...])
        return payload
    }
}

/// Accumulates input bytes and yields complete newline-terminated lines, bounded so a peer that
/// never sends a newline cannot grow memory without limit. An oversized line is reported once as
/// `.overflow` and its remaining bytes are discarded up to the next newline, so the stream
/// resynchronises instead of failing.
public struct HelperLineBuffer: Sendable {
    private var pending = Data()
    private var discarding = false
    public let maxLineBytes: Int

    public init(maxLineBytes: Int = HelperProtocol.defaultMaxLineBytes) { self.maxLineBytes = maxLineBytes }

    /// Appends bytes and returns every event they complete.
    public mutating func append(_ bytes: Data) -> [HelperLineEvent] {
        var events: [HelperLineEvent] = []
        var rest = bytes[...]
        while !rest.isEmpty {
            if let newline = rest.firstIndex(of: 0x0A) {
                let head = rest[rest.startIndex..<newline]
                rest = rest[(newline + 1)...]
                if discarding {
                    discarding = false
                    continue
                }
                var line = pending
                line.append(head)
                pending = Data()
                if line.last == 0x0D { line = line.dropLast() }
                if line.count > maxLineBytes {
                    events.append(.overflow(limit: maxLineBytes))
                } else {
                    events.append(.line(String(decoding: line, as: UTF8.self)))
                }
            } else {
                if !discarding {
                    pending.append(rest)
                    if pending.count > maxLineBytes {
                        events.append(.overflow(limit: maxLineBytes))
                        pending = Data()
                        discarding = true
                    }
                }
                rest = rest[rest.endIndex...]
            }
        }
        return events
    }

    /// Flushes a final line that had no trailing newline (call at EOF).
    public mutating func finish() -> [HelperLineEvent] {
        defer { pending = Data(); discarding = false }
        guard !discarding, !pending.isEmpty else { return [] }
        var line = pending
        if line.last == 0x0D { line = line.dropLast() }
        return [.line(String(decoding: line, as: UTF8.self))]
    }
}
