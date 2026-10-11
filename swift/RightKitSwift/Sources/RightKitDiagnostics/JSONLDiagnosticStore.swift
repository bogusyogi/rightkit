import Foundation

/// One diagnostics record. Values are redacted on the way in, not on the way out — a log that has to
/// be sanitised when read is a log that has already leaked to everything that copied the file.
public struct DiagnosticEvent: Sendable {
    public let name: String
    public let fields: [String: String]
    /// When the event happened, if the caller knows. Nil (the default) means the store stamps the
    /// line with its clock when the line is written, as it always has. A supplied value is written
    /// as given, so an event recorded from a background job still carries the time it occurred.
    public let timestamp: Date?

    public init(name: String, fields: [String: String] = [:], timestamp: Date? = nil) {
        self.name = name
        self.fields = fields
        self.timestamp = timestamp
    }
}

/// How field keys and values are transformed before they reach the file.
public enum RedactionPolicy: Sendable {
    /// Every key and value passes through `PrivacyRedactor`, and so does the event name. Default.
    case standard
    /// Only the values of the named fields pass through `PrivacyRedactor`. Other values, all keys, and
    /// the event name are written as given (the event name is still screened).
    case fields([String])
    /// The closure receives `(key, value)` and returns the value to write. Keys and the event name
    /// still pass through `PrivacyRedactor`. The closure must not return anything it would not log.
    case custom(@Sendable (_ key: String, _ value: String) -> String)
    /// Nothing is transformed. Only for callers that have already sanitised every field.
    case none
}

/// Configuration for `JSONLDiagnosticStore`. The defaults match the pre-0.6.0 store: 256 KB cap, standard
/// redaction, no file protection class change, no uploader.
public struct StoreOptions: Sendable {
    /// Data-protection class applied to the log and its lock file after every write. Nil leaves the
    /// platform default. On iOS, `.complete` makes writes fail while the device is locked; a record
    /// dropped that way is reported as `false` from `record`.
    public var fileProtection: FileProtectionType?
    public var redaction: RedactionPolicy
    /// The log is trimmed once it grows past this many bytes.
    public var maxBytes: Int
    /// Bytes kept when a trim runs, rounded down to whole records. Nil keeps half of `maxBytes`.
    public var retainBytes: Int?
    /// Receives each sealed batch on `drain()`. The URL is valid only until the call returns; the
    /// uploader must finish reading it before returning, and the store deletes it on success.
    public var uploader: (@Sendable (URL) async throws -> Void)?

    public init(
        fileProtection: FileProtectionType? = nil,
        redaction: RedactionPolicy = .standard,
        maxBytes: Int = 256 * 1024,
        retainBytes: Int? = nil,
        uploader: (@Sendable (URL) async throws -> Void)? = nil,
    ) {
        self.fileProtection = fileProtection
        self.redaction = redaction
        self.maxBytes = max(1, maxBytes)
        // A retain target at or above the cap would trim almost nothing and rewrite the file on every record.
        self.retainBytes = retainBytes.map { min(max(0, $0), self.maxBytes) }
        self.uploader = uploader
    }
}

public enum DiagnosticStoreError: Error, Equatable, Sendable {
    /// `drain()` was called without `StoreOptions.uploader`. Nothing was sealed or moved.
    case uploaderNotConfigured
    /// Another drain holds the drain lock, possibly in another process. Its batches will be uploaded by it.
    case drainInProgress
    /// The record lock could not be taken, so the active log was not sealed.
    case lockUnavailable
}

/// Bounded JSONL diagnostics log with an injected storage root and clock, so tests drive time and
/// the filesystem instead of the app group and the wall clock.
///
/// Extracted from HeardRight's `DogfoodTraceStore`, with two deliberate corrections. Trimming cuts on a
/// line boundary: the original kept `data.suffix(maxBytes / 2)`, which slices mid-record and leaves a
/// truncated JSON fragment as the first line, bounding the file by corrupting it. Trimming and appending
/// also share one exclusive lock, so concurrent writers do not lose records.
public struct JSONLDiagnosticStore: Sendable {
    public let fileURL: URL
    public let options: StoreOptions
    private let now: @Sendable () -> Date
    private let redactor = PrivacyRedactor()

    public init(
        fileURL: URL,
        options: StoreOptions = StoreOptions(),
        now: @escaping @Sendable () -> Date = { Date() },
    ) {
        self.fileURL = fileURL
        self.options = options
        self.now = now
    }

    /// Source-compatible with the pre-0.6.0 initializer. `maxBytes` is required here so that
    /// `init(fileURL:)` resolves to the options initializer without ambiguity.
    public init(
        fileURL: URL,
        maxBytes: Int,
        now: @escaping @Sendable () -> Date = { Date() },
    ) {
        self.init(fileURL: fileURL, options: StoreOptions(maxBytes: maxBytes), now: now)
    }

    // MARK: - Recording

