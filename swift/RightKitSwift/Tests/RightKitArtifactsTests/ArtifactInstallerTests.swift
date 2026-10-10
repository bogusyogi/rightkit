import Foundation
import Testing

@testable import RightKitArtifacts

private func scratch() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("rightkit-art-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
}

/// Build a .tar.gz with the system tar, so the tests exercise real archives rather than a hand-rolled
/// encoder that could agree with a hand-rolled parser and prove nothing.
@discardableResult
private func makeTarGz(in root: URL, named name: String, build: (URL) throws -> Void) throws -> URL {
    let stage = root.appendingPathComponent("stage-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: stage, withIntermediateDirectories: true)
    try build(stage)

    let archive = root.appendingPathComponent(name)
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/tar")
    process.arguments = ["-czf", archive.path, "-C", stage.path] + (try FileManager.default
        .contentsOfDirectory(atPath: stage.path))
    try process.run()
    process.waitUntilExit()
    #expect(process.terminationStatus == 0)
    return archive
}

private func writeModel(_ dir: URL, contents: String = "weights") throws {
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    try contents.write(to: dir.appendingPathComponent("model.bin"), atomically: true, encoding: .utf8)
}

/// A one-file "unified" artifact whose model.bin holds `version`.
private func versionedArchive(_ version: String, in root: URL) throws -> URL {
    try makeTarGz(in: root, named: "\(version)-\(UUID().uuidString).tar.gz") { stage in
        try writeModel(stage.appendingPathComponent("unified"), contents: version)
    }
}

private func descriptor(_ digest: String? = nil, version: String? = nil) -> ArtifactDescriptor {
    ArtifactDescriptor(topLevelDirectory: "unified", expectedSHA256: digest, version: version)
}

private let unverified = InstallOptions(allowUnverified: true)

private func activeContents(_ dest: URL) throws -> String {
    try String(contentsOf: dest.appendingPathComponent("unified/model.bin"), encoding: .utf8)
}

private func entries(_ dest: URL) -> [String] {
    (try? FileManager.default.contentsOfDirectory(atPath: dest.path)) ?? []
}

/// Runs an install that must fail with `expected`, without relying on Swift Testing's async throws matching.
private func expectInstallError<T>(
    _ expected: ArtifactError,
    _ body: () async throws -> T,
    sourceLocation: SourceLocation = #_sourceLocation,
) async {
    do {
        _ = try await body()
        Issue.record("expected \(expected), but the call succeeded", sourceLocation: sourceLocation)
    } catch let error as ArtifactError {
        #expect(error == expected, sourceLocation: sourceLocation)
    } catch {
        Issue.record("expected \(expected), got \(error)", sourceLocation: sourceLocation)
    }
}

/// Signals that a test "crashed" at a fault point. The snapshot already holds the on-disk state.
private struct SimulatedCrash: Error {}

/// Copies the destination root to `snapshot` at `point`, then throws, so the snapshot is the state
/// a process killed at that instant would leave behind.
private func crashAt(_ point: ArtifactInstaller.ActivationFault, snapshot: URL) -> ArtifactInstaller.FaultInjector {
    { fault, root in
        guard fault == point else { return }
        try FileManager.default.copyItem(at: root, to: snapshot)
        throw SimulatedCrash()
    }
}

/// Installs v1, then starts a v2 install that crashes at `point`. Returns the crashed root's snapshot.
private func installThenCrash(
    _ point: ArtifactInstaller.ActivationFault,
    root: URL,
    dest: URL,
) async throws -> URL {
    _ = try await ArtifactInstaller.install(
        archive: try versionedArchive("v1", in: root),
        descriptor: descriptor(),
        destinationRoot: dest,
        options: unverified,
    )
    let snapshot = root.appendingPathComponent("crash-\(UUID().uuidString)")
    let archive = try versionedArchive("v2", in: root)
    do {
        _ = try await ArtifactInstaller.performInstall(
            archive: archive,
            descriptor: descriptor(),
            destinationRoot: dest,
            options: unverified,
            fault: crashAt(point, snapshot: snapshot),
        )
        Issue.record("the fault at \(point) did not fire")
    } catch is SimulatedCrash {
    }
    return snapshot
}

@Test func installsAndVerifiesChecksum() async throws {
    let root = try scratch()
    let archive = try makeTarGz(in: root, named: "a.tar.gz") { stage in
        try writeModel(stage.appendingPathComponent("unified"))
    }
    let digest = try ArtifactInstaller.sha256(of: archive)
    let dest = root.appendingPathComponent("install")

    let installed = try await ArtifactInstaller.install(
        archive: archive,
        descriptor: descriptor(digest),
        destinationRoot: dest,
    )

    #expect(FileManager.default.fileExists(atPath: installed.appendingPathComponent("model.bin").path))
}

@Test func rejectsAChecksumMismatchBeforeTouchingAnything() async throws {
    let root = try scratch()
    let archive = try makeTarGz(in: root, named: "a.tar.gz") { stage in
        try writeModel(stage.appendingPathComponent("unified"))
    }
    let dest = root.appendingPathComponent("install")
    let wrong = String(repeating: "0", count: 64)
    let actual = try ArtifactInstaller.sha256(of: archive)

    await expectInstallError(.checksumMismatch(expected: wrong, actual: actual)) {
        try await ArtifactInstaller.install(archive: archive, descriptor: descriptor(wrong), destinationRoot: dest)
    }
    #expect(!FileManager.default.fileExists(atPath: dest.appendingPathComponent("unified").path))
}

/// The 0.6.0 fail-closed rule: a missing digest is refused before the archive is read.
@Test func aNilDigestFailsClosedUnlessExplicitlyAllowed() async throws {
    let root = try scratch()
    let archive = try versionedArchive("v1", in: root)
    let dest = root.appendingPathComponent("install")

    await expectInstallError(.missingExpectedDigest) {
        try await ArtifactInstaller.install(archive: archive, descriptor: descriptor(), destinationRoot: dest)
    }
    #expect(!FileManager.default.fileExists(atPath: dest.path), "nothing is created when the digest is missing")

    try await ArtifactInstaller.install(archive: archive, descriptor: descriptor(), destinationRoot: dest, options: unverified)
    #expect(try activeContents(dest) == "v1")
}

@Test func rejectsTraversalAbsoluteAndForeignRoots() async throws {
    let root = try scratch()
    // A member that climbs out of the artifact directory.
    let escaping = try makeTarGz(in: root, named: "escape.tar.gz") { stage in
        let outside = stage.appendingPathComponent("unified/../evil")
        try writeModel(outside.standardizedFileURL)
        try writeModel(stage.appendingPathComponent("unified"))
    }
    // An archive whose root is simply not the one we asked for.
    let foreign = try makeTarGz(in: root, named: "foreign.tar.gz") { stage in
        try writeModel(stage.appendingPathComponent("somethingelse"))
    }

    for archive in [escaping, foreign] {
        do {
            try await ArtifactInstaller.install(
                archive: archive,
                descriptor: descriptor(),
                destinationRoot: root.appendingPathComponent("install-\(UUID().uuidString)"),
                options: unverified,
            )
            Issue.record("a hostile archive was installed")
        } catch let error as ArtifactError {
            _ = error  // any ArtifactError is a rejection
        }
    }
}

@Test func rejectsSymlinkMembers() async throws {
    let root = try scratch()
    let archive = try makeTarGz(in: root, named: "link.tar.gz") { stage in
        let dir = stage.appendingPathComponent("unified")
        try writeModel(dir)
        // A symlink pointing at the user's keys is the whole reason this fails closed.
        try FileManager.default.createSymbolicLink(
            at: dir.appendingPathComponent("escape"),
            withDestinationURL: URL(fileURLWithPath: "/etc/passwd"),
        )
    }

    await expectInstallError(.unsupportedArchiveMember("unified/escape")) {
        try await ArtifactInstaller.install(
            archive: archive,
            descriptor: descriptor(),
            destinationRoot: root.appendingPathComponent("install"),
            options: unverified,
        )
    }
}

@Test func rejectsAnInflatedArchiveOverTheCap() async throws {
    let root = try scratch()
    let archive = try makeTarGz(in: root, named: "big.tar.gz") { stage in
        // Compresses tiny, expands large — the zip-bomb shape.
        try writeModel(stage.appendingPathComponent("unified"), contents: String(repeating: "a", count: 2_000_000))
    }

    await expectInstallError(.inflatedArchiveTooLarge) {
        try await ArtifactInstaller.install(
            archive: archive,
            descriptor: ArtifactDescriptor(topLevelDirectory: "unified", expectedSHA256: nil, maxInflatedBytes: 4096),
            destinationRoot: root.appendingPathComponent("install"),
            options: unverified,
        )
    }
}

@Test func rejectsATruncatedArchive() async throws {
    let root = try scratch()
    let archive = try makeTarGz(in: root, named: "trunc.tar.gz") { stage in
        try writeModel(stage.appendingPathComponent("unified"), contents: String(repeating: "x", count: 100_000))
    }
    let data = try Data(contentsOf: archive)
    try data.prefix(data.count / 2).write(to: archive)

    do {
        try await ArtifactInstaller.install(
            archive: archive,
            descriptor: descriptor(),
            destinationRoot: root.appendingPathComponent("install"),
            options: unverified,
        )
        Issue.record("a truncated archive was installed")
    } catch let error as ArtifactError {
        _ = error
    }
}

/// The §4 gate: no failed install may delete or expose a partially installed current artifact.
@Test func aFailedInstallRollsBackToThePriorArtifact() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    let good = try makeTarGz(in: root, named: "good.tar.gz") { stage in
        try writeModel(stage.appendingPathComponent("unified"), contents: "v1")
    }
    try await ArtifactInstaller.install(archive: good, descriptor: descriptor(), destinationRoot: dest, options: unverified)

    let bad = try makeTarGz(in: root, named: "bad.tar.gz") { stage in
        let dir = stage.appendingPathComponent("unified")
        try writeModel(dir, contents: "v2")
        try FileManager.default.createSymbolicLink(
            at: dir.appendingPathComponent("escape"),
            withDestinationURL: URL(fileURLWithPath: "/etc/passwd"),
        )
    }
    do {
        try await ArtifactInstaller.install(archive: bad, descriptor: descriptor(), destinationRoot: dest, options: unverified)
        Issue.record("a bad archive was installed")
    } catch let error as ArtifactError {
        _ = error
    }

    // The previous artifact survives, intact and complete — not half-replaced by v2.
    #expect(try activeContents(dest) == "v1")
    #expect(!FileManager.default.fileExists(atPath: dest.appendingPathComponent("unified/escape").path))
    #expect(recoveryLeftovers(dest).isEmpty, "a rejected install leaves no aside, marker or staging behind")
}

