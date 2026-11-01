#if os(macOS)
import Foundation
import RightKitFS

/// The one place the macOS notch names Pulse's state folder, RightKit's per-app data root
/// `~/Library/Application Support/Orthic Labs/Pulse` (`RightKitAppRoot`, RightKitSwift 0.8.1; the same
/// folder `pulse_core::store::state_root` resolves). Every reader of it goes through here. Each caller
/// keeps its own rule for tests: the launcher uses `Runtime.testScratch`, the others refuse a real
/// folder under test through `Runtime.realApplicationSupport`, and an injected directory is used as given.
enum StateRoot {
    /// The name of Pulse's app root under `Orthic Labs`.
    nonisolated static let appName = "Pulse"

    /// The installed state folder, refused under test (`what` names the caller in the refusal).
    nonisolated static func installed(_ what: String) -> URL {
        installed(in: Runtime.realApplicationSupport(what))
    }

    /// The state folder under an Application Support folder the caller already holds (an injected
    /// folder, or one read from the system). The owner creates it private; when it cannot, the folder
    /// is still named at the same place so a reader fails on the missing file, not on the name.
    nonisolated static func installed(in applicationSupport: URL) -> URL {
        if let root = try? RightKitAppRoot(app: appName, baseOverride: applicationSupport) {
            return root.root
        }
        return applicationSupport
            .appendingPathComponent(RightKitAppRoot.suiteDirectory, isDirectory: true)
            .appendingPathComponent(appName, isDirectory: true)
    }
}
#endif
