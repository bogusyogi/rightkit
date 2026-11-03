import Foundation

#if os(macOS)
import Darwin

public enum RightKitLaunchAgentError: Error, Equatable, Sendable {
    /// The label is not 1 to 128 characters of letters, digits, `.`, `_`, `-`.
    case invalidLabel(String)
    case programNotAbsolute(String)
    /// An argument or environment entry holds a NUL or a line break.
    case invalidText
    case commandFailed(command: String, status: Int32, stderr: String)
}

/// How launchd restarts the agent after it exits (same cases as `rightkit-daemon` `RestartPolicy`).
public enum RightKitLaunchAgentRestart: Sendable, Equatable {
    case never
    /// After a non-zero exit (the default).
    case onFailure
    case always
}

public struct RightKitLaunchAgentSpec: Sendable, Equatable {
    /// The launchd label; also the plist file name.
    public var label: String
    /// The program: an absolute path.
    public var executable: String
    public var arguments: [String]
    public var environment: [String: String]
    public var restart: RightKitLaunchAgentRestart

    public init(
        label: String, executable: String, arguments: [String] = [], environment: [String: String] = [:],
        restart: RightKitLaunchAgentRestart = .onFailure
    ) {
        self.label = label
        self.executable = executable
        self.arguments = arguments
        self.environment = environment
        self.restart = restart
    }

    func validate() throws {
        try RightKitLaunchAgent.validate(label: label)
        guard executable.hasPrefix("/") else { throw RightKitLaunchAgentError.programNotAbsolute(executable) }
        func clean(_ s: String) -> Bool { !s.contains("\0") && !s.contains("\n") && !s.contains("\r") }
        guard clean(executable), arguments.allSatisfy(clean),
              environment.allSatisfy({ !$0.key.isEmpty && !$0.key.contains("=") && clean($0.key) && clean($0.value) })
        else { throw RightKitLaunchAgentError.invalidText }
    }
}

public enum RightKitLaunchAgentState: Sendable, Equatable {
    case notInstalled
    /// Installed (the plist exists) and not running, or not loaded.
    case stopped
    case running(pid: Int32?)
}

/// A launchd user agent, for the daemon-hosting apps (rightkit#405). It follows `rightkit-daemon`
/// `install.rs` `LaunchAgent` so a service installed by either side is managed by the other: the
/// plist is `~/Library/LaunchAgents/<label>.plist` (same text), loaded into `gui/<uid>` with
/// `launchctl bootstrap`, replaced with `bootout` then `bootstrap`, started with
/// `kickstart -k`, stopped with `kill SIGTERM`, and read with `launchctl print`. This is the
/// path-based mechanism; `RightKitLoginItem` (`SMAppService`) is the bundled-plist one for an app
/// that ships the agent inside its bundle. Per-user, no administrator rights.
public struct RightKitLaunchAgent: Sendable {
    public let agentsDirectory: URL
    public let uid: uid_t

    /// The current user's agents directory and uid.
    public init() {
        self.init(
            agentsDirectory: FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/LaunchAgents", isDirectory: true),
            uid: getuid())
    }

    public init(agentsDirectory: URL, uid: uid_t) {
        self.agentsDirectory = agentsDirectory
        self.uid = uid
    }

    public static func validate(label: String) throws {
        guard !label.isEmpty, label.utf8.count <= 128,
              label.utf8.allSatisfy({
                  ($0 >= 0x30 && $0 <= 0x39) || ($0 >= 0x41 && $0 <= 0x5a) || ($0 >= 0x61 && $0 <= 0x7a)
                      || $0 == 0x2e || $0 == 0x5f || $0 == 0x2d
              })
        else { throw RightKitLaunchAgentError.invalidLabel(label) }
    }

    public func plistURL(label: String) -> URL {
        agentsDirectory.appendingPathComponent("\(label).plist")
    }

    private var domain: String { "gui/\(uid)" }
    private func target(_ label: String) -> String { "gui/\(uid)/\(label)" }

    private static func xmlEscape(_ text: String) -> String {
        var out = ""
        for c in text {
            switch c {
            case "&": out += "&amp;"
            case "<": out += "&lt;"
            case ">": out += "&gt;"
            case "\"": out += "&quot;"
            case "'": out += "&apos;"
            default: out.append(c)
            }
        }
        return out
    }

