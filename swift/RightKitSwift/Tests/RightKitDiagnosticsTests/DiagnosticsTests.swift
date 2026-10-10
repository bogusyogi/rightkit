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

/// Runs `body(index)` on `count` concurrent threads and waits for all of them.
private func concurrently(_ count: Int, _ body: @escaping @Sendable (Int) -> Void) {
    let group = DispatchGroup()
    for index in 0..<count {
        group.enter()
        Thread { body(index); group.leave() }.start()
    }
    group.wait()
}

@Test func concurrentWritersOnOnePathLoseNoRecords() {
    let url = scratchURL()
    // Separate store values on one path model separate writers (extension, app, helper).
    concurrently(8) { writer in
        let store = JSONLDiagnosticStore(fileURL: url, now: { Date(timeIntervalSince1970: 0) })
        for index in 0..<40 { store.record(DiagnosticEvent(name: "w\(writer)", fields: ["i": String(index)])) }
    }
    let lines = JSONLDiagnosticStore(fileURL: url).validLines()
    #expect(lines.count == 8 * 40, "every committed record survives concurrent appends")
}

@Test func concurrentWritersWithForcedTrimsLeaveOnlyReadableCommittedLines() throws {
    let url = scratchURL()
    // 512-byte cap forces many trims while eight writers append.
    concurrently(8) { writer in
        let store = JSONLDiagnosticStore(fileURL: url, maxBytes: 512, now: { Date(timeIntervalSince1970: 0) })
        for index in 0..<60 { store.record(DiagnosticEvent(name: "w\(writer)", fields: ["i": String(index)])) }
    }
    let raw = try String(contentsOf: url, encoding: .utf8).split(separator: "\n", omittingEmptySubsequences: true)
    let store = JSONLDiagnosticStore(fileURL: url, maxBytes: 512)
    #expect(!raw.isEmpty)
    #expect(raw.count == store.validLines().count, "a trim or interleaved write left a fragment")
    let size = try #require((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)
    #expect(size.intValue <= 512 * 2)
}

@Test func aRecordAfterAnInterruptedWriterIsNotGluedToTheFragment() throws {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(fileURL: url, now: { Date(timeIntervalSince1970: 0) })
    store.record(DiagnosticEvent(name: "first"))
    let handle = try FileHandle(forWritingTo: url)
    try handle.seekToEnd()
    try handle.write(contentsOf: Data("{\"event\":\"tor".utf8))
    try handle.close()
    store.record(DiagnosticEvent(name: "second"))
    let events = store.validLines()
    #expect(events.count == 2 && events.last?.contains("\"event\":\"second\"") == true,
            "the committed record after a torn write stays recoverable")
}

// MARK: - 0.6.0: options, redaction policies, trim boundary, async append, drain

private actor BatchRecorder {
    private(set) var contents: [String] = []
    func add(_ text: String) { contents.append(text) }
}

private struct UploadFailed: Error {}

private func sealedFiles(in url: URL) -> [String] {
    let names = (try? FileManager.default.contentsOfDirectory(atPath: url.deletingLastPathComponent().path)) ?? []
    return names.filter { $0.contains(".sealed-") }
}

@Test func redactionNoneWritesFieldsVerbatim() throws {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(fileURL: url, options: StoreOptions(redaction: .none))
    #expect(store.record(DiagnosticEvent(name: "install.ok", fields: ["auth": "Bearer x"])))
    let line = try #require(store.validLines().first)
    #expect(line.contains("\"auth\":\"Bearer x\""))
}

@Test func redactionFieldsScreensOnlyTheNamedValues() throws {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(fileURL: url, options: StoreOptions(redaction: .fields(["auth"])))
    #expect(store.record(DiagnosticEvent(name: "fetch", fields: ["auth": "Bearer x", "source": "https://x/y"])))
    let line = try #require(store.validLines().first)
    #expect(line.contains("\"auth\":\"[redacted]\""))
    // JSONSerialization escapes forward slashes, so the URL is checked in its encoded form.
    #expect(line.contains("\"source\":\"https:\\/\\/x\\/y\""))
}

@Test func redactionCustomSeesKeyAndValue() throws {
    let url = scratchURL()
    let policy = RedactionPolicy.custom { key, value in key == "auth" ? "len-\(value.count)" : value }
    let store = JSONLDiagnosticStore(fileURL: url, options: StoreOptions(redaction: policy))
    #expect(store.record(DiagnosticEvent(name: "fetch", fields: ["auth": "Bearer x", "stage": "verify"])))
    let line = try #require(store.validLines().first)
    #expect(line.contains("\"auth\":\"len-8\""))
    #expect(line.contains("\"stage\":\"verify\""))
}

@Test func standardRedactionIsTheDefault() {
    #expect(StoreOptions().redaction.isStandard)
    #expect(StoreOptions().maxBytes == 256 * 1024)
}

@Test func fileProtectionOptionStillWritesRecords() {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(fileURL: url, options: StoreOptions(fileProtection: .completeUntilFirstUserAuthentication))
    #expect(store.record(DiagnosticEvent(name: "protected")))
    #expect(store.validLines().count == 1)
}

