#if os(macOS)
import Foundation
import Testing
@testable import RightKitUpdate

private struct AcceptAll: UpdateVerifier { func verify(app: URL, expectedVersion: String) throws {} }
private struct RejectAll: UpdateVerifier {
    func verify(app: URL, expectedVersion: String) throws { throw UpdateError.verificationFailed("rejected") }
}

private func sandbox() -> URL {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("rk-mac-\(UUID().uuidString)", isDirectory: true)
    try! FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

@discardableResult
private func makeApp(at url: URL, bundleID: String = "dev.example.app", version: String, marker: String? = nil) throws -> URL {
    let contents = url.appendingPathComponent("Contents", isDirectory: true)
    try FileManager.default.createDirectory(at: contents, withIntermediateDirectories: true)
    let plist: [String: Any] = ["CFBundleIdentifier": bundleID, "CFBundleShortVersionString": version, "CFBundleName": "App"]
    try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0).write(to: contents.appendingPathComponent("Info.plist"))
    try Data((marker ?? version).utf8).write(to: contents.appendingPathComponent("marker.txt"))
    return url
}

private func zip(_ app: URL, to archive: URL) throws {
    try SystemToolRunner().run("/usr/bin/ditto", ["-c", "-k", "--keepParent", app.path, archive.path])
}

private func marker(_ app: URL) throws -> String {
    String(decoding: try Data(contentsOf: app.appendingPathComponent("Contents/marker.txt")), as: UTF8.self)
}

private let newRelease = UpdateRelease(version: "2.0.0")

// MARK: Verifier

@Test func requirementPinsDeveloperIDBundleAndTeam() throws {
    let verifier = try DeveloperIDVerifier(bundleIdentifier: "dev.orthic.pulse", teamIdentifier: "6KLGD3LLKF")
    #expect(verifier.requirementText.contains(#"identifier "dev.orthic.pulse""#))
    #expect(verifier.requirementText.contains(#"certificate leaf[subject.OU] = "6KLGD3LLKF""#))
    #expect(verifier.requirementText.contains("1.2.840.113635.100.6.2.6"))
    #expect(verifier.requirementText.contains("anchor apple generic"))
}

@Test func verifierRefusesMalformedIdentifiersSoNothingReachesTheRequirementLanguage() {
    #expect(throws: UpdateError.self) { try DeveloperIDVerifier(bundleIdentifier: #"x" or true"#, teamIdentifier: "6KLGD3LLKF") }
    #expect(throws: UpdateError.self) { try DeveloperIDVerifier(bundleIdentifier: "", teamIdentifier: "6KLGD3LLKF") }
    #expect(throws: UpdateError.self) { try DeveloperIDVerifier(bundleIdentifier: "dev.example.app", teamIdentifier: "short") }
    #expect(throws: UpdateError.self) { try DeveloperIDVerifier(bundleIdentifier: "dev.example.app", teamIdentifier: "6klgd3llkf") }
}

@Test func verifierFailsClosedOnIdentityVersionAndUnsignedBundles() throws {
    let dir = sandbox()
    defer { try? FileManager.default.removeItem(at: dir) }
    let verifier = try DeveloperIDVerifier(bundleIdentifier: "dev.example.app", teamIdentifier: "6KLGD3LLKF")
    let app = try makeApp(at: dir.appendingPathComponent("App.app"), version: "2.0.0")
    // Right identity and version, but no Developer ID signature: refused.
    #expect(throws: UpdateError.self) { try verifier.verify(app: app, expectedVersion: "2.0.0") }
    // Version the bundle does not carry.
    #expect(throws: UpdateError.self) { try verifier.verify(app: app, expectedVersion: "2.0.1") }
    let other = try makeApp(at: dir.appendingPathComponent("Other.app"), bundleID: "dev.example.other", version: "2.0.0")
    #expect(throws: UpdateError.verificationFailed("bundle identifier differs")) { try verifier.verify(app: other, expectedVersion: "2.0.0") }
    #expect(throws: UpdateError.self) { try verifier.verify(app: dir.appendingPathComponent("Missing.app"), expectedVersion: "2.0.0") }
}

// MARK: Stage

