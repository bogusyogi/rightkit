import Foundation

/// One diagnostics record. Values are redacted on the way in, not on the way out — a log that has to
/// be sanitised when read is a log that has already leaked to everything that copied the file.
public struct DiagnosticEvent: Sendable {
    public let name: String
    public let fields: [String: String]

    public init(name: String, fields: [String: String] = [:]) {
        self.name = name
        self.fields = fields
    }
}

/// Bounded JSONL diagnostics log with an injected storage root and clock, so tests drive time and
/// the filesystem instead of the app group and the wall clock.
///
/// Extracted from HeardRight's `DogfoodTraceStore`, with one deliberate correction: trimming now cuts
/// on a line boundary. The original kept `data.suffix(maxBytes / 2)`, which slices mid-record and
/// leaves a truncated JSON fragment as the first line — bounding the file by corrupting it.
public struct JSONLDiagnosticStore: Sendable {
    public let fileURL: URL
    private let maxBytes: Int
    private let now: @Sendable () -> Date
    private let redactor = PrivacyRedactor()

    public init(
        fileURL: URL,
        maxBytes: Int = 256 * 1024,
        now: @escaping @Sendable () -> Date = { Date() },
    ) {
        self.fileURL = fileURL
        self.maxBytes = maxBytes
        self.now = now
    }

    @discardableResult
    public func record(_ event: DiagnosticEvent) -> Bool {
        var payload: [String: String] = [:]
        for (key, value) in event.fields {
            payload[redactor.redact(key)] = redactor.redact(value)
        }
        payload["event"] = redactor.redact(event.name)
        payload["timestamp"] = ISO8601DateFormatter().string(from: now())

        // sortedKeys: a diagnostics line has to be diffable and stable across runs.
        guard let data = try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys]) else {
            return false
        }
        var line = data
        line.append(0x0A)

        do {
            try FileManager.default.createDirectory(
                at: fileURL.deletingLastPathComponent(),
                withIntermediateDirectories: true,
            )
            // One transaction per canonical path: trim and append happen under the same exclusive
            // lock, so concurrent writers (threads, or processes sharing an app-group log) neither
            // lose each other's records nor trim away a record another writer is appending.
            let target = fileURL.resolvingSymlinksInPath()
            guard try Self.withExclusiveLock(for: target, { try commit(line, to: target) }) != nil else {
                return false
            }
            excludeFromBackup()
            return true
        } catch {
            return false
        }
    }

    private func commit(_ line: Data, to target: URL) throws {
        try trimIfNeeded(target)
        let fd = open(target.path, O_WRONLY | O_APPEND | O_CREAT, 0o600)
        guard fd >= 0 else { throw CocoaError(.fileWriteUnknown) }
        defer { close(fd) }
        var record = Data()
        // A crash mid-append leaves a trailing fragment with no newline; start a fresh line so the
        // new record is never glued onto it and lost with it.
        if Self.lastByte(ofFileAt: target.path) .map({ $0 != 0x0A }) ?? false { record.append(0x0A) }
        record.append(line)
        // O_APPEND makes the single write land at end-of-file; a short write is retried to the end.
        try record.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let n = write(fd, raw.baseAddress! + offset, raw.count - offset)
                if n < 0 && errno == EINTR { continue }
                guard n > 0 else { throw CocoaError(.fileWriteUnknown) }
                offset += n
            }
        }
    }

    private static func lastByte(ofFileAt path: String) -> UInt8? {
        let fd = open(path, O_RDONLY)
        guard fd >= 0 else { return nil }
        defer { close(fd) }
        var byte: UInt8 = 0
        let size = lseek(fd, 0, SEEK_END)
        guard size > 0, pread(fd, &byte, 1, size - 1) == 1 else { return nil }
        return byte
    }

    /// `flock` on a sibling lock file. flock locks belong to the open file description, so this
    /// excludes other threads of this process as well as other processes. The wait is bounded and
    /// fails closed: when the lock cannot be taken the record is dropped, never written unlocked.
    private static func withExclusiveLock<T>(for target: URL, _ body: () throws -> T) throws -> T? {
        let fd = open(target.path + ".lock", O_RDWR | O_CREAT, 0o600)
        guard fd >= 0 else { return nil }
        defer { close(fd) }
        let deadline = Date().addingTimeInterval(lockTimeout)
        while flock(fd, LOCK_EX | LOCK_NB) != 0 {
            guard errno == EWOULDBLOCK || errno == EINTR, Date() < deadline else { return nil }
            usleep(1_000)
        }
        defer { flock(fd, LOCK_UN) }
        return try body()
    }

    private static let lockTimeout: TimeInterval = 5

    /// Every line the file currently holds, dropping any partial trailing write. A crash mid-append
    /// must cost the last record, never the log.
    public func validLines() -> [String] {
        guard let text = try? String(contentsOf: fileURL, encoding: .utf8) else { return [] }
        return text
            .split(separator: "\n", omittingEmptySubsequences: true)
            .map(String.init)
            .filter { line in
                guard let data = line.data(using: .utf8) else { return false }
                return (try? JSONSerialization.jsonObject(with: data)) != nil
            }
    }

    private func trimIfNeeded(_ target: URL) throws {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: target.path),
              let size = (attributes[.size] as? NSNumber)?.intValue,
              size > maxBytes,
              let data = try? Data(contentsOf: target)
        else { return }

        let tail = data.suffix(maxBytes / 2)
        // Advance past the partial record the cut landed in; without this the survivor is a fragment.
        let boundary = tail.firstIndex(of: 0x0A).map { tail.index(after: $0) } ?? tail.endIndex
        try Data(tail[boundary...]).write(to: target, options: .atomic)
    }

    private func excludeFromBackup() {
        var url = fileURL
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)
    }
}
