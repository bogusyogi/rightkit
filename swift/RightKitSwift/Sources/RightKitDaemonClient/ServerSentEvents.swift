import Foundation

/// One decoded server-sent-event frame, as `rightkit-daemon` writes it: `id: <seq>`,
/// `event: <topic>`, `data: <json>`, ended by a blank line.
public struct SSEFrame: Equatable, Sendable {
    /// The `id` field when it is an unsigned integer; `nil` for a frame without one (a `resync`).
    public var id: UInt64?
    public var event: String
    /// The `data` lines joined with `\n`.
    public var data: String

    public init(id: UInt64?, event: String, data: String) {
        self.id = id
        self.event = event
        self.data = data
    }
}

public enum SSEError: Error, Equatable, Sendable {
    /// One line is longer than the parser allows (the stream is not one this client should read).
    case lineTooLong
}

/// Incremental SSE parser. Feed it bytes as they arrive; it returns the frames that completed.
///
/// Rules match `rightkit-daemon` `loopback/replay.rs`: a line starting with `:` is a comment (the
/// keepalive) and is ignored; a blank line ends a frame, and a frame with no `data` line is not
/// delivered; `field: value` drops one leading space; an `id` that is not an unsigned integer
/// leaves the frame without one; unknown fields are ignored. Blank lines are significant, so this
/// parser reads bytes itself instead of `URLSession.AsyncBytes.lines`, which drops them.
public struct SSEParser: Sendable {
    public static let maxLineBytes = 8 * 1024 * 1024

    private var line: [UInt8] = []
    private var current = SSEFrame(id: nil, event: "", data: "")
    private var hasData = false

    public init() {}

    /// Feed bytes; returns the frames completed by them, in order.
    public mutating func feed<S: Sequence>(_ bytes: S) throws -> [SSEFrame] where S.Element == UInt8 {
        var frames: [SSEFrame] = []
        for byte in bytes {
            if byte == 0x0A {
                if line.last == 0x0D { line.removeLast() }
                if let frame = pushLine(String(decoding: line, as: UTF8.self)) { frames.append(frame) }
                line.removeAll(keepingCapacity: true)
            } else {
                line.append(byte)
                if line.count > Self.maxLineBytes { throw SSEError.lineTooLong }
            }
        }
        return frames
    }

    /// Feed one line without its terminator; a blank line ends a frame.
    public mutating func pushLine(_ text: String) -> SSEFrame? {
        if text.isEmpty {
            let frame = current
            let had = hasData
            current = SSEFrame(id: nil, event: "", data: "")
            hasData = false
            return had ? frame : nil
        }
        if text.hasPrefix(":") { return nil }
        let field: Substring
        var value: Substring
        if let colon = text.firstIndex(of: ":") {
            field = text[text.startIndex..<colon]
            value = text[text.index(after: colon)...]
            if value.hasPrefix(" ") { value = value.dropFirst() }
        } else {
            field = Substring(text)
            value = ""
        }
        switch field {
        case "id":
            current.id = UInt64(value.trimmingCharacters(in: .whitespaces))
        case "event":
            current.event = String(value)
        case "data":
            if hasData { current.data += "\n" }
            current.data.append(contentsOf: value)
            hasData = true
        default:
            break
        }
        return nil
    }
}
