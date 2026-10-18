import Foundation
import RightKitHub

// QA and dev hooks. Release builds never link RightKitHubQA.

/// A scripted engine for previews, QA runs and screenshots. It answers `call` from registered
/// responses and pushes events on demand; it never touches a real engine.
public final class QAEngineClient: EngineClient, @unchecked Sendable {
    /// Answers one method: request payload in, response payload out, or a thrown `EngineError`.
    public typealias Handler = @Sendable (Data) throws -> Data

    private struct Subscriber {
        let topics: Set<String>
        let continuation: AsyncThrowingStream<EngineEvent, Error>.Continuation
    }

    private let lock = NSLock()
    private var handlers: [String: Handler] = [:]
    private var subscribers: [UUID: Subscriber] = [:]
    private var watchers: [UUID: AsyncStream<EngineLifecycle>.Continuation] = [:]
    private var state: EngineLifecycle
    private var methods: [String] = []

    public init(lifecycle: EngineLifecycle = .ready) {
        state = lifecycle
    }

    /// Registers the answer for a method.
    public func respond(to method: String, with handler: @escaping Handler) {
        lock.withLock { handlers[method] = handler }
    }

    /// Registers a fixed JSON answer for a method.
    public func respond(to method: String, json: String) {
        respond(to: method) { _ in Data(json.utf8) }
    }

    /// Pushes an event to every subscriber of `topic`.
    public func emit(topic: String, json: String) {
        let event = EngineEvent(topic: topic, payload: Data(json.utf8))
        let targets = lock.withLock { subscribers.values.filter { $0.topics.contains(topic) } }
        for target in targets {
            target.continuation.yield(event)
        }
    }

    /// Moves the engine to a lifecycle state and tells every watcher.
    public func setLifecycle(_ newState: EngineLifecycle) {
        let targets = lock.withLock { () -> [AsyncStream<EngineLifecycle>.Continuation] in
            state = newState
            return Array(watchers.values)
        }
        for target in targets {
            target.yield(newState)
        }
    }

    /// The methods called so far, in order.
    public func recordedCalls() -> [String] {
        lock.withLock { methods }
    }

    public func call(
        _ method: String,
        params: Data,
        timeout: Duration?,
        onEvent: (@Sendable (Data) -> Void)?
    ) async throws -> Data {
        let (handler, current) = lock.withLock { () -> (Handler?, EngineLifecycle) in
            methods.append(method)
            return (handlers[method], state)
        }
        if current != .ready {
            throw EngineError(code: "engine_unavailable", message: "QA engine is \(current.rawValue)", retryable: current != .down)
        }
        guard let handler else {
            throw EngineError(code: "method_not_found", message: "no QA response for \(method)", retryable: false)
        }
        return try handler(params)
    }

    public func subscribe(_ topics: [String]) -> AsyncThrowingStream<EngineEvent, Error> {
        AsyncThrowingStream<EngineEvent, Error> { continuation in
            let id = UUID()
            lock.withLock { subscribers[id] = Subscriber(topics: Set(topics), continuation: continuation) }
            continuation.onTermination = { [weak self] _ in
                self?.removeSubscriber(id)
            }
        }
    }

    public var lifecycle: AsyncStream<EngineLifecycle> {
        AsyncStream<EngineLifecycle> { continuation in
            let id = UUID()
            let current = lock.withLock { () -> EngineLifecycle in
                watchers[id] = continuation
                return state
            }
            continuation.yield(current)
            continuation.onTermination = { [weak self] _ in
                self?.removeWatcher(id)
            }
        }
    }

    private func removeSubscriber(_ id: UUID) {
        lock.withLock { subscribers[id] = nil }
    }

    private func removeWatcher(_ id: UUID) {
        lock.withLock { watchers[id] = nil }
    }
}