    @discardableResult
    public func record(_ event: DiagnosticEvent) -> Bool {
        guard let line = encode(event) else { return false }
        do {
            try FileManager.default.createDirectory(
                at: fileURL.deletingLastPathComponent(),
                withIntermediateDirectories: true,
            )
            // One transaction per canonical path: trim and append happen under the same exclusive
            // lock, so concurrent writers (threads, or processes sharing an app-group log) neither
            // lose each other's records nor trim away a record another writer is appending.
            let target = fileURL.resolvingSymlinksInPath()
            guard try withExclusiveLock(for: target, { try commit(line, to: target) }) != nil else {
                return false
            }
            excludeFromBackup()
            return true
        } catch {
            return false
        }
    }

    /// Async form of `record`. The blocking lock wait runs off the caller's executor.
    ///
    /// ## Ordering
    ///
    /// Every `append` and ``enqueue(_:)`` on the same file in this process goes through one serial
    /// FIFO queue (keyed by the file's resolved path), so lines are written in the order the calls
    /// reached the queue:
    /// - **`enqueue` calls from one caller (thread or task) are written in exactly the order made.**
    /// - `append` calls where each is awaited before the next begins are written in that order.
    /// - `append` calls started without awaiting each other (separate `Task`s) reach the queue in
    ///   whatever order Swift concurrency starts them; for those, supply ``DiagnosticEvent/timestamp``
    ///   and order by it, or use `enqueue`.
    /// - Across processes the file lock serialises each write; relative order is arrival at the lock.
    ///
    /// A line's `timestamp` is `event.timestamp` when given; otherwise the store's clock at the moment
    /// the line is written (so two un-timestamped events logged in the same millisecond may differ
    /// in file order from call order only in the un-awaited case above).
    public func append(_ event: DiagnosticEvent) async -> Bool {
        let store = self
        return await withCheckedContinuation { continuation in
            Self.queue(for: fileURL).async {
                continuation.resume(returning: store.record(event))
            }
        }
    }

    /// `append` with a caller-supplied timestamp for this event.
    public func append(_ event: DiagnosticEvent, at timestamp: Date) async -> Bool {
        await append(DiagnosticEvent(name: event.name, fields: event.fields, timestamp: timestamp))
    }

    /// Fire-and-forget, order-preserving form for synchronous callers: returns at once; the line is
    /// written later on the file's serial queue, strictly after every earlier `enqueue` or `append`
    /// that reached it. The event is stamped with ``DiagnosticEvent/timestamp`` or, if nil, **the time
    /// of this call**, so the written time is the time it happened, not the time the queue got to it.
    /// A failed write is dropped silently; use ``flush()`` to wait for the queue and `append` when the
    /// result matters.
    public func enqueue(_ event: DiagnosticEvent) {
        let stamped = DiagnosticEvent(name: event.name, fields: event.fields, timestamp: event.timestamp ?? now())
        let store = self
        Self.queue(for: fileURL).async { _ = store.record(stamped) }
    }