// MARK: - Required files

@Test func aMissingRequiredFileRejectsBeforeActivationAndKeepsThePrevious() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    try await ArtifactInstaller.install(archive: try versionedArchive("v1", in: root), descriptor: descriptor(), destinationRoot: dest, options: unverified)

    var options = unverified
    options.requiredFiles = ["model.bin", "VERSION"]
    await expectInstallError(.missingRequiredFile("VERSION")) {
        try await ArtifactInstaller.install(archive: try versionedArchive("v2", in: root), descriptor: descriptor(), destinationRoot: dest, options: options)
    }
    #expect(try activeContents(dest) == "v1")
    #expect(recoveryLeftovers(dest).isEmpty)
}

@Test func requiredFilePathsCannotEscapeTheArtifact() async throws {
    let root = try scratch()
    for bad in ["../secret", "/etc/passwd", "a//b", ""] {
        var options = unverified
        options.requiredFiles = [bad]
        await expectInstallError(.invalidRequiredFilePath(bad)) {
            try await ArtifactInstaller.install(archive: try versionedArchive("v1", in: root), descriptor: descriptor(), destinationRoot: root.appendingPathComponent("install-\(UUID().uuidString)"), options: options)
        }
    }
}

// MARK: - Manifest

@Test func theManifestRecordsFilesDigestsAndVersion() async throws {
    let root = try scratch()
    let archive = try versionedArchive("v1", in: root)
    let digest = try ArtifactInstaller.sha256(of: archive)
    let dest = root.appendingPathComponent("install")

    try await ArtifactInstaller.install(archive: archive, descriptor: descriptor(digest, version: "2.0.0"), destinationRoot: dest)

    let manifest = try #require(try ArtifactInstaller.readManifest(destinationRoot: dest, topLevelDirectory: "unified"))
    #expect(manifest.version == "2.0.0")
    #expect(manifest.archiveSHA256 == digest)
    #expect(manifest.topLevelDirectory == "unified")
    let model = try #require(manifest.files.first { $0.path == "model.bin" })
    #expect(model.size == 2)
    #expect(model.sha256 == (try ArtifactInstaller.sha256(of: dest.appendingPathComponent("unified/model.bin"))))
    #expect(!manifest.files.contains { $0.path == ArtifactManifest.fileName }, "the manifest does not list itself")
}

