import Foundation
import Testing
@testable import RightKitFS

@Suite struct RightKitFSTests {
    func make(_ app: String = "Pulse") throws -> (RightKitAppRoot, URL) {
        let base = FileManager.default.temporaryDirectory.appendingPathComponent("rkfs-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        return (try RightKitAppRoot(app: app, baseOverride: base), base)
    }

    @Test func nameValidationMatchesRust() {
        for ok in ["Pulse", "a", "my-app_1.x"] { #expect(RightKitAppRoot.isValidAppName(ok)) }
        for bad in ["", "-a", "a-", "a..b", "shared", "Shared", "CON", "con.txt", "a/b", "a b", String(repeating: "a", count: 65)] {
            #expect(!RightKitAppRoot.isValidAppName(bad), "\(bad)")
        }
    }

    @Test func rootIsOrthicLabsApp() throws {
        let (r, _) = try make()
        #expect(r.root.path.hasSuffix("Orthic Labs/Pulse"))
        #expect(throws: RightKitFSError.invalidName("../x")) { try RightKitAppRoot(app: "../x") }
    }

    @Test func dataDirRejectsEscapes() throws {
        let (r, base) = try make()
        let d = try r.dataDir("logs/sub")
        #expect(FileManager.default.fileExists(atPath: d.path))
        for bad in ["../x", "a/../../x", "/etc", "", "a\0b"] {
            #expect(throws: RightKitFSError.pathEscape(bad)) { try r.dataDir(bad) }
        }
        let outside = base.appendingPathComponent("outside")
        try FileManager.default.createDirectory(at: outside, withIntermediateDirectories: true)
        try FileManager.default.createSymbolicLink(at: r.root.appendingPathComponent("link"), withDestinationURL: outside)
        #expect(throws: RightKitFSError.pathEscape("link/x")) { try r.dataDir("link/x") }
        #expect(throws: RightKitFSError.pathEscape("link/f")) { try r.atomicWrite(Data("x".utf8), to: "link/f") }
    }

    @Test func atomicWriteReplacesAndIsPrivate() throws {
        let (r, _) = try make()
        try r.atomicWrite(Data("one".utf8), to: "a/b.txt")
        try r.atomicWrite(Data("two".utf8), to: "a/b.txt")
        let u = try r.path("a/b.txt")
        #expect(try Data(contentsOf: u) == Data("two".utf8))
        let mode = try FileManager.default.attributesOfItem(atPath: u.path)[.posixPermissions] as? Int
        #expect(mode == 0o600)
        let left = try FileManager.default.contentsOfDirectory(atPath: u.deletingLastPathComponent().path)
        #expect(left == ["b.txt"])
    }

    @Test func atomicWriteRefusesSymlinkDestination() throws {
        let (r, base) = try make()
        let target = base.appendingPathComponent("t.txt")
        try Data("t".utf8).write(to: target)
        try FileManager.default.createSymbolicLink(at: r.root.appendingPathComponent("l.txt"), withDestinationURL: target)
        #expect(throws: RightKitFSError.self) { try r.atomicWrite(Data("x".utf8), to: "l.txt") }
        #expect(try Data(contentsOf: target) == Data("t".utf8))
    }

    @Test func copyVerifiedChecksDigest() throws {
        let (r, base) = try make()
        let src = base.appendingPathComponent("src.bin")
        try Data("hello".utf8).write(to: src)
        let want = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        #expect(try r.copyVerified(from: src, to: "c/dst.bin", expectedSHA256: want.uppercased()) == want)
        #expect(try Data(contentsOf: r.path("c/dst.bin")) == Data("hello".utf8))
        #expect(throws: RightKitFSError.self) { try r.copyVerified(from: src, to: "c/bad.bin", expectedSHA256: "00") }
        #expect(!FileManager.default.fileExists(atPath: try r.path("c/bad.bin").path))
        let link = base.appendingPathComponent("lnk")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: src)
        #expect(throws: RightKitFSError.symlink(link.path)) { try r.copyVerified(from: link, to: "c/x") }
    }

    @Test func appendRecordLineDelimited() throws {
        let (r, _) = try make()
        try r.appendRecord(Data("a".utf8), to: "log/x.jsonl")
        try r.appendRecord(Data("b".utf8), to: "log/x.jsonl")
        #expect(try String(contentsOf: r.path("log/x.jsonl"), encoding: .utf8) == "a\nb\n")
        #expect(throws: RightKitFSError.self) { try r.appendRecord(Data("a\nb".utf8), to: "log/x.jsonl") }
    }
}
