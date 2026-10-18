import Foundation
import Testing
@testable import RightKitFS

@Suite struct ContainedRootTests {
    func tmp() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("rkfs-c-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    func make() throws -> (RightKitContainedRoot, URL, URL) {
        let outer = try tmp()
        let outside = outer.appendingPathComponent("outside")
        try FileManager.default.createDirectory(at: outside, withIntermediateDirectories: true)
        let root = try RightKitContainedRoot(base: outer.appendingPathComponent("Workspaces"))
        return (root, outer, outside)
    }

    @Test func emptyPathIsTheRootOnlyWhenAllowed() throws {
        let (r, _, _) = try make()
        #expect(try r.resolve("") == r.base)
        #expect(try r.resolve(".") == r.base)
        #expect(throws: RightKitFSError.pathEscape("")) { try r.resolve("", allowRoot: false) }
    }

    @Test func rejectsDotDotAbsoluteAndNul() throws {
        let (r, _, _) = try make()
        for bad in ["../x", "a/../../x", "a/..", "/etc/passwd", "a\0b"] {
            #expect(throws: RightKitFSError.pathEscape(bad)) { try r.resolve(bad) }
        }
    }

    @Test func missingTailResolvesInsideAndSiblingPrefixIsNotInside() throws {
        let (r, outer, _) = try make()
        let u = try r.resolve("a/b/new.txt")
        #expect(u.path == r.base.path + "/a/b/new.txt")
        // "Workspaces-evil" shares a name prefix with the base: whole components are compared.
        let evil = outer.appendingPathComponent("Workspaces-evil")
        try FileManager.default.createDirectory(at: evil, withIntermediateDirectories: true)
        try FileManager.default.createSymbolicLink(at: r.base.appendingPathComponent("l"), withDestinationURL: evil)
        #expect(throws: RightKitFSError.pathEscape("l/x")) { try r.resolve("l/x") }
    }

    @Test func symlinkEscapesAreRefusedButInsideLinksAreFollowed() throws {
        let (r, _, outside) = try make()
        try FileManager.default.createSymbolicLink(at: r.base.appendingPathComponent("out"), withDestinationURL: outside)
        #expect(throws: RightKitFSError.pathEscape("out")) { try r.resolve("out") }
        #expect(throws: RightKitFSError.pathEscape("out/new.txt")) { try r.resolve("out/new.txt") }
        #expect(throws: RightKitFSError.pathEscape("out/new.txt")) { try r.atomicWrite(Data("x".utf8), to: "out/new.txt") }
        #expect(!FileManager.default.fileExists(atPath: outside.appendingPathComponent("new.txt").path))
        try FileManager.default.createDirectory(at: r.base.appendingPathComponent("real"), withIntermediateDirectories: true)
        try FileManager.default.createSymbolicLink(at: r.base.appendingPathComponent("alias"),
                                                   withDestinationURL: r.base.appendingPathComponent("real"))
        _ = try r.resolve("alias/f.txt")
    }

    @Test func danglingAndLoopingLinksAreRefused() throws {
        let (r, _, outside) = try make()
        try FileManager.default.createSymbolicLink(at: r.base.appendingPathComponent("dangling"),
                                                   withDestinationURL: outside.appendingPathComponent("nope.txt"))
        #expect(throws: RightKitFSError.pathEscape("dangling")) { try r.resolve("dangling") }
        #expect(throws: RightKitFSError.pathEscape("dangling/x")) { try r.resolve("dangling/x") }
        // A dangling link whose target is inside is refused as well: its target is unknown.
        try FileManager.default.createSymbolicLink(at: r.base.appendingPathComponent("inner"),
                                                   withDestinationURL: r.base.appendingPathComponent("absent"))
        #expect(throws: RightKitFSError.pathEscape("inner")) { try r.resolve("inner") }
        try FileManager.default.createSymbolicLink(at: r.base.appendingPathComponent("a"), withDestinationURL: r.base.appendingPathComponent("b"))
        try FileManager.default.createSymbolicLink(at: r.base.appendingPathComponent("b"), withDestinationURL: r.base.appendingPathComponent("a"))
        #expect(throws: (any Error).self) { try r.resolve("a/x") }
    }

