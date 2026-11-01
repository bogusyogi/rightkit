import RightKitNotchCore
import RightKitNotch
import Foundation

/// The Claude command-line account new terminals use (`pulse claude use <name>`).
/// the app's core writes `~/Library/Application Support/Orthic Labs/the app/claude-cli/active` and one
/// config folder per account under `accounts/`; this only reads the folder names.
/// Nothing in them is opened.
enum ClaudeCliAccount {
    private static var base: URL {
        if Runtime.isUnderTest {
            return StateRoot.installed(in: Runtime.testScratch).appendingPathComponent("claude-cli")
        }
        return StateRoot.installed("the Claude command-line account")
            .appendingPathComponent("claude-cli")
    }

    /// `["active": name?, "accounts": [names]]`, or nil when no account was ever made.
    static func published() -> [String: Any]? {
        let root = base
        let names = ((try? FileManager.default.contentsOfDirectory(
            atPath: root.appendingPathComponent("accounts").path)) ?? [])
            .filter { !$0.hasPrefix(".") }
            .sorted()
        guard !names.isEmpty else { return nil }
        let active = (try? String(contentsOf: root.appendingPathComponent("active"), encoding: .utf8))?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        var out: [String: Any] = ["accounts": names]
        if let active, names.contains(active) { out["active"] = active }
        return out
    }
}
