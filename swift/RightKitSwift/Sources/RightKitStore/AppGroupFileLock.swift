import Foundation
#if canImport(Darwin)
import Darwin
#endif

/// `UserDefaults` read-modify-write is not atomic across processes. Every App Group queue or
/// correlation mutation runs under this lock.
public enum AppGroupFileLock {
    /// Runs `body` while holding the exclusive lock and returns its result, or `nil` when the lock
    /// cannot be taken (open failed, non-contended `flock` error, or the bounded window expired).
    /// The body NEVER runs without the lock: a timeout must not become an unsynchronised
    /// read-modify-write, so callers fail closed and surface the `nil`.
    ///
    /// The wait is bounded because `flock(LOCK_EX)` blocks forever when a holder is suspended by
    /// the OS mid-section, which wedges every other process (a real 41-minute hang in practice).
    public static func withExclusiveLock<T>(
        _ configuration: AppGroupConfiguration,
        _ body: () throws -> T
    ) rethrows -> T? {
        #if canImport(Darwin)
        let primary: URL
        var fallback: URL?
        if let directory = configuration.lockDirectory {
            primary = directory.appendingPathComponent(configuration.lockName)
        } else {
            let temporary = FileManager.default.temporaryDirectory.appendingPathComponent(configuration.lockName)
            if let group = configuration.containerURL?.appendingPathComponent(configuration.lockName) {
                primary = group
                if configuration.allowTemporaryLockFallback { fallback = temporary }
            } else if configuration.allowTemporaryLockFallback {
                primary = temporary
            } else {
                // No shared container and no explicit directory: a private lock would be a lie.
                configuration.report(.lockAcquireFailed(reason: "no_shared_container", errno: ENOENT, waitedMilliseconds: 0))
                return nil
            }
        }
        var descriptor = open(primary.path, O_CREAT | O_RDWR, S_IRUSR | S_IWUSR)
        var openErrno = errno
        if descriptor < 0, let fallback {
            configuration.report(.lockOpenFallback(errno: openErrno))
            descriptor = open(fallback.path, O_CREAT | O_RDWR, S_IRUSR | S_IWUSR)
            openErrno = errno
        }
        guard descriptor >= 0 else {
            configuration.report(.lockAcquireFailed(reason: "open_failed", errno: openErrno, waitedMilliseconds: 0))
            return nil
        }
        defer { close(descriptor) }

        var acquired = false
        var lastErrno: Int32 = EWOULDBLOCK
        var waited: UInt32 = 0
        while waited < configuration.lockTimeoutMicroseconds {
            if flock(descriptor, LOCK_EX | LOCK_NB) == 0 {
                acquired = true
                break
            }
            lastErrno = errno
            guard lastErrno == EWOULDBLOCK || lastErrno == EINTR else { break }
            usleep(configuration.lockPollMicroseconds)
            waited += configuration.lockPollMicroseconds
        }
        guard acquired else {
            let milliseconds = Int(waited / 1_000)
            if lastErrno == EWOULDBLOCK {
                configuration.report(.lockTimeout(waitedMilliseconds: milliseconds))
            } else {
                configuration.report(.lockAcquireFailed(reason: "flock_failed", errno: lastErrno, waitedMilliseconds: milliseconds))
            }
            return nil
        }
        defer { flock(descriptor, LOCK_UN) }
        return try body()
        #else
        // Platforms without flock have no App Group container: fail closed.
        return nil
        #endif
    }
}
