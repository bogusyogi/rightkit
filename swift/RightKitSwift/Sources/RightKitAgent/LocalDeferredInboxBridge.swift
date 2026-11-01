import Foundation

/// Lossless JSON payload carried by an inbox message without binding local UI
/// to a particular producer's schema.
public indirect enum LocalDeferredInboxJSONValue: Codable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([LocalDeferredInboxJSONValue])
    case object([String: LocalDeferredInboxJSONValue])

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([LocalDeferredInboxJSONValue].self) {
            self = .array(value)
        } else {
            self = .object(try container.decode([String: LocalDeferredInboxJSONValue].self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .bool(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }
}

/// JSON-safe projection of one bounded local inbox message.
public struct LocalDeferredInboxMessage: Codable, Identifiable, Sendable {
    public let id: UInt64
    public let channel: String
    public let body: String
    public let data: LocalDeferredInboxJSONValue?
    public let enqueuedAtMs: UInt64
}

public struct LocalDeferredInboxSnapshot: Codable, Sendable {
    public let messages: [LocalDeferredInboxMessage]
    public let droppedTotal: UInt64
    public let pendingCount: Int
}

/// Thin iOS transport over mobile-runtime's deferred/inbox ABI. `LocalRuntime`
/// owns construction because it alone owns its opaque runtime handle.
public struct LocalDeferredInboxBridge: @unchecked Sendable {
    let raw: OpaquePointer

    public enum Failure: Error, LocalizedError {
        case runtime(String)
        case decoding(String)

        public var errorDescription: String? {
            switch self {
            case .runtime(let message): return message
            case .decoding(let message): return "Inbox response could not be decoded: \(message)"
            }
        }
    }

    public func deferWork(id: String) throws {
        let status = id.withCString { coderight_mobile_runtime_defer_inbox_work(raw, $0) }
        guard status == 0 else { throw Failure.runtime(lastError()) }
    }

    public func resolveWork(
        id: String,
        content: String,
        isError: Bool = false,
        dataJSON: String? = nil
    ) throws {
        let status = id.withCString { idPointer in
            content.withCString { contentPointer in
                dataJSON?.withCString { dataPointer in
                    coderight_mobile_runtime_resolve_inbox_work(
                        raw, idPointer, contentPointer, isError, dataPointer)
                } ?? coderight_mobile_runtime_resolve_inbox_work(
                    raw, idPointer, contentPointer, isError, nil)
            }
        }
        guard status == 0 else { throw Failure.runtime(lastError()) }
    }

    @discardableResult
    public func postLocalMessage(
        channel: String,
        body: String,
        dataJSON: String? = nil
    ) throws -> UInt64 {
        let id = channel.withCString { channelPointer in
            body.withCString { bodyPointer in
                dataJSON?.withCString { dataPointer in
                    coderight_mobile_runtime_post_local_inbox_message(
                        raw, channelPointer, bodyPointer, dataPointer)
                } ?? coderight_mobile_runtime_post_local_inbox_message(
                    raw, channelPointer, bodyPointer, nil)
            }
        }
        guard id >= 0 else { throw Failure.runtime(lastError()) }
        return UInt64(id)
    }

    public func read(limit: Int = 20) throws -> LocalDeferredInboxSnapshot {
        guard let pointer = coderight_mobile_runtime_read_async_inbox(raw, Int32(limit)) else {
            throw Failure.runtime(lastError())
        }
        defer { coderight_mobile_string_free(pointer) }
        let data = Data(String(cString: pointer).utf8)
        do {
            return try JSONDecoder().decode(LocalDeferredInboxSnapshot.self, from: data)
        } catch {
            throw Failure.decoding(error.localizedDescription)
        }
    }

    @discardableResult
    public func acknowledge(through id: UInt64) throws -> Int {
        let count = coderight_mobile_runtime_acknowledge_async_inbox(raw, Int64(id))
        guard count >= 0 else { throw Failure.runtime(lastError()) }
        return Int(count)
    }

    private func lastError() -> String {
        guard let pointer = coderight_mobile_runtime_last_error(raw) else {
            return "local runtime rejected inbox request"
        }
        defer { coderight_mobile_string_free(pointer) }
        return String(cString: pointer)
    }
}
