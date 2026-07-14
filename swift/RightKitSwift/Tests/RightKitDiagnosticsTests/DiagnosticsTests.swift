import Foundation
import Testing

@testable import RightKitDiagnostics

private func scratchURL() -> URL {
    FileManager.default.temporaryDirectory
        .appendingPathComponent("rightkit-diag-\(UUID().uuidString)")
        .appendingPathComponent("log.jsonl")
}

@Test func redactsCredentialsUrlsAndCapturePaths() {
    let redactor = PrivacyRedactor()

    // The §3 gate: secrets, auth headers, URLs, and file/audio paths must never survive into a log.
    for leak in [
        "Bearer abc123",
        "Authorization: Basic zzz",
        "my-secret-value",
        "token=abc",
        "https://api.rightapps.dev/v1/licenses",
        "http://127.0.0.1:5057",
        "file:///Users/adrian/Library/x.wav",
        "/tmp/capture.caf",
        "/Users/adrian/recording.wav",
    ] {
        #expect(redactor.redact(leak) == PrivacyRedactor.redacted, "leaked: \(leak)")
    }
}

@Test func keepsBenignIdentifiersButBoundsThem() {
    let redactor = PrivacyRedactor()

    #expect(redactor.redact("wake_word_detected") == "wake_word_detected")
    #expect(redactor.redact("model.install:ok") == "model.install:ok")
    // Anything outside the allowlist becomes `-`, so an event name cannot smuggle structure.
    #expect(redactor.redact("event\u{1B}[31m;drop") == "event--31m-drop")
    #expect(redactor.redact("") == PrivacyRedactor.unknown)
    #expect(redactor.redact(String(repeating: "a", count: 200)).count == PrivacyRedactor.maxLength)
}

@Test func recordsSortedStableLines() throws {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(fileURL: url, now: { Date(timeIntervalSince1970: 0) })

    #expect(store.record(DiagnosticEvent(name: "install.ok", fields: ["stage": "verify", "auth": "Bearer x"])))

    let lines = store.validLines()
    #expect(lines.count == 1)
    let line = try #require(lines.first)
    // sortedKeys makes the line diffable, and the credential never reaches the file.
    #expect(line.hasPrefix("{\"auth\":\"[redacted]\""))
    #expect(!line.contains("Bearer"))
    #expect(line.contains("\"event\":\"install.ok\""))
}

@Test func trimmingBoundsTheFileWithoutCorruptingIt() throws {
    let url = scratchURL()
    // Small cap so a handful of records trip the trim.
    let store = JSONLDiagnosticStore(fileURL: url, maxBytes: 512, now: { Date(timeIntervalSince1970: 0) })

    for index in 0..<80 {
        store.record(DiagnosticEvent(name: "event.\(index)", fields: ["i": String(index)]))
    }

    let size = try #require((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)
    #expect(size.intValue <= 512 * 2)

    // Every surviving line must still parse. HeardRight's suffix(maxBytes/2) cut mid-record, so the
    // first survivor was a JSON fragment — the log stayed small by becoming unreadable.
    let raw = try String(contentsOf: url, encoding: .utf8)
        .split(separator: "\n", omittingEmptySubsequences: true)
    #expect(raw.count == store.validLines().count, "a trimmed line failed to parse")
    #expect(!raw.isEmpty)
}

@Test func aTornWriteCostsTheLastRecordNotTheLog() throws {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(fileURL: url, now: { Date(timeIntervalSince1970: 0) })
    store.record(DiagnosticEvent(name: "first"))
    store.record(DiagnosticEvent(name: "second"))

    // Simulate a crash mid-append: a partial record with no newline.
    let handle = try FileHandle(forWritingTo: url)
    try handle.seekToEnd()
    try handle.write(contentsOf: Data("{\"event\":\"tor".utf8))
    try handle.close()

    let lines = store.validLines()
    #expect(lines.count == 2)
    #expect(lines.allSatisfy { $0.contains("\"event\"") })
}
