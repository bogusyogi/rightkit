import Foundation
import Testing
@testable import RightKitFS

@Suite struct RightKitAppRootOverrideTests {
    func base() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("rkfs-root-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    @Test func explicitRootIsUsedVerbatim() throws {
        let folder = try base().appendingPathComponent("Pulse")
        let root = try RightKitAppRoot(root: folder, name: "Pulse")
        #expect(root.root == folder.resolvingSymlinksInPath())
        #expect(!root.root.path.contains("Orthic Labs"))
        #expect(FileManager.default.fileExists(atPath: folder.path))
    }

    @Test func atomicWriteAndEscapeRulesApplyUnderTheOverride() throws {
        let root = try RightKitAppRoot(root: try base().appendingPathComponent("Pulse"), name: "Pulse")
        try root.atomicWrite(Data("x".utf8), to: "launcher/rates.json")
        #expect(try Data(contentsOf: root.root.appendingPathComponent("launcher/rates.json")) == Data("x".utf8))
        #expect(throws: RightKitFSError.pathEscape("../x")) { try root.atomicWrite(Data(), to: "../x") }
    }

    @Test func nameIsStillValidated() throws {
        #expect(throws: RightKitFSError.invalidName("../x")) {
            try RightKitAppRoot(root: try base().appendingPathComponent("a"), name: "../x")
        }
    }
}
