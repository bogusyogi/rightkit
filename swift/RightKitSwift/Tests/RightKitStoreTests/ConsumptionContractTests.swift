import Foundation
import Testing

/// The one distribution contract: git URL + exact version tag (see docs/RIGHTKIT-SHARED-PACKAGES.md,
/// Swift section). This resolves the package the way a consuming app does, from a fresh clone of a
/// tagged copy of this package, with an `exact:` pin.
@Suite struct ConsumptionContractTests {
    private static let packageRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()

    private func run(_ tool: String, _ args: [String], in directory: URL) throws -> (status: Int32, output: String) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = [tool] + args
        process.currentDirectoryURL = directory
        var env = ProcessInfo.processInfo.environment
        env["GIT_CONFIG_GLOBAL"] = "/dev/null"
        process.environment = env
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        try process.run()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return (process.terminationStatus, String(decoding: data, as: UTF8.self))
    }

    @Test func versionFileIsExactSemverAndTheReadmePinsExactlyThatVersion() throws {
        let version = try String(contentsOf: Self.packageRoot.appendingPathComponent("VERSION"), encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        #expect(version.range(of: #"^\d+\.\d+\.\d+$"#, options: .regularExpression) != nil)
        let readme = try String(contentsOf: Self.packageRoot.appendingPathComponent("README.md"), encoding: .utf8)
        #expect(readme.contains(#"exact: "\#(version)""#), "README consumption example pins the current version exactly")
        #expect(!readme.contains("from: \""), "no floating version range in the consumption contract")
    }

    @Test func freshConsumerResolvesTheTaggedPackageWithAnExactPin() throws {
        let version = try String(contentsOf: Self.packageRoot.appendingPathComponent("VERSION"), encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let work = FileManager.default.temporaryDirectory.appendingPathComponent("rk-consume-\(UUID().uuidString)")
        let repo = work.appendingPathComponent("rightkit-swift")
        let app = work.appendingPathComponent("app")
        defer { try? FileManager.default.removeItem(at: work) }
        try FileManager.default.createDirectory(at: repo, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: app.appendingPathComponent("Sources/App"), withIntermediateDirectories: true)
        // The published mirror is the package tree minus build output.
        for entry in ["Package.swift", "VERSION", "README.md", "Sources", "Tests"] {
            try FileManager.default.copyItem(at: Self.packageRoot.appendingPathComponent(entry), to: repo.appendingPathComponent(entry))
        }
        for step in [["init", "-q"], ["add", "-A"], ["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "release"], ["tag", version]] {
            let r = try run("git", step, in: repo)
            #expect(r.status == 0, "git \(step.joined(separator: " ")): \(r.output)")
        }
        try """
        // swift-tools-version: 6.0
        import PackageDescription
        let package = Package(
            name: "App", platforms: [.iOS(.v17), .macOS(.v14)],
            dependencies: [.package(url: "file://\(repo.path)", exact: "\(version)")],
            targets: [.target(name: "App", dependencies: [
                .product(name: "RightKitCloudSync", package: "rightkit-swift"),
                .product(name: "RightKitStore", package: "rightkit-swift"),
            ])]
        )
        """.write(to: app.appendingPathComponent("Package.swift"), atomically: true, encoding: .utf8)
        try "import RightKitStore\nimport RightKitCloudSync\npublic let ok = true\n"
            .write(to: app.appendingPathComponent("Sources/App/App.swift"), atomically: true, encoding: .utf8)
        let resolved = try run("swift", ["package", "resolve"], in: app)
        #expect(resolved.status == 0, "\(resolved.output)")
        let pins = try String(contentsOf: app.appendingPathComponent("Package.resolved"), encoding: .utf8)
        #expect(pins.contains("\"version\" : \"\(version)\""), "resolved pin is the exact published tag")
    }
}