    /// Returns once every `enqueue` and `append` made before this call has been attempted.
    public func flush() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            Self.queue(for: fileURL).async { continuation.resume() }
        }
    }

    private static let queues = QueueRegistry()

    private static func queue(for fileURL: URL) -> DispatchQueue {
        queues.queue(forPath: fileURL.resolvingSymlinksInPath().path)
    }

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

    // MARK: - Draining

    /// Seals the active log, uploads every sealed batch oldest first, and deletes each one after its
    /// upload succeeds. Records written during the upload go to a fresh active log.
    ///
    /// Delivery is at-least-once: if the uploader throws, the failed batch and any later ones stay on
    /// disk and the next `drain()` uploads them again. Returns the number of batches uploaded.
    public func drain() async throws -> Int {
        guard let uploader = options.uploader else { throw DiagnosticStoreError.uploaderNotConfigured }
        let target = fileURL.resolvingSymlinksInPath()
        let fd = open(target.path + ".drain.lock", O_RDWR | O_CREAT, 0o600)
        guard fd >= 0 else { throw CocoaError(.fileWriteUnknown) }
        defer {
            flock(fd, LOCK_UN)
            close(fd)
        }
        guard flock(fd, LOCK_EX | LOCK_NB) == 0 else { throw DiagnosticStoreError.drainInProgress }

        try sealActiveLog(target)

        var uploaded = 0
        for batch in try sealedBatches(for: target) {
            try await uploader(batch)
            try FileManager.default.removeItem(at: batch)
            uploaded += 1
        }
        return uploaded
    }

    // MARK: - Encoding

    private func encode(_ event: DiagnosticEvent) -> Data? {
        var payload: [String: String] = [:]
        for (key, value) in event.fields {
            let (outKey, outValue) = transform(key: key, value: value)
            payload[outKey] = outValue
        }
        payload["event"] = options.redaction.keepsEventName ? event.name : redactor.redact(event.name)
        payload["timestamp"] = ISO8601DateFormatter().string(from: event.timestamp ?? now())

        // sortedKeys: a diagnostics line has to be diffable and stable across runs.
        guard let data = try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys]) else {
            return nil
        }
        var line = data
        line.append(0x0A)
        return line
    }

    private func transform(key: String, value: String) -> (String, String) {
        switch options.redaction {
        case .standard:
            return (redactor.redact(key), redactor.redact(value))
        case .fields(let names):
            return (key, names.contains(key) ? redactor.redact(value) : value)
        case .custom(let transform):
            return (redactor.redact(key), transform(key, value))
        case .none:
            return (key, value)
        }
    }

    // MARK: - Writing

    private func commit(_ line: Data, to target: URL) throws {
        try trimIfNeeded(target)
        let fd = open(target.path, O_WRONLY | O_APPEND | O_CREAT, 0o600)
        guard fd >= 0 else { throw CocoaError(.fileWriteUnknown) }
        defer { close(fd) }
        var record = Data()
        // A crash mid-append leaves a trailing fragment with no newline; start a fresh line so the
        // new record is never glued onto it and lost with it.
        if Self.lastByte(ofFileAt: target.path).map({ $0 != 0x0A }) ?? false { record.append(0x0A) }
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
        applyFileProtection(to: target.path)
    }

    /// Keeps the newest whole records, at most `retainBytes`. The cut never lands mid-record: if it
    /// falls inside one, that record is dropped, so every surviving line parses.
    private func trimIfNeeded(_ target: URL) throws {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: target.path),
              let size = (attributes[.size] as? NSNumber)?.intValue,
              size > options.maxBytes,
              let data = try? Data(contentsOf: target)
        else { return }

        let retain = min(max(0, options.retainBytes ?? options.maxBytes / 2), data.count)
        let cut = data.index(data.endIndex, offsetBy: -retain)
        var boundary = cut
        if cut > data.startIndex, data[data.index(before: cut)] != 0x0A {
            boundary = data[cut...].firstIndex(of: 0x0A).map { data.index(after: $0) } ?? data.endIndex
        }
        try Data(data[boundary...]).write(to: target, options: .atomic)
        applyFileProtection(to: target.path)
    }

    private func applyFileProtection(to path: String) {
        guard let protection = options.fileProtection else { return }
        try? FileManager.default.setAttributes([.protectionKey: protection], ofItemAtPath: path)
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
    private func withExclusiveLock<T>(for target: URL, _ body: () throws -> T) throws -> T? {
        let fd = open(target.path + ".lock", O_RDWR | O_CREAT, 0o600)
        guard fd >= 0 else { return nil }
        defer { close(fd) }
        applyFileProtection(to: target.path + ".lock")
        let deadline = Date().addingTimeInterval(Self.lockTimeout)
        while flock(fd, LOCK_EX | LOCK_NB) != 0 {
            guard errno == EWOULDBLOCK || errno == EINTR, Date() < deadline else { return nil }
            usleep(1_000)
        }
        defer { flock(fd, LOCK_UN) }
        return try body()
    }

    private static let lockTimeout: TimeInterval = 5

    // MARK: - Sealing

    /// Renames the active log to a sealed batch under the record lock, so no append straddles the seal.
    /// The zero-padded timestamp in the name keeps batches in chronological order when sorted.
    private func sealActiveLog(_ target: URL) throws {
        let stamp = Int64((now().timeIntervalSince1970 * 1_000).rounded())
        let digits = String(stamp)
        let padded = String(repeating: "0", count: max(0, 16 - digits.count)) + digits
        let sealed = target.deletingLastPathComponent().appendingPathComponent(
            "\(target.lastPathComponent).sealed-\(padded)-\(UUID().uuidString).jsonl",
        )
        guard try withExclusiveLock(for: target, {
            let size = (try? FileManager.default.attributesOfItem(atPath: target.path))?[.size] as? NSNumber
            guard let size, size.intValue > 0 else { return }
            try FileManager.default.moveItem(at: target, to: sealed)
        }) != nil else {
            throw DiagnosticStoreError.lockUnavailable
        }
    }

    private func sealedBatches(for target: URL) throws -> [URL] {
        let prefix = target.lastPathComponent + ".sealed-"
        return try FileManager.default.contentsOfDirectory(
            at: target.deletingLastPathComponent(),
            includingPropertiesForKeys: nil,
        )
        .filter { $0.lastPathComponent.hasPrefix(prefix) && $0.pathExtension == "jsonl" }
        .sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    private func excludeFromBackup() {
        var url = fileURL
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)
    }
}

private extension RedactionPolicy {
    var keepsEventName: Bool {
        if case .none = self { return true }
        return false
    }
}

/// One serial utility queue per canonical log path, shared by every store value for that path.
private final class QueueRegistry: @unchecked Sendable {
    private let lock = NSLock()
    private var queues: [String: DispatchQueue] = [:]

    func queue(forPath path: String) -> DispatchQueue {
        lock.lock(); defer { lock.unlock() }
        if let existing = queues[path] { return existing }
        let created = DispatchQueue(label: "org.rightkit.diagnostics.\(path)", qos: .utility)
        queues[path] = created
        return created
    }
}
