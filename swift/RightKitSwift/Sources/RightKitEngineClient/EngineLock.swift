import Foundation

/// A value guarded by a lock. The one place in `RightKitEngineClient` that opts out of the compiler's
/// Sendable check.
///
/// `@unchecked Sendable` is justified here and only here: `value` is private and is read and
/// written solely inside `withLock`, which holds `lock` for the whole closure, so no two threads
/// ever touch it at once; and `Value: Sendable` means whatever is copied out of the closure is
/// itself safe to share. `NSLock` is used instead of `OSAllocatedUnfairLock` or `Mutex` because
/// neither is available on every platform and deployment target this package builds for.
package final class Locked<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Value

    package init(_ value: Value) {
        self.value = value
    }

    /// Runs `body` with exclusive access. Do not block, await or re-enter the same `Locked`
    /// inside `body`; yielding to an `AsyncStream` continuation is fine (it never blocks).
    package func withLock<R>(_ body: (inout Value) throws -> R) rethrows -> R {
        lock.lock()
        defer { lock.unlock() }
        return try body(&value)
    }
}
