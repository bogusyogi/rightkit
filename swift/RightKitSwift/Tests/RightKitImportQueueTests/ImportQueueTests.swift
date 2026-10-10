import Foundation
import Testing
import UniformTypeIdentifiers

@testable import RightKitImportQueue

private func config(legacyKey: String? = nil, maxFileBytes: Int = 1_000) -> ShareIntakeConfiguration {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("rk-import-\(UUID().uuidString)")
    try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    return ShareIntakeConfiguration(
        groupID: "group.test", rootOverride: root,
        defaultsOverride: UserDefaults(suiteName: "rk-import-\(UUID().uuidString)")!,
        legacyQueueKey: legacyKey, maxFileBytes: maxFileBytes
    )
}

@Test func queueSurvivesRestartRecordsFailureAndRemovesOnlyOnAcknowledge() throws {
    let cfg = config()
    let payload = PendingSharePayload(text: "hello", fileURL: nil, typeIdentifier: "public.plain-text")
    try SharedImportQueue(configuration: cfg).enqueue(payload)

    let reopened = SharedImportQueue(configuration: cfg)
    struct Boom: LocalizedError { var errorDescription: String? { "decode failed" } }
    try reopened.markFailed(payload.id, error: Boom())
    let stored = try reopened.pending()
    #expect(stored.map(\.id) == [payload.id] && stored[0].lastError == "decode failed")

    try reopened.acknowledge(payload.id)
    #expect(try reopened.pending().isEmpty)
}

