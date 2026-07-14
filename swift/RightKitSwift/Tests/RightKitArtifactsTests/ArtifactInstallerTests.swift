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

@Test func installsAndVerifiesChecksum() throws {
    let root = try scratch()
    let archive = try makeTarGz(in: root, named: "a.tar.gz") { stage in
        try writeModel(stage.appendingPathComponent("unified"))
    }
    let digest = try ArtifactInstaller.sha256(of: archive)
    let dest = root.appendingPathComponent("install")

    let installed = try ArtifactInstaller.install(
        archive: archive,
        descriptor: ArtifactDescriptor(topLevelDirectory: "unified", expectedSHA256: digest),
        destinationRoot: dest,
    )

    #expect(FileManager.default.fileExists(atPath: installed.appendingPathComponent("model.bin").path))
}

@Test func rejectsAChecksumMismatchBeforeTouchingAnything() throws {
    let root = try scratch()
    let archive = try makeTarGz(in: root, named: "a.tar.gz") { stage in
        try writeModel(stage.appendingPathComponent("unified"))
    }
    let dest = root.appendingPathComponent("install")
    let wrong = String(repeating: "0", count: 64)

    #expect(throws: ArtifactError.self) {
        try ArtifactInstaller.install(
            archive: archive,
            descriptor: ArtifactDescriptor(topLevelDirectory: "unified", expectedSHA256: wrong),
            destinationRoot: dest,
        )
    }
    #expect(!FileManager.default.fileExists(atPath: dest.appendingPathComponent("unified").path))
}

@Test func rejectsTraversalAbsoluteAndForeignRoots() throws {
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
        #expect(throws: ArtifactError.self) {
            try ArtifactInstaller.install(
                archive: archive,
                descriptor: ArtifactDescriptor(topLevelDirectory: "unified", expectedSHA256: nil),
                destinationRoot: root.appendingPathComponent("install-\(UUID().uuidString)"),
            )
        }
    }
}

@Test func rejectsSymlinkMembers() throws {
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

    #expect(throws: ArtifactError.unsupportedArchiveMember("unified/escape")) {
        try ArtifactInstaller.install(
            archive: archive,
            descriptor: ArtifactDescriptor(topLevelDirectory: "unified", expectedSHA256: nil),
            destinationRoot: root.appendingPathComponent("install"),
        )
    }
}

@Test func rejectsAnInflatedArchiveOverTheCap() throws {
    let root = try scratch()
    let archive = try makeTarGz(in: root, named: "big.tar.gz") { stage in
        // Compresses tiny, expands large — the zip-bomb shape.
        try writeModel(stage.appendingPathComponent("unified"), contents: String(repeating: "a", count: 2_000_000))
    }

    #expect(throws: ArtifactError.inflatedArchiveTooLarge) {
        try ArtifactInstaller.install(
            archive: archive,
            descriptor: ArtifactDescriptor(topLevelDirectory: "unified", expectedSHA256: nil, maxInflatedBytes: 4096),
            destinationRoot: root.appendingPathComponent("install"),
        )
    }
}

@Test func rejectsATruncatedArchive() throws {
    let root = try scratch()
    let archive = try makeTarGz(in: root, named: "trunc.tar.gz") { stage in
        try writeModel(stage.appendingPathComponent("unified"), contents: String(repeating: "x", count: 100_000))
    }
    let data = try Data(contentsOf: archive)
    try data.prefix(data.count / 2).write(to: archive)

    #expect(throws: ArtifactError.self) {
        try ArtifactInstaller.install(
            archive: archive,
            descriptor: ArtifactDescriptor(topLevelDirectory: "unified", expectedSHA256: nil),
            destinationRoot: root.appendingPathComponent("install"),
        )
    }
}

/// The §4 gate: no failed install may delete or expose a partially installed current artifact.
@Test func aFailedInstallRollsBackToThePriorArtifact() throws {
    let root = try scratch()
    let dest = root.appendingPathComponent("install")
    let good = try makeTarGz(in: root, named: "good.tar.gz") { stage in
        try writeModel(stage.appendingPathComponent("unified"), contents: "v1")
    }
    try ArtifactInstaller.install(
        archive: good,
        descriptor: ArtifactDescriptor(topLevelDirectory: "unified", expectedSHA256: nil),
        destinationRoot: dest,
    )

    let bad = try makeTarGz(in: root, named: "bad.tar.gz") { stage in
        let dir = stage.appendingPathComponent("unified")
        try writeModel(dir, contents: "v2")
        try FileManager.default.createSymbolicLink(
            at: dir.appendingPathComponent("escape"),
            withDestinationURL: URL(fileURLWithPath: "/etc/passwd"),
        )
    }
    #expect(throws: ArtifactError.self) {
        try ArtifactInstaller.install(
            archive: bad,
            descriptor: ArtifactDescriptor(topLevelDirectory: "unified", expectedSHA256: nil),
            destinationRoot: dest,
        )
    }

    // The previous artifact survives, intact and complete — not half-replaced by v2.
    let installed = try String(contentsOf: dest.appendingPathComponent("unified/model.bin"), encoding: .utf8)
    #expect(installed == "v1")
    #expect(!FileManager.default.fileExists(atPath: dest.appendingPathComponent("unified/escape").path))
}
