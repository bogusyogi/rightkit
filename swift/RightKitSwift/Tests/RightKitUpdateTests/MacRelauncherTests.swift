#if os(macOS)
import Foundation
import Testing
@testable import RightKitUpdate

// PK16-2 (Pulse updater): the detached relaunch helper, moved from Pulse's `PulseRelauncher`. The
// Internal helper tests inject `/usr/bin/touch`. The public spawn regression below uses a
// deliberately nonexistent app with the actual `/usr/bin/open`, so no application is opened.

private func scratch() -> URL {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("rk-relaunch-\(UUID().uuidString)", isDirectory: true)
    try! FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

/// The pid of a process that has already exited.
private func exitedPID() throws -> Int32 {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/true")
    try process.run()
    process.waitUntilExit()
    return process.processIdentifier
}

@discardableResult
private func runHelper(pid: Int32, limit: Int, app: URL, work: URL?, selfDirectory: URL) throws -> Int32 {
    let script = selfDirectory.appendingPathComponent("relaunch.sh")
    try MacAppRelauncher.scriptText().write(to: script, atomically: true, encoding: .utf8)
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/bin/sh")
    process.arguments = [script.path, String(pid), app.path, work?.path ?? "", selfDirectory.path, String(limit), "/usr/bin/touch"]
    try process.run()
    process.waitUntilExit()
    return process.terminationStatus
}

@Test func theHelperOpensTheAppOnceTheProcessHasQuitAndCleansUp() throws {
    let root = scratch()
    defer { try? FileManager.default.removeItem(at: root) }
    let app = root.appendingPathComponent("opened.marker")
    let work = root.appendingPathComponent("work", isDirectory: true)
    let selfDirectory = root.appendingPathComponent("self", isDirectory: true)
    try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: selfDirectory, withIntermediateDirectories: true)

    let status = try runHelper(pid: try exitedPID(), limit: 5, app: app, work: work, selfDirectory: selfDirectory)
    #expect(status == 0)
    #expect(FileManager.default.fileExists(atPath: app.path), "the installed app was opened")
    #expect(!FileManager.default.fileExists(atPath: work.path), "the staging folder was removed")
    #expect(!FileManager.default.fileExists(atPath: selfDirectory.path), "the helper removed its own script")
}

@Test func theHelperOpensNothingAndKeepsTheStagingFolderWhenTheAppNeverQuits() throws {
    let root = scratch()
    defer { try? FileManager.default.removeItem(at: root) }
    let app = root.appendingPathComponent("opened.marker")
    let work = root.appendingPathComponent("work", isDirectory: true)
    let selfDirectory = root.appendingPathComponent("self", isDirectory: true)
    try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: selfDirectory, withIntermediateDirectories: true)

    // This process is alive for the whole wait (two 0.2 s polls).
    let status = try runHelper(pid: ProcessInfo.processInfo.processIdentifier, limit: 2, app: app, work: work, selfDirectory: selfDirectory)
    #expect(status == 1)
    #expect(!FileManager.default.fileExists(atPath: app.path), "no second instance is opened over a live one")
    #expect(FileManager.default.fileExists(atPath: work.path), "the staged copy stays for a retry")
    #expect(!FileManager.default.fileExists(atPath: selfDirectory.path))
}

@Test func anEmptyWorkDirectoryRemovesNothingExceptTheHelper() throws {
    let root = scratch()
    defer { try? FileManager.default.removeItem(at: root) }
    let app = root.appendingPathComponent("opened.marker")
    let bystander = root.appendingPathComponent("bystander", isDirectory: true)
    let selfDirectory = root.appendingPathComponent("self", isDirectory: true)
    try FileManager.default.createDirectory(at: bystander, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: selfDirectory, withIntermediateDirectories: true)

    #expect(try runHelper(pid: try exitedPID(), limit: 5, app: app, work: nil, selfDirectory: selfDirectory) == 0)
    #expect(FileManager.default.fileExists(atPath: bystander.path))
    #expect(FileManager.default.fileExists(atPath: app.path))
}

