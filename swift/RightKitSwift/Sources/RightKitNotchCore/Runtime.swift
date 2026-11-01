#if os(macOS)
import Foundation

/// Whether this process is an `xcodebuild test` host rather than a the app
/// somebody launched.
///
/// The unit bundle is hosted by the app itself, so a test run *is* a running
/// the app — and anything that would reach outside the process has to ask
/// first. Without the check every test run put a live request on the usage
/// endpoint, and every `NotchWindowController` a test constructed ordered a
/// real panel onto the developer's screen: a single `make test` put forty of
/// them up at once, over whatever was being worked on, for the half minute the
/// suite took.
///
/// The same answer holds for the CI view-shot renderer (`PULSE_RENDER_VIEWS=1`,
/// App/ViewShots.swift): it is an automated run on somebody's Mac that draws
/// from fixtures, so it must reach nothing real either.
///
/// Both detections exist only in a QA build (`PULSE_QA`); a shipped build is never "under test".
///
/// Owner's rule: no automated run touches real user data, read-only included
/// (the real home, Application Support, `~/.claude`, `~/.codex`, the keychain,
/// the app's UserDefaults, the pasteboard, the network). Every resolver whose
/// default is a real location calls `refuseUnderTest`, `realHome` or
/// `realApplicationSupport`, so a run that reaches one stops with a message
/// naming it instead of reading the folder. Inject a temporary directory.
public enum Runtime {
    /// The one path the app is installed at. macOS keys login items, background items and privacy
    /// grants (Removable Volumes, Files and Folders, Full Disk Access, screen) to path and
    /// identity, so a copy running from anywhere else (a build folder, a QA temp install, a
    /// mounted dmg) never registers a login item, the engine agent or the privileged helper:
    /// that would re-point the one registration at a path that goes away.
    public nonisolated static let fixedInstallPath = "/Applications/Pulse.app"

    public nonisolated static var isFixedInstall: Bool {
        Bundle.main.bundleURL.resolvingSymlinksInPath().path == fixedInstallPath
    }

    /// Forwards to `RuntimeFlags` (App/RuntimeFlags.swift), which the shared notch face reads.
    public static var isUnderTest: Bool { RuntimeFlags.isUnderTest }

    /// Traps when an automated run reaches `what`, which stands for real user data.
    /// An empty folder of this process's own, for code that must have somewhere to keep
    /// files while under test. Never the real home.
    public static let testScratch: URL = {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("pulse-under-test-\(ProcessInfo.processInfo.processIdentifier)", isDirectory: true)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }()

    public static func refuseUnderTest(_ what: String) {
        guard isUnderTest else { return }
        // Written out first: a release build's trap carries no message, and without one the
        // only sign in a CI log is a run that ended early.
        fputs("refused under test: \(what) would reach real user data; inject a temporary location instead\n", stderr)
        fatalError("\(what) would reach real user data under test")
    }

    /// The real home folder, refused under test.
    public static func realHome(_ what: String) -> URL {
        refuseUnderTest(what)
        return URL(fileURLWithPath: AccountHome.path, isDirectory: true)
    }

    /// `NSHomeDirectory()`, refused under test.
    public static func realHomePath(_ what: String) -> String {
        refuseUnderTest(what)
        return AccountHome.path
    }

    /// The real `~/Library/Application Support`, refused under test.
    public static func realApplicationSupport(_ what: String) -> URL {
        refuseUnderTest(what)
        return URL(fileURLWithPath: AccountHome.path, isDirectory: true)
            .appendingPathComponent("Library/Application Support", isDirectory: true)
    }
}
#endif