@Test func stageExtractsAZipAndVerifiesTheCopy() async throws {
    let dir = sandbox()
    defer { try? FileManager.default.removeItem(at: dir) }
    let source = try makeApp(at: dir.appendingPathComponent("src/App.app"), version: "2.0.0")
    let archive = dir.appendingPathComponent("App.zip")
    try zip(source, to: archive)
    let install = dir.appendingPathComponent("Applications/App.app")
    let work = dir.appendingPathComponent("work")
    try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)

    let installer = MacAppInstaller(installURL: install, verifier: AcceptAll())
    let staged = try await installer.stage(artifact: archive, release: newRelease, currentVersion: "1.0.0", workDirectory: work)
    #expect(try marker(staged.appURL) == "2.0.0")

    let rejecting = MacAppInstaller(installURL: install, verifier: RejectAll())
    let work2 = dir.appendingPathComponent("work2")
    try FileManager.default.createDirectory(at: work2, withIntermediateDirectories: true)
    await #expect(throws: UpdateError.verificationFailed("rejected")) {
        _ = try await rejecting.stage(artifact: archive, release: newRelease, currentVersion: "1.0.0", workDirectory: work2)
    }
}

@Test func stageRefusesAnOlderOrEqualVersionAndUnknownArtifacts() async throws {
    let dir = sandbox()
    defer { try? FileManager.default.removeItem(at: dir) }
    let installer = MacAppInstaller(installURL: dir.appendingPathComponent("App.app"), verifier: AcceptAll())
    await #expect(throws: UpdateError.notNewer) {
        _ = try await installer.stage(artifact: dir.appendingPathComponent("a.zip"), release: newRelease, currentVersion: "2.0.0", workDirectory: dir)
    }
    await #expect(throws: UpdateError.self) {
        _ = try await installer.stage(artifact: dir.appendingPathComponent("a.exe"), release: newRelease, currentVersion: "1.0.0", workDirectory: dir)
    }
}

// MARK: Install

@Test func installReplacesInPlaceKeepsNoPreviousAndDeletesAStaleOne() async throws {
    let dir = sandbox()
    defer { try? FileManager.default.removeItem(at: dir) }
    let apps = dir.appendingPathComponent("Applications", isDirectory: true)
    let install = try makeApp(at: apps.appendingPathComponent("App.app"), version: "1.0.0")
    let stale = try makeApp(at: apps.appendingPathComponent("App.app.previous"), version: "0.9.0")
    let staged = StagedBundle(
        appURL: try makeApp(at: dir.appendingPathComponent("work/staged/App.app"), version: "2.0.0"), version: "2.0.0",
        workDirectory: dir.appendingPathComponent("work"),
    )
    try await MacAppInstaller(installURL: install, verifier: AcceptAll()).install(staged)

    #expect(try marker(install) == "2.0.0")
    #expect(!FileManager.default.fileExists(atPath: stale.path))
    let leftovers = try FileManager.default.contentsOfDirectory(atPath: apps.path)
    #expect(leftovers == ["App.app"])
}

@Test func installInstallsFreshWhenNothingIsThere() async throws {
    let dir = sandbox()
    defer { try? FileManager.default.removeItem(at: dir) }
    let apps = dir.appendingPathComponent("Applications", isDirectory: true)
    try FileManager.default.createDirectory(at: apps, withIntermediateDirectories: true)
    let staged = StagedBundle(appURL: try makeApp(at: dir.appendingPathComponent("s/App.app"), version: "2.0.0"), version: "2.0.0", workDirectory: dir)
    let install = apps.appendingPathComponent("App.app")
    try await MacAppInstaller(installURL: install, verifier: AcceptAll()).install(staged)
    #expect(try marker(install) == "2.0.0")
}

@Test func aRejectedCopyLeavesTheInstalledAppUntouched() async throws {
    let dir = sandbox()
    defer { try? FileManager.default.removeItem(at: dir) }
    let apps = dir.appendingPathComponent("Applications", isDirectory: true)
    let install = try makeApp(at: apps.appendingPathComponent("App.app"), version: "1.0.0")
    let staged = StagedBundle(appURL: try makeApp(at: dir.appendingPathComponent("s/App.app"), version: "2.0.0"), version: "2.0.0", workDirectory: dir)
    await #expect(throws: UpdateError.verificationFailed("rejected")) {
        try await MacAppInstaller(installURL: install, verifier: RejectAll()).install(staged)
    }
    #expect(try marker(install) == "1.0.0")
    #expect(try FileManager.default.contentsOfDirectory(atPath: apps.path) == ["App.app"])
}

@Test func systemToolRunnerReportsFailures() {
    #expect(throws: UpdateError.self) { try SystemToolRunner().run("/usr/bin/false", []) }
    #expect(throws: UpdateError.self) { try SystemToolRunner().run("/nonexistent/tool", []) }
    #expect((try? SystemToolRunner().run("/bin/echo", ["hi"])).map { String(decoding: $0, as: UTF8.self) } == "hi\n")
}
#endif