    @Test func symlinkedBaseIsResolved() throws {
        let (r, outer, _) = try make()
        let alias = outer.appendingPathComponent("alias")
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: r.base)
        let viaAlias = try RightKitContainedRoot(base: alias)
        #expect(viaAlias.base == r.base)
        _ = try viaAlias.resolve("x/y")
    }

    @Test func atomicWriteCreatesParentsReplacesAndLeavesNoTemp() throws {
        let (r, _, _) = try make()
        try r.atomicWrite(Data("one".utf8), to: "exports/t.md")
        try r.atomicWrite(Data("two".utf8), to: "exports/t.md")
        let u = try r.resolve("exports/t.md")
        #expect(try Data(contentsOf: u) == Data("two".utf8))
        let mode = try FileManager.default.attributesOfItem(atPath: u.path)[.posixPermissions] as? Int
        #expect(mode == 0o600)
        #expect(try FileManager.default.contentsOfDirectory(atPath: u.deletingLastPathComponent().path) == ["t.md"])
        #expect(throws: RightKitFSError.pathEscape("")) { try r.atomicWrite(Data(), to: "") }
    }

    @Test func atomicWriteFailureCleansTempAndKeepsOriginal() throws {
        let (r, _, outside) = try make()
        try r.atomicWrite(Data("keep".utf8), to: "d/f.txt")
        // A directory at the target: refused, original sibling files untouched, no temp left.
        try FileManager.default.createDirectory(at: r.base.appendingPathComponent("d/dir"), withIntermediateDirectories: true)
        #expect(throws: RightKitFSError.self) { try r.atomicWrite(Data("x".utf8), to: "d/dir") }
        #expect(try FileManager.default.contentsOfDirectory(atPath: r.base.appendingPathComponent("d").path).sorted() == ["dir", "f.txt"])
        // A symlink at the target is refused and its target untouched.
        let t = outside.appendingPathComponent("t.txt")
        try Data("t".utf8).write(to: t)
        try FileManager.default.createSymbolicLink(at: r.base.appendingPathComponent("l.txt"), withDestinationURL: t)
        #expect(throws: RightKitFSError.self) { try r.atomicWrite(Data("x".utf8), to: "l.txt") }
        #expect(try Data(contentsOf: t) == Data("t".utf8))
    }
}

@Suite struct AppRootPrimitivesTests {
    func make() throws -> (RightKitAppRoot, URL) {
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("rkfs-p-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        return (try RightKitAppRoot(app: "Code", baseOverride: base), base)
    }

    @Test func readExistsRemoveList() throws {
        let (r, _) = try make()
        #expect(try r.readData("a/x.bin") == nil)
        #expect(!r.exists("a/x.bin"))
        try r.atomicWrite(Data("hi".utf8), to: "a/x.bin")
        try r.atomicWrite(Data("yo".utf8), to: "a/y.bin")
        #expect(try r.readData("a/x.bin") == Data("hi".utf8))
        #expect(r.exists("a/x.bin"))
        #expect(try r.list("a") == ["x.bin", "y.bin"])
        #expect(try r.list("missing") == [])
        #expect(try r.list("") == ["a"])
        try r.remove("a/x.bin")
        try r.remove("a/x.bin")
        #expect(!r.exists("a/x.bin"))
        #expect(!r.exists("../x"))
        #expect(throws: RightKitFSError.pathEscape("../x")) { try r.remove("../x") }
        #expect(throws: RightKitFSError.pathEscape("../x")) { try r.readData("../x") }
        #expect(throws: RightKitFSError.pathEscape("")) { try r.remove("") }
    }

    @Test func readRefusesSymlinksAndEscapes() throws {
        let (r, base) = try make()
        let secret = base.appendingPathComponent("secret.txt")
        try Data("s".utf8).write(to: secret)
        try FileManager.default.createSymbolicLink(at: r.root.appendingPathComponent("l"), withDestinationURL: secret)
        #expect(throws: RightKitFSError.pathEscape("l")) { try r.readData("l") }
        #expect(throws: RightKitFSError.pathEscape("l")) { try r.remove("l") }
        #expect(try Data(contentsOf: secret) == Data("s".utf8))
        // A link inside the root to a file inside the root: the final name is not followed.
        try r.atomicWrite(Data("in".utf8), to: "real.txt")
        try FileManager.default.createSymbolicLink(at: r.root.appendingPathComponent("in-link"),
                                                   withDestinationURL: r.root.appendingPathComponent("real.txt"))
        #expect(throws: RightKitFSError.symlink(try r.path("in-link").path)) { try r.readData("in-link") }
    }

    @Test func migrateMovesVerifiesAndIsOneShot() throws {
        let (r, base) = try make()
        let legacy = base.appendingPathComponent("old.json")
        try Data("legacy".utf8).write(to: legacy)
        #expect(try r.migrate(from: legacy, to: "drafts/new.json"))
        #expect(try r.readData("drafts/new.json") == Data("legacy".utf8))
        #expect(!FileManager.default.fileExists(atPath: legacy.path))
        // Destination exists: no-op, the second legacy file is left alone.
        try Data("other".utf8).write(to: legacy)
        #expect(try !r.migrate(from: legacy, to: "drafts/new.json"))
        #expect(try r.readData("drafts/new.json") == Data("legacy".utf8))
        #expect(FileManager.default.fileExists(atPath: legacy.path))
        // Absent source: no-op.
        #expect(try !r.migrate(from: base.appendingPathComponent("none"), to: "drafts/none.json"))
        #expect(!r.exists("drafts/none.json"))
    }

    @Test func migrateRefusesSymlinkSourceAndEscapes() throws {
        let (r, base) = try make()
        let real = base.appendingPathComponent("real")
        try Data("x".utf8).write(to: real)
        let link = base.appendingPathComponent("lnk")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: real)
        #expect(throws: RightKitFSError.symlink(link.path)) { try r.migrate(from: link, to: "a") }
        #expect(FileManager.default.fileExists(atPath: real.path))
        #expect(throws: RightKitFSError.pathEscape("../a")) { try r.migrate(from: real, to: "../a") }
        #expect(FileManager.default.fileExists(atPath: real.path))
    }
}
