import Foundation

// The engine contract as Swift sees it. The engine is a `rightkit-job-queue` process: a request
// and response protocol (`hello`, request, response, events) with push events through the shared
// `events.subscribe {topics}` convention. This file holds only the client-side protocol and
// value types; no transport lives here. See `SupervisedEngine` and `AttachedEngine`.

/// One pushed event frame, `{topic, payload}`, from `events.subscribe`.
public struct EngineEvent: Sendable, Equatable {
    /// The topic the frame was published on, for example `settings`.
    public let topic: String
    /// The frame payload, undecoded JSON. The subscriber knows the shape of its topics.
    public let payload: Data

    public init(topic: String, payload: Data) {
        self.topic = topic
        self.payload = payload
    }
}

/// An engine failure, matching the job-queue error frame `{code, message, retryable}`.
public struct EngineError: Error, Sendable, Equatable, Codable, CustomStringConvertible {
    /// A stable machine code, for example `engine_unavailable`.
    public let code: String
    /// A human-readable message. Never parse it; branch on `code`.
    public let message: String
    /// True when repeating the same call can succeed (the engine was busy or recovering).
    public let retryable: Bool

    public init(code: String, message: String, retryable: Bool) {
        self.code = code
        self.message = message
        self.retryable = retryable
    }

    public var description: String { "\(code): \(message)" }

    /// The error every client throws until the native binding is linked.
    public static let bindingNotLinked = EngineError(
        code: "engine_unavailable",
        message: "native binding not linked",
        retryable: false
    )
}

/// The engine's lifecycle as a client sees it.
public enum EngineLifecycle: String, Sendable, Equatable, Codable {
    /// The engine is starting; calls wait or fail with a retryable error.
    case booting
    /// The engine answers calls.
    case ready
    /// The engine stopped unexpectedly and the supervisor is restarting it.
    case recovering
    /// The engine is not running and will not be restarted by this client.
    case down
}

/// A client of one engine. The hub, the notch, widgets and iOS all talk to the engine through
/// this protocol and never reach the Rust core any other way.
public protocol EngineClient: Sendable {
    /// Sends one request and returns the response payload.
    ///
    /// - Parameters:
    ///   - method: The command name, for example `settings.get`.
    ///   - params: The request payload, JSON.
    ///   - timeout: Longest wait for the response, or nil for the client's default.
    ///   - onEvent: Called with each progress frame the command streams before its response.
    /// - Throws: `EngineError` for engine failures; `CancellationError` if the task is cancelled.
    func call(
        _ method: String,
        params: Data,
        timeout: Duration?,
        onEvent: (@Sendable (Data) -> Void)?
    ) async throws -> Data

    /// Streams `{topic, payload}` frames for the given topics (`events.subscribe`). The stream ends
    /// only when the consumer cancels or the engine shuts down; an engine failure ends it with an error.
    func subscribe(_ topics: [String]) -> AsyncThrowingStream<EngineEvent, Error>

    /// The lifecycle, starting with the current state. Never ends while the client is alive.
    var lifecycle: AsyncStream<EngineLifecycle> { get }
}

public extension EngineClient {
    /// `call` without progress frames.
    func call(_ method: String, params: Data, timeout: Duration? = nil) async throws -> Data {
        try await call(method, params: params, timeout: timeout, onEvent: nil)
    }

    /// A typed call: encodes `params` and decodes the response with `JSONEncoder` and `JSONDecoder`.
    ///
    /// Use ``EngineEmpty`` for commands that take or return `{}`.
    func call<P: Encodable, R: Decodable>(
        _ method: String,
        _ params: P,
        as type: R.Type,
        timeout: Duration? = nil
    ) async throws -> R {
        let body = try JSONEncoder().encode(params)
        let data = try await call(method, params: body, timeout: timeout, onEvent: nil)
        return try JSONDecoder().decode(type, from: data)
    }
}

/// The empty JSON object `{}`, for commands with no parameters or no result.
public struct EngineEmpty: Codable, Sendable, Equatable {
    public init() {}
}
