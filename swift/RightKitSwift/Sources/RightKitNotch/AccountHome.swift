#if os(macOS)
import Foundation

/// The account's own home folder. `HOME` decides, except when it points into
/// `<account>/ClaudeProfiles/`: a Pulse started from a shell inside a second Claude profile
/// (Claude B) inherits that `HOME`, and it and its children must still work in the account's
/// folders (bogusyogi/pulse#364), so the password database's home wins there. Any other `HOME`
/// (a test fixture included) is honoured. Same rule as pulse-core's `home::account_home`.
enum AccountHome {
    static var path: String {
        let env = ProcessInfo.processInfo.environment["HOME"] ?? ""
        guard let entry = getpwuid(getuid()), let dir = entry.pointee.pw_dir else {
            return env.isEmpty ? NSHomeDirectory() : env
        }
        let account = String(cString: dir)
        if env.isEmpty || env.hasPrefix(account + "/ClaudeProfiles/") || env == account + "/ClaudeProfiles" {
            return account
        }
        return env
    }

    /// The environment for a child process: `HOME` is the account home, and `CLAUDE_CONFIG_DIR` is
    /// removed (a child gets it only when Pulse sets it deliberately, as the usage probe does).
    static func childEnvironment(_ inherited: [String: String] = ProcessInfo.processInfo.environment) -> [String: String] {
        var environment = inherited
        environment["HOME"] = path
        environment.removeValue(forKey: "CLAUDE_CONFIG_DIR")
        return environment
    }
}
#endif
