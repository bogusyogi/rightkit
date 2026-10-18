import Foundation

// MARK: - Event delivery

/// Hands the progress frames of one call from the native callback thread to the caller's
/// `onEvent`. `push` only copies into an `AsyncStream` buffer (never blocks); a task of its own
/// drains the buffer into the handler, so a slow handler cannot hold up the engine.
///
/// Sendable without an opt-out: every stored property is an immutable, Sendable value (a stream
/// continuation and a task).
package final class EventPump: Sendable {
    private let continuation: AsyncStream<Data>.Continuation
    private let task: Task<Void, Never>

    package init(handler: @escaping @Sendable (Data) -> Void) {
        let (stream, continuation) = AsyncStream<Data>.makeStream(bufferingPolicy: .unbounded)
        self.continuation = continuation
        self.task = Task {
            for await frame in stream { handler(frame) }
        }
    }

    /// Called from the native callback. Copies nothing more and waits for nothing.
    package func push(_ frame: Data) {
        continuation.yield(frame)
    }

    /// No more frames will arrive; the drain task ends after the buffered ones.
    package func finish() {
        continuation.finish()
    }

    /// Returns once every buffered frame has reached the handler (after `finish`).
    package func drain() async {
        await task.value
    }

    /// Drops what is buffered and stops delivering.
    package func cancel() {
        continuation.finish()
        task.cancel()
    }
}

/// The caller's side of one native call: a continuation resumed exactly once, by the native
/// thread's result or by task cancellation, whichever comes first. A result that arrives after
/// cancellation is dropped.
package final class CallWait: Sendable {
    private enum Phase: Sendable {
        case idle
        case waiting(CheckedContinuation<Data, Error>)
        case cancelledEarly
        case finished
    }

    private let phase = Locked<Phase>(.idle)

    package init() {}

    /// Registers the continuation. Returns false (after resuming it with `CancellationError`)
    /// when the task was already cancelled, in which case no native call must be started.
    package func install(_ continuation: CheckedContinuation<Data, Error>) -> Bool {
        let cancelled = phase.withLock { p -> Bool in
            switch p {
            case .idle:
                p = .waiting(continuation)
                return false
            default:
                p = .finished
                return true
            }
        }
        if cancelled { continuation.resume(throwing: CancellationError()) }
        return !cancelled
    }

    package func cancel() {
        let waiting = phase.withLock { p -> CheckedContinuation<Data, Error>? in
            switch p {
            case .idle:
                p = .cancelledEarly
                return nil
            case .waiting(let continuation):
                p = .finished
                return continuation
            case .cancelledEarly, .finished:
                return nil
            }
        }
        waiting?.resume(throwing: CancellationError())
    }

    package func complete(_ result: Result<Data, EngineError>) {
        let waiting = phase.withLock { p -> CheckedContinuation<Data, Error>? in
            guard case .waiting(let continuation) = p else { return nil }
            p = .finished
            return continuation
        }
        switch result {
        case .success(let data): waiting?.resume(returning: data)
        case .failure(let error): waiting?.resume(throwing: error)
        }
    }
}
