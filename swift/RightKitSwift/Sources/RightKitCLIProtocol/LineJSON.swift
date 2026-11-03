import Foundation

/// Errors from the line-JSON helper protocol.
public enum LineJSONError: Error, Equatable, Sendable {
    /// A line grew past the reader's limit before its newline arrived. The reader is reset; the stream must resync.
    case lineTooLarge(limit: Int)
    /// A complete line is not valid UTF-8 JSON for the requested type.
    case malformed(String)
    /// A value could not be encoded.
    case encodingFailed(String)
}

/// Framing for the `rightkit` CLI helper protocol: one compact JSON value per line, UTF-8, newline terminated.
///
/// JSON text never contains a raw newline, so a newline is always a frame boundary. A helper or its caller
/// reads with ``LineJSONReader`` and writes with ``encode(_:)``; both sides must agree on the same limit.
public enum LineJSON {
    /// Default cap on one frame: 1 MiB.
    public static let defaultMaxLineBytes = 1_048_576

    /// One compact JSON value followed by `\n`.
    public static func encode<T: Encodable>(_ value: T) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        let body: Data
        do {
            body = try encoder.encode(value)
        } catch {
            throw LineJSONError.encodingFailed(error.localizedDescription)
        }
        var frame = body
        frame.append(0x0A)
        return frame
    }

    /// Decodes one frame body (without its newline).
    public static func decode<T: Decodable>(_ type: T.Type, from line: Data) throws -> T {
        do {
            return try JSONDecoder().decode(type, from: line)
        } catch {
            throw LineJSONError.malformed(String(describing: error))
        }
    }
}

/// Splits a byte stream into frames. Feed each chunk read from the pipe, then take the complete frames.
public struct LineJSONReader: Sendable {
    public let maxLineBytes: Int
    private var pending = Data()

    public init(maxLineBytes: Int = LineJSON.defaultMaxLineBytes) {
        self.maxLineBytes = max(1, maxLineBytes)
    }

    /// Appends a chunk and returns every frame it completes, without the newline. An incomplete tail is kept.
    /// Throws ``LineJSONError/lineTooLarge(limit:)`` and discards the buffer when a line exceeds the limit.
    public mutating func append(_ chunk: Data) throws -> [Data] {
        pending.append(chunk)
        var frames: [Data] = []
        while let newline = pending.firstIndex(of: 0x0A) {
            frames.append(Data(pending[pending.startIndex..<newline]))
            pending.removeSubrange(pending.startIndex...newline)
        }
        if pending.count > maxLineBytes {
            pending.removeAll()
            throw LineJSONError.lineTooLarge(limit: maxLineBytes)
        }
        return frames
    }

    /// Bytes buffered for the next, still incomplete, frame.
    public var bufferedByteCount: Int { pending.count }
}

/// A request in the helper protocol. `id` correlates a response to its request.
public struct LineRequest<Params: Codable & Sendable>: Codable, Sendable {
    public let id: String
    public let method: String
    public let params: Params

    public init(id: String, method: String, params: Params) {
        self.id = id
        self.method = method
        self.params = params
    }
}

/// A response in the helper protocol: either `result` on success or `error` on failure, never both.
public struct LineResponse<Output: Codable & Sendable>: Codable, Sendable {
    public let id: String
    public let ok: Bool
    public let result: Output?
    public let error: String?

    public static func success(id: String, result: Output) -> LineResponse {
        LineResponse(id: id, ok: true, result: result, error: nil)
    }

    public static func failure(id: String, message: String) -> LineResponse {
        LineResponse(id: id, ok: false, result: nil, error: message)
    }
}
