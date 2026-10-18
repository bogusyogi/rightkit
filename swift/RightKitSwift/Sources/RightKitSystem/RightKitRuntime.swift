import Foundation

#if os(macOS)

public enum RightKitRuntimeError: Error, Equatable, Sendable {
    case refusedUnderTest(String)
}

/// Whether this process is an automated host rather than an app somebody launched, and the guards
/// that keep an automated run away from real user data.
///
/// A unit bundle hosted by the app is a running app: anything that reaches outside the process has
/// to ask first. Without the check every automated run puts live requests on real endpoints and real
/// windows on the developer's screen. The rule: no automated run touches real user data, read-only
/// included (the real home, Application Support, the keychain, the app's UserDefaults, the
/// pasteboard, the network). Every resolver whose default is a real location calls
/// `refuseUnderTest`, `realHome` or `realApplicationSupport`, so a run that reaches one stops with a
/// message naming it instead of reading the folder. Inject a temporary directory.
public enum RightKitRuntime {
    /// Environment variables that mark an XCTest host.
    public static let xctestEnvironmentKeys = ["XCTestConfigurationFilePath", "XCTestBundlePath", "XCTestSessionIdentifier"]

    /// Pure decision over an environment, so it is testable. `extraFlags` are variable names that
    /// mean "automated run" when set to `1` (an app's own render or tour mode).
    public static func evaluate(
        environment: [String: String], hasXCTestCaseClass: Bool = false, hasXCTestBundle: Bool = false,
        extraFlags: [String] = [],
    ) -> Bool {
        xctestEnvironmentKeys.contains { environment[$0] != nil }
            || hasXCTestCaseClass || hasXCTestBundle
            || extraFlags.contains { environment[$0] == "1" }
    }

    /// True in an XCTest host. Evaluated once. Apps with their own automated modes call
    /// `detect(extraFlags:)`.
    public static let isUnderTest: Bool = detect(extraFlags: [])

    public static func detect(extraFlags: [String]) -> Bool {
        evaluate(
            environment: ProcessInfo.processInfo.environment,
            hasXCTestCaseClass: NSClassFromString("XCTestCase") != nil,
            hasXCTestBundle: Bundle.allBundles.contains { $0.bundleURL.pathExtension == "xctest" },
            extraFlags: extraFlags)
    }

    /// An empty folder of this process's own, for code that must keep files while under test. Never
    /// the real home.
    public static let testScratch: URL = {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("rightkit-under-test-\(ProcessInfo.processInfo.processIdentifier)", isDirectory: true)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }()

    /// Throws when an automated run reaches `what`, which stands for real user data.
    public static func requireNotUnderTest(_ what: String) throws {
        if isUnderTest { throw RightKitRuntimeError.refusedUnderTest(what) }
    }

    /// Traps (after writing the reason to stderr) when an automated run reaches `what`. The message is
    /// written first: a release build's trap carries none, and without one the only sign in a CI log
    /// is a run that ended early.
    public static func refuseUnderTest(_ what: String) {
        guard isUnderTest else { return }
        FileHandle.standardError.write(Data(
            "refused under test: \(what) would reach real user data; inject a temporary location instead\n".utf8))
        fatalError("\(what) would reach real user data under test")
    }

    /// The real home folder, refused under test.
    public static func realHome(_ what: String) -> URL {
        refuseUnderTest(what)
        return FileManager.default.homeDirectoryForCurrentUser
    }

    /// The real `~/Library/Application Support`, refused under test.
    public static func realApplicationSupport(_ what: String) -> URL {
        refuseUnderTest(what)
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    }
}
#endif
