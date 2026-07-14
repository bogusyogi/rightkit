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
            try trimIfNeeded()
            if FileManager.default.fileExists(atPath: fileURL.path) {
                let handle = try FileHandle(forWritingTo: fileURL)
                try handle.seekToEnd()
                try handle.write(contentsOf: line)
                try handle.close()
            } else {
                try line.write(to: fileURL, options: .atomic)
            }
            excludeFromBackup()
            return true
        } catch {
            return false
        }
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

    private func trimIfNeeded() throws {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: fileURL.path),
              let size = (attributes[.size] as? NSNumber)?.intValue,
              size > maxBytes,
              let data = try? Data(contentsOf: fileURL)
        else { return }

        let tail = data.suffix(maxBytes / 2)
        // Advance past the partial record the cut landed in; without this the survivor is a fragment.
        let boundary = tail.firstIndex(of: 0x0A).map { tail.index(after: $0) } ?? tail.endIndex
        try Data(tail[boundary...]).write(to: fileURL, options: .atomic)
    }

    private func excludeFromBackup() {
        var url = fileURL
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)
    }
}