@Test func trimCutsOnRecordBoundariesAndKeepsTheNewest() throws {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(fileURL: url, options: StoreOptions(maxBytes: 512), now: { Date(timeIntervalSince1970: 0) })
    for index in 0..<120 {
        store.record(DiagnosticEvent(name: "event.\(index)", fields: ["i": String(index)]))
    }
    let raw = try String(contentsOf: url, encoding: .utf8).split(separator: "\n", omittingEmptySubsequences: true)
    // Every survivor is a whole record: starts with an object open and ends with its close.
    #expect(raw.allSatisfy { $0.hasPrefix("{\"event\"") && $0.hasSuffix("}") })
    #expect(raw.count == store.validLines().count)
    // Trimming drops the oldest records, so the newest one must survive.
    #expect(raw.last?.contains("\"event\":\"event.119\"") == true)
}

@Test func retainBytesBoundsTheTrimTarget() throws {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(
        fileURL: url,
        options: StoreOptions(maxBytes: 1_000, retainBytes: 100),
        now: { Date(timeIntervalSince1970: 0) },
    )
    for index in 0..<60 { store.record(DiagnosticEvent(name: "e", fields: ["i": String(index)])) }
    let size = try #require((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)
    #expect(size.intValue <= 1_000)
    #expect(store.validLines().count >= 1)
}

@Test func appendAsyncWritesTheSameRecordAsRecord() async throws {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(fileURL: url, now: { Date(timeIntervalSince1970: 0) })
    #expect(await store.append(DiagnosticEvent(name: "async.ok", fields: ["stage": "verify"])))
    let line = try #require(store.validLines().first)
    #expect(line.contains("\"event\":\"async.ok\""))
}

@Test func drainUploadsTheSealedBatchAndDeletesIt() async throws {
    let url = scratchURL()
    let recorder = BatchRecorder()
    let store = JSONLDiagnosticStore(
        fileURL: url,
        options: StoreOptions(uploader: { batch in
            await recorder.add(try String(contentsOf: batch, encoding: .utf8))
        }),
        now: { Date(timeIntervalSince1970: 0) },
    )
    store.record(DiagnosticEvent(name: "first"))
    store.record(DiagnosticEvent(name: "second"))

    #expect(try await store.drain() == 1)
    let uploaded = await recorder.contents
    #expect(uploaded.count == 1)
    #expect(uploaded.first?.components(separatedBy: "\n").filter { !$0.isEmpty }.count == 2)
    #expect(store.validLines().isEmpty, "sealed records leave the active log")
    #expect(sealedFiles(in: url).isEmpty, "a successfully uploaded batch is deleted")

    // Records written after a drain start a fresh active log.
    store.record(DiagnosticEvent(name: "third"))
    #expect(store.validLines().count == 1)
}

@Test func aFailedUploadKeepsTheBatchForTheNextDrain() async throws {
    let url = scratchURL()
    let recorder = BatchRecorder()
    let failing = JSONLDiagnosticStore(
        fileURL: url,
        options: StoreOptions(uploader: { _ in throw UploadFailed() }),
        now: { Date(timeIntervalSince1970: 0) },
    )
    failing.record(DiagnosticEvent(name: "keep.me"))

    await #expect(throws: UploadFailed.self) { _ = try await failing.drain() }
    #expect(sealedFiles(in: url).count == 1, "the batch survives a failed upload")

    let working = JSONLDiagnosticStore(
        fileURL: url,
        options: StoreOptions(uploader: { batch in await recorder.add(try String(contentsOf: batch, encoding: .utf8)) }),
    )
    #expect(try await working.drain() == 1)
    let uploaded = await recorder.contents
    #expect(uploaded.first?.contains("keep.me") == true, "at-least-once: the retained record is uploaded")
    #expect(sealedFiles(in: url).isEmpty)
}

@Test func drainWithoutAnUploaderChangesNothing() async throws {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(fileURL: url)
    store.record(DiagnosticEvent(name: "stays"))
    await #expect(throws: DiagnosticStoreError.uploaderNotConfigured) { _ = try await store.drain() }
    #expect(store.validLines().count == 1)
}

@Test func aSecondDrainWhileTheLockIsHeldFailsInsteadOfDoubleUploading() async throws {
    let url = scratchURL()
    let store = JSONLDiagnosticStore(fileURL: url, options: StoreOptions(uploader: { _ in }))
    store.record(DiagnosticEvent(name: "held"))
    try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)

    // Stand in for another process that is mid-drain.
    let fd = open(url.resolvingSymlinksInPath().path + ".drain.lock", O_RDWR | O_CREAT, 0o600)
    #expect(flock(fd, LOCK_EX | LOCK_NB) == 0)
    defer { flock(fd, LOCK_UN); close(fd) }

    await #expect(throws: DiagnosticStoreError.drainInProgress) { _ = try await store.drain() }
    #expect(store.validLines().count == 1, "the active log is untouched while another drain runs")
}

private extension RedactionPolicy {
    var isStandard: Bool {
        if case .standard = self { return true }
        return false
    }
}