@Test func anUnverifiedInstallRecordsNoArchiveDigest() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    try await ArtifactInstaller.install(archive: try versionedArchive("v1", in: root), descriptor: descriptor(), destinationRoot: dest, options: unverified)
    let manifest = try #require(try ArtifactInstaller.readManifest(destinationRoot: dest, topLevelDirectory: "unified"))
    #expect(manifest.archiveSHA256 == nil)
}

@Test func readManifestIsNilBeforeAnyInstall() throws {
    let root = try scratch()
    #expect(try ArtifactInstaller.readManifest(destinationRoot: root, topLevelDirectory: "unified") == nil)
}

// MARK: - Post-activation validation

@Test func aPassingValidationKeepsTheNewInstallAndDiscardsTheAside() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    try await ArtifactInstaller.install(archive: try versionedArchive("v1", in: root), descriptor: descriptor(), destinationRoot: dest, options: unverified)

    let aside = dest.appendingPathComponent(".aside-unified")
    var options = unverified
    // The previous install must still be on disk while validation runs.
    options.postActivationValidation = { _ in
        guard FileManager.default.fileExists(atPath: aside.path) else { throw SimulatedCrash() }
    }
    try await ArtifactInstaller.install(archive: try versionedArchive("v2", in: root), descriptor: descriptor(), destinationRoot: dest, options: options)

    #expect(try activeContents(dest) == "v2")
    #expect(recoveryLeftovers(dest).isEmpty, "the aside is discarded only after validation passes")
}

