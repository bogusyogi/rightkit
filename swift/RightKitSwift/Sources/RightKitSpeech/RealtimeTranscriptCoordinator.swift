import Foundation

/// Shared state boundary for future streaming ASR updates. It deliberately
/// stores partial text only when the existing ASR pipeline supplies it; this
/// type never invents text or changes the macOS-owned recognition semantics.
public final class RealtimeTranscriptCoordinator {
    public struct State: Equatable {
        let sessionID: String
        let partialText: String
        let updatedAt: Date
        let isFinal: Bool
    }

    private let lock = NSLock()
    private var state: State?

    public func start(sessionID: String) {
        lock.lock()
        state = State(sessionID: sessionID, partialText: "", updatedAt: Date(), isFinal: false)
        lock.unlock()
    }

    public func publish(partialText: String, sessionID: String) {
        lock.lock()
        guard state?.sessionID == sessionID else {
            lock.unlock()
            return
        }
        state = State(
            sessionID: sessionID,
            partialText: partialText,
            updatedAt: Date(),
            isFinal: false
        )
        lock.unlock()
    }

    public func finish(sessionID: String, finalText: String) {
        lock.lock()
        guard state?.sessionID == sessionID else {
            lock.unlock()
            return
        }
        state = State(sessionID: sessionID, partialText: finalText, updatedAt: Date(), isFinal: true)
        lock.unlock()
    }

    public func cancel(sessionID: String) {
        lock.lock()
        if state?.sessionID == sessionID { state = nil }
        lock.unlock()
    }

    public var current: State? {
        lock.lock(); defer { lock.unlock() }
        return state
    }
}