@Test func theLiveAliveCheckPrecedesTheOpen() {
    let script = MacAppRelauncher.scriptText()
    let abort = script.range(of: "exit 1")
    let open = script.range(of: "\"$opener\" \"$app\"")
    #expect(abort != nil && open != nil)
    if let abort, let open { #expect(abort.lowerBound < open.lowerBound) }
}

@Test func spawnedInItsOwnSessionTheHelperReopensTheAppAndRemovesTheWorkDirectory() async throws {
    let root = scratch()
    defer { try? FileManager.default.removeItem(at: root) }
    let app = root.appendingPathComponent("opened.marker")
    let work = root.appendingPathComponent("work", isDirectory: true)
    try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)

    try MacAppRelauncher.spawn(appPath: app.path, workPath: work.path, pid: try exitedPID(), iterations: 5, opener: "/usr/bin/touch")

    let deadline = Date().addingTimeInterval(10)
    while !(FileManager.default.fileExists(atPath: app.path) && !FileManager.default.fileExists(atPath: work.path)), Date() < deadline {
        try await Task.sleep(nanoseconds: 50_000_000)
    }
    #expect(FileManager.default.fileExists(atPath: app.path))
    #expect(!FileManager.default.fileExists(atPath: work.path))
}
@Test func publicSpawnAcceptsAnExitedPIDAndRemovesOnlyItsPrivateWorkDirectory() async throws {
    let root = scratch()
    defer { try? FileManager.default.removeItem(at: root) }
    let app = root.appendingPathComponent("nonexistent.app", isDirectory: true)
    let work = root.appendingPathComponent("private-work", isDirectory: true)
    let bystander = root.appendingPathComponent("bystander", isDirectory: true)
    try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: bystander, withIntermediateDirectories: true)
    try #require(!FileManager.default.fileExists(atPath: app.path))
    // Returning normally proves public spawn succeeded. The opener is deliberately allowed to
    // fail later: that asynchronous exit has never been a synchronous spawn failure.
    try MacAppRelauncher.spawn(reopening: app, workDirectory: work, after: try exitedPID(), waitSeconds: 1)
    let deadline = Date().addingTimeInterval(10)
    while FileManager.default.fileExists(atPath: work.path), Date() < deadline {
        try await Task.sleep(nanoseconds: 50_000_000)
    }
    #expect(!FileManager.default.fileExists(atPath: work.path))
    #expect(FileManager.default.fileExists(atPath: bystander.path))
    #expect(!FileManager.default.fileExists(atPath: app.path))
}

private struct RequiredMetadataVerifier: UpdateVerifier {
    func verify(app: URL, expectedVersion: String) throws {
        // Real filesystem failure from the public verification seam, rather than a constructed
        // UpdateError: install must classify it & remove the just-copied incoming bundle.
        _ = try Data(contentsOf: app.appendingPathComponent("Contents/required-metadata"))
    }
}

@Test func publicInstallClassifiesNativeVerificationIOFailureAndPreservesInstalledCopy() async throws {
    let root = scratch()
    defer { try? FileManager.default.removeItem(at: root) }
    let apps = root.appendingPathComponent("Applications", isDirectory: true)
    let installed = apps.appendingPathComponent("Probe.app", isDirectory: true)
    let source = root.appendingPathComponent("staged/Probe.app", isDirectory: true)
    try FileManager.default.createDirectory(at: installed.appendingPathComponent("Contents"), withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: source.appendingPathComponent("Contents"), withIntermediateDirectories: true)
    let marker = installed.appendingPathComponent("Contents/installed-marker")
    try Data("original".utf8).write(to: marker)
    try Data("candidate".utf8).write(to: source.appendingPathComponent("Contents/candidate-marker"))
    let staged = StagedBundle(appURL: source, version: "2.0.0", workDirectory: root.appendingPathComponent("staged"))
    let installer = MacAppInstaller(installURL: installed, verifier: RequiredMetadataVerifier())
    await #expect(throws: UpdateError.installFailed("the new copy could not be put in place")) {
        try await installer.install(staged)
    }
    #expect(try Data(contentsOf: marker) == Data("original".utf8))
    #expect(try FileManager.default.contentsOfDirectory(atPath: apps.path) == ["Probe.app"])
    #expect(FileManager.default.fileExists(atPath: source.path))
}
#endif