@Test func aFailingValidationRestoresThePreviousInstall() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    try await ArtifactInstaller.install(archive: try versionedArchive("v1", in: root), descriptor: descriptor(), destinationRoot: dest, options: unverified)

    var options = unverified
    options.postActivationValidation = { _ in throw SimulatedCrash() }
    await expectInstallError(.postActivationValidationFailed(String(describing: SimulatedCrash()))) {
        try await ArtifactInstaller.install(archive: try versionedArchive("v2", in: root), descriptor: descriptor(), destinationRoot: dest, options: options)
    }

    #expect(try activeContents(dest) == "v1")
    #expect(recoveryLeftovers(dest).isEmpty)
}

// MARK: - Crash recovery

@Test func aCrashAfterTheAsideMoveRestoresThePreviousInstall() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    let snapshot = try await installThenCrash(.afterAsideMove, root: root, dest: dest)

    #expect(entries(snapshot).contains(".aside-unified"))
    #expect(!entries(snapshot).contains("unified"))
    let outcome = try await ArtifactInstaller.recoverInterruptedInstall(destinationRoot: snapshot, topLevelDirectory: "unified")
    #expect(outcome == .rolledBack)
    #expect(try activeContents(snapshot) == "v1")
    #expect(recoveryLeftovers(snapshot).isEmpty)
}

@Test func aCrashAfterActivationRollsBackTheUnvalidatedInstall() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    let snapshot = try await installThenCrash(.afterActivation, root: root, dest: dest)

    // The new install is in place, but nothing accepted it yet: it must not stay active unvalidated.
    #expect(try activeContents(snapshot) == "v2")
    let outcome = try await ArtifactInstaller.recoverInterruptedInstall(destinationRoot: snapshot, topLevelDirectory: "unified")
    #expect(outcome == .rolledBack)
    #expect(try activeContents(snapshot) == "v1")
    #expect(recoveryLeftovers(snapshot).isEmpty)
}