    /// The plist text for `spec` (identical layout to the daemon's; environment keys sorted).
    public static func plist(for spec: RightKitLaunchAgentSpec) -> String {
        var out = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
        out += "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n"
        out += "<plist version=\"1.0\">\n<dict>\n"
        out += "  <key>Label</key>\n  <string>\(xmlEscape(spec.label))</string>\n"
        out += "  <key>ProgramArguments</key>\n  <array>\n    <string>\(xmlEscape(spec.executable))</string>\n"
        for argument in spec.arguments { out += "    <string>\(xmlEscape(argument))</string>\n" }
        out += "  </array>\n  <key>RunAtLoad</key>\n  <true/>\n"
        if !spec.environment.isEmpty {
            out += "  <key>EnvironmentVariables</key>\n  <dict>\n"
            for key in spec.environment.keys.sorted() {
                out += "    <key>\(xmlEscape(key))</key>\n    <string>\(xmlEscape(spec.environment[key] ?? ""))</string>\n"
            }
            out += "  </dict>\n"
        }
        switch spec.restart {
        case .never: break
        case .always: out += "  <key>KeepAlive</key>\n  <true/>\n"
        case .onFailure:
            out += "  <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n"
        }
        out += "</dict>\n</plist>\n"
        return out
    }

    /// Write the plist (atomically, owner-only) and load it. A loaded copy is replaced; with
    /// `RunAtLoad` it starts now and at each login.
    public func install(_ spec: RightKitLaunchAgentSpec) throws {
        try spec.validate()
        try FileManager.default.createDirectory(at: agentsDirectory, withIntermediateDirectories: true)
        let url = plistURL(label: spec.label)
        try Data(Self.plist(for: spec).utf8).write(to: url, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
        // bootout fails when nothing is loaded, which is fine.
        _ = try? Self.launchctl(["bootout", target(spec.label)])
        try Self.launchctlChecked(["bootstrap", domain, url.path])
    }

    /// Start (or restart) now.
    public func start(label: String) throws {
        try Self.validate(label: label)
        try Self.launchctlChecked(["kickstart", "-k", target(label)])
    }

    /// Ask the process to end (SIGTERM). The restart policy may start it again. A service that is
    /// not running is already stopped.
    public func stop(label: String) throws {
        try Self.validate(label: label)
        let out = try Self.launchctl(["kill", "SIGTERM", target(label)])
        if out.status == 0 { return }
        if case .running = try status(label: label) {
            throw RightKitLaunchAgentError.commandFailed(command: "launchctl kill", status: out.status, stderr: out.stderr)
        }
    }

    public func status(label: String) throws -> RightKitLaunchAgentState {
        try Self.validate(label: label)
        let out = try Self.launchctl(["print", target(label)])
        guard out.status == 0 else {
            return FileManager.default.fileExists(atPath: plistURL(label: label).path) ? .stopped : .notInstalled
        }
        for line in out.stdout.split(separator: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("pid = "), let pid = Int32(trimmed.dropFirst("pid = ".count).trimmingCharacters(in: .whitespaces)) {
                return .running(pid: pid)
            }
        }
        return .stopped
    }

    /// Unload and delete the plist. An agent that is not installed is not an error.
    public func uninstall(label: String) throws {
        try Self.validate(label: label)
        _ = try? Self.launchctl(["bootout", target(label)])
        do {
            try FileManager.default.removeItem(at: plistURL(label: label))
        } catch let error as NSError where error.domain == NSCocoaErrorDomain && error.code == NSFileNoSuchFileError {
            return
        }
    }

    // MARK: launchctl

    private struct Output {
        var status: Int32
        var stdout: String
        var stderr: String
    }

    private static func launchctl(_ arguments: [String]) throws -> Output {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        process.arguments = arguments
        // One pipe for both streams, read to the end before waiting, so a full pipe never stalls launchctl.
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        try process.run()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let text = String(decoding: data, as: UTF8.self)
        return Output(status: process.terminationStatus, stdout: text, stderr: text)
    }

    private static func launchctlChecked(_ arguments: [String]) throws {
        let out = try launchctl(arguments)
        guard out.status == 0 else {
            throw RightKitLaunchAgentError.commandFailed(
                command: "launchctl \(arguments.first ?? "")", status: out.status,
                stderr: out.stderr.trimmingCharacters(in: .whitespacesAndNewlines))
        }
    }
}
#endif
