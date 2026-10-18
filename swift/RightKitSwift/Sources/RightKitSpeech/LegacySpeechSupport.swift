#if canImport(Speech) // Speech is unavailable on watchOS; RightKitAudio carries the watch recorder
import Foundation
import Speech

/// Local-only policy for the pre-iOS/macOS 26 `SFSpeechRecognizer` path. `SFSpeechRecognizer` sends
/// audio to Apple's servers unless the request requires on-device recognition, and a request that
/// requires it cannot run on a recognizer that lacks the model. So the policy fails closed: a
/// recognizer without on-device support is never used, and every request is forced on-device.
///
/// `allowsNetwork` is the explicit product opt-in (``LiveDictationSession`` `onDeviceOnly: false`): a
/// locale with no on-device model may then use a network-capable recognizer. A recognizer that does have
/// the model is still forced on-device.
enum LegacySpeechPolicy {
    static func unsupportedError(for locale: Locale?) -> SpeechError {
        .engineUnavailable("on-device speech recognition is unavailable for \(locale?.identifier ?? "this language")")
    }

    /// The first candidate that is available AND recognizes on-device; throws rather than falling
    /// back to a network-capable recognizer.
    static func select<Recognizer>(
        from candidates: [Recognizer],
        locale: Locale? = nil,
        allowsNetwork: Bool = false,
        isAvailable: (Recognizer) -> Bool,
        supportsOnDevice: (Recognizer) -> Bool
    ) throws -> Recognizer {
        if let local = candidates.first(where: { isAvailable($0) && supportsOnDevice($0) }) { return local }
        if allowsNetwork, let remote = candidates.first(where: { isAvailable($0) }) { return remote }
        throw unsupportedError(for: locale)
    }

    /// Forces on-device recognition; throws before any recognition task exists when the recognizer
    /// cannot honor it.
    static func configure(
        _ request: SFSpeechRecognitionRequest, supportsOnDevice: Bool, locale: Locale? = nil, allowsNetwork: Bool = false
    ) throws {
        if supportsOnDevice {
            request.requiresOnDeviceRecognition = true
        } else if allowsNetwork {
            request.requiresOnDeviceRecognition = false
        } else {
            throw unsupportedError(for: locale)
        }
    }
}

/// Bridges one callback-driven recognition into `async`, with cancellation that resumes the
/// continuation exactly once, cancels the underlying task, and drops any late callback.
final class CancellableRecognition: @unchecked Sendable {
    typealias Completion = @Sendable (Result<String, Error>) -> Void

    private let lock = NSLock()
    private var continuation: CheckedContinuation<String, Error>?
    private var cancelTask: (@Sendable () -> Void)?
    private var cancelled = false
    private var finished = false

    /// `start` begins recognition, reports the outcome through its completion handler, and returns
    /// a closure that cancels the underlying task. It is not called when the caller is already
    /// cancelled.
    func run(start: @escaping @Sendable (@escaping Completion) -> (@Sendable () -> Void)) async throws -> String {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<String, Error>) in
                let begin = lock.withLock { () -> Bool in
                    if cancelled { return false }
                    self.continuation = continuation
                    return true
                }
                guard begin else {
                    continuation.resume(throwing: CancellationError())
                    return
                }
                let cancel = start { [self] result in finish(result) }
                let stopNow = lock.withLock { () -> Bool in
                    if cancelled || finished { return true }
                    cancelTask = cancel
                    return false
                }
                if stopNow { cancel() }
            }
        } onCancel: {
            cancel()
        }
    }

    private func finish(_ result: Result<String, Error>) {
        let pending = lock.withLock { () -> CheckedContinuation<String, Error>? in
            guard !finished, let continuation else { return nil }
            finished = true
            self.continuation = nil
            return continuation
        }
        pending?.resume(with: result)
    }

    private func cancel() {
        let (pending, stop) = lock.withLock { () -> (CheckedContinuation<String, Error>?, (@Sendable () -> Void)?) in
            cancelled = true
            let pending = finished ? nil : continuation
            if pending != nil { finished = true }
            continuation = nil
            defer { cancelTask = nil }
            return (pending, cancelTask)
        }
        stop?()
        pending?.resume(throwing: CancellationError())
    }
}
#endif