@Test func aCrashBeforeValidationWithoutAValidatorRollsBack() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    let snapshot = try await installThenCrash(.beforeValidation, root: root, dest: dest)

    let outcome = try await ArtifactInstaller.recoverInterruptedInstall(destinationRoot: snapshot, topLevelDirectory: "unified")
    #expect(outcome == .rolledBack)
    #expect(try activeContents(snapshot) == "v1")
    #expect(recoveryLeftovers(snapshot).isEmpty)
}

@Test func aCrashBeforeValidationWithAPassingValidatorCompletesTheInstall() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    let snapshot = try await installThenCrash(.beforeValidation, root: root, dest: dest)

    let outcome = try await ArtifactInstaller.recoverInterruptedInstall(
        destinationRoot: snapshot,
        topLevelDirectory: "unified",
        postActivationValidation: { _ in },
    )
    #expect(outcome == .completed)
    #expect(try activeContents(snapshot) == "v2")
    #expect(recoveryLeftovers(snapshot).isEmpty)
}

@Test func aCrashBeforeValidationWithAFailingValidatorRollsBack() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    let snapshot = try await installThenCrash(.beforeValidation, root: root, dest: dest)

    let outcome = try await ArtifactInstaller.recoverInterruptedInstall(
        destinationRoot: snapshot,
        topLevelDirectory: "unified",
        postActivationValidation: { _ in throw SimulatedCrash() },
    )
    #expect(outcome == .rolledBack)
    #expect(try activeContents(snapshot) == "v1")
}

@Test func aCrashOnAFirstInstallLeavesNoArtifactBehind() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    let archive = try versionedArchive("v2", in: root)
    let snapshot = root.appendingPathComponent("crash-first")
    do {
        _ = try await ArtifactInstaller.performInstall(
            archive: archive,
            descriptor: descriptor(),
            destinationRoot: dest,
            options: unverified,
            fault: crashAt(.afterActivation, snapshot: snapshot),
        )
    } catch is SimulatedCrash {
    }

    let outcome = try await ArtifactInstaller.recoverInterruptedInstall(destinationRoot: snapshot, topLevelDirectory: "unified")
    #expect(outcome == .rolledBack)
    #expect(!FileManager.default.fileExists(atPath: snapshot.appendingPathComponent("unified").path))
    #expect(recoveryLeftovers(snapshot).isEmpty)
}

@Test func anInstallAfterACrashRecoversFirstAndLeavesNoAside() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    let snapshot = try await installThenCrash(.beforeValidation, root: root, dest: dest)

    try await ArtifactInstaller.install(archive: try versionedArchive("v3", in: root), descriptor: descriptor(), destinationRoot: snapshot, options: unverified)
    #expect(try activeContents(snapshot) == "v3")
    #expect(recoveryLeftovers(snapshot).isEmpty)
}

@Test func aCorruptMarkerFailsClosedInsteadOfGuessing() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    try await ArtifactInstaller.install(archive: try versionedArchive("v1", in: root), descriptor: descriptor(), destinationRoot: dest, options: unverified)
    try Data("not json".utf8).write(to: dest.appendingPathComponent(".pending-unified.json"))

    await expectInstallError(.recoveryMarkerUnreadable) {
        try await ArtifactInstaller.recoverInterruptedInstall(destinationRoot: dest, topLevelDirectory: "unified")
    }
    #expect(try activeContents(dest) == "v1", "an unreadable marker never changes the active install")
}

@Test func aSecondInstallOfTheSameDirectoryFailsFastWhileTheLockIsHeld() async throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    try FileManager.default.createDirectory(at: dest, withIntermediateDirectories: true)
    let fd = open(dest.appendingPathComponent(".install-unified.lock").path, O_RDWR | O_CREAT, 0o600)
    #expect(flock(fd, LOCK_EX | LOCK_NB) == 0)
    defer { flock(fd, LOCK_UN); close(fd) }

    await expectInstallError(.installInProgress) {
        try await ArtifactInstaller.install(archive: try versionedArchive("v1", in: root), descriptor: descriptor(), destinationRoot: dest, options: unverified)
    }
}

/// Anything an interrupted or completed activation could leave in the destination root.
private func recoveryLeftovers(_ root: URL) -> [String] {
    entries(root).filter {
        $0.hasPrefix(".aside-") || $0.hasPrefix(".pending-") || $0.hasPrefix(".trash-") || $0.hasPrefix(".staging-")
    }
}