@Test func legacyDefaultsQueueMigratesOnceWithStableIdsAndCorruptFileIsNotSkipped() throws {
    let cfg = config(legacyKey: "legacy.queue")
    // A pre-`id` payload: its identity must derive from content so a repeat migration is idempotent.
    let legacy = Data(#"[{"text":"old","fileURL":null,"typeIdentifier":"public.plain-text"}]"#.utf8)
    cfg.defaultsOverride?.set(legacy, forKey: "legacy.queue")
    let queue = SharedImportQueue(configuration: cfg)
    let first = try queue.pending()
    #expect(first.count == 1 && first[0].text == "old")
    #expect(cfg.defaultsOverride?.data(forKey: "legacy.queue") == nil)
    cfg.defaultsOverride?.set(legacy, forKey: "legacy.queue")
    #expect(try queue.pending().map(\.id) == first.map(\.id), "re-migrating the same payload must not duplicate it")

    let dir = cfg.rootOverride!.appendingPathComponent(cfg.queueDirectoryName)
    try Data("{not json".utf8).write(to: dir.appendingPathComponent("00-corrupt.json"))
    #expect(throws: SharedImportQueue.QueueError.unreadable) { try queue.pending() }
}

@MainActor
@Test func extractorStagesFilesWithinCapAndRefusesOversizedOnes() async throws {
    let cfg = config(maxFileBytes: 100)
    let extractor = ShareItemExtractor(configuration: cfg, accepted: .documentsAndMedia)
    func item(bytes: Int, name: String) throws -> NSExtensionItem {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(name)
        try Data(repeating: 7, count: bytes).write(to: url)
        let provider = NSItemProvider(contentsOf: url, contentType: .pdf)
        let item = NSExtensionItem()
        item.attachments = [provider]
        return item
    }
    let small = try await extractor.extractFirst(from: [item(bytes: 50, name: "small.pdf")])
    let staged = try #require(small)
    #expect(staged.typeIdentifier == UTType.pdf.identifier && staged.originalName == "small.pdf")
    let urlString = try #require(staged.fileURL)
    let stagedURL = try #require(URL(string: urlString))
    #expect(try Data(contentsOf: stagedURL).count == 50)

    #expect(await extractor.extractFirst(from: [try item(bytes: 500, name: "big.pdf")]) == nil)
    let leftovers = try FileManager.default.contentsOfDirectory(
        at: cfg.rootOverride!.appendingPathComponent(cfg.stagingDirectoryName), includingPropertiesForKeys: nil)
    #expect(leftovers.count == 1, "an oversized item must leave nothing staged")

    let text = NSItemProvider(item: "  shared words  " as NSString, typeIdentifier: UTType.plainText.identifier)
    let textItem = NSExtensionItem()
    textItem.attachments = [text]
    #expect(await extractor.extractFirst(from: [textItem])?.text == "shared words")
}

@Test func stagedWriteIsDurableAndAnyFailedStepLeavesNoPartialFile() throws {
    let cfg = config()
    let dir = cfg.containerRoot!.appendingPathComponent(cfg.queueDirectoryName)
    let good = PendingSharePayload(text: String(repeating: "x", count: 50_000), fileURL: nil, typeIdentifier: "public.plain-text")
    var queue = SharedImportQueue(configuration: cfg)
    try queue.enqueue(good)
    #expect(try queue.pending().first?.text == good.text, "staged bytes read back exactly")

    for failing in ["write", "fsync-file", "rename", "fsync-directory"] {
        let before = try FileManager.default.contentsOfDirectory(atPath: dir.path).sorted()
        queue.faultInjection = { if $0 == failing { throw CocoaError(.fileWriteUnknown) } }
        let doomed = PendingSharePayload(text: "doomed-\(failing)", fileURL: nil, typeIdentifier: "public.plain-text")
        #expect(throws: SharedImportQueue.QueueError.writeFailed) { try queue.enqueue(doomed) }
        let after = try FileManager.default.contentsOfDirectory(atPath: dir.path).sorted()
        // fsync-directory fails after the atomic rename, so only that step may leave a complete item.
        let leftovers = after.filter { !before.contains($0) }
        #expect(leftovers.allSatisfy { !$0.hasSuffix(".tmp") }, "no temp file after \(failing)")
        if failing != "fsync-directory" { #expect(after == before, "no partial file after \(failing)") }
        if failing == "fsync-directory" { try queue.acknowledge(doomed.id) }
    }
    queue.faultInjection = nil
    #expect(try queue.pending().map(\.id) == [good.id])
}

private func stagedEntries(_ cfg: ShareIntakeConfiguration) -> [String] {
    let dir = cfg.rootOverride!.appendingPathComponent(cfg.stagingDirectoryName)
    return ((try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []).sorted()
}

@Test func stagingEnforcesTheCapOnCopiedBytesWhenThePreCopySizeIsUnknown() throws {
    let cfg = config(maxFileBytes: 1_000)
    // A FIFO reports size 0 up front, so only the byte-counted copy can notice it is over the cap.
    let fifo = FileManager.default.temporaryDirectory.appendingPathComponent("rk-fifo-\(UUID().uuidString)")
    #expect(mkfifo(fifo.path, 0o600) == 0)
    defer { try? FileManager.default.removeItem(at: fifo) }
    signal(SIGPIPE, SIG_IGN)  // the copy stops reading at the cap, closing the pipe under the writer
    let writer = Thread {
        guard let handle = FileHandle(forWritingAtPath: fifo.path) else { return }
        try? handle.write(contentsOf: Data(repeating: 9, count: 10_000))
        try? handle.close()
    }
    writer.start()
    #expect(throws: CocoaError.self) {
        try ShareItemExtractor.stageExternalFile(from: fifo, fileExtension: "bin", configuration: cfg)
    }
    #expect(stagedEntries(cfg).isEmpty, "an over-cap stream must leave no staged or partial file")
}

@Test func stagingLeavesNothingBehindWhenTheCopyFailsAndStillStagesOrdinaryFiles() throws {
    let cfg = config(maxFileBytes: 1_000)
    let missing = FileManager.default.temporaryDirectory.appendingPathComponent("rk-missing-\(UUID().uuidString)")
    #expect(throws: (any Error).self) {
        try ShareItemExtractor.stageExternalFile(from: missing, fileExtension: "bin", configuration: cfg)
    }
    #expect(stagedEntries(cfg).isEmpty)

    let exact = FileManager.default.temporaryDirectory.appendingPathComponent("rk-exact-\(UUID().uuidString)")
    try Data(repeating: 1, count: 1_000).write(to: exact)
    defer { try? FileManager.default.removeItem(at: exact) }
    let staged = try ShareItemExtractor.stageExternalFile(from: exact, fileExtension: "bin", configuration: cfg)
    #expect(try Data(contentsOf: staged).count == 1_000, "a file exactly at the cap stages intact")
    #expect(stagedEntries(cfg) == [staged.lastPathComponent], "only the published file remains")
}
