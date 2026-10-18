#if os(macOS)
import Foundation
import Security

// macOS install pipeline: extract the downloaded disk image or zip, verify the app inside it
// (identity, version, Developer ID signature, Gatekeeper) and only then replace the installed app.
// There is no unverified path: ``MacAppInstaller`` cannot be built without a verifier.
// Decision D1 (issue 771): the swap keeps no backup, and a stale `<App>.app.previous` is deleted.

/// Runs a system tool to completion.
public protocol ToolRunning: Sendable {
    /// Standard output of `tool`. A non-zero exit throws ``UpdateError/toolFailed(tool:detail:)``.
    @discardableResult
    func run(_ tool: String, _ arguments: [String]) throws -> Data
}

public struct SystemToolRunner: ToolRunning {
    public init() {}

    @discardableResult
    public func run(_ tool: String, _ arguments: [String]) throws -> Data {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: tool)
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        let output = Pipe()
        let errors = Pipe()
        process.standardOutput = output
        process.standardError = errors
        do { try process.run() } catch { throw UpdateError.toolFailed(tool: tool, detail: "could not start") }
        let data = output.fileHandleForReading.readDataToEndOfFile()
        let text = errors.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            let detail = String(data: text.prefix(400), encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            throw UpdateError.toolFailed(tool: tool, detail: detail.isEmpty ? "exit \(process.terminationStatus)" : detail)
        }
        return data
    }
}

/// Decides whether a staged app is the one the publisher shipped.
public protocol UpdateVerifier: Sendable {
    /// Throws ``UpdateError/verificationFailed(_:)`` unless every check passes. There is no partial acceptance.
    func verify(app: URL, expectedVersion: String) throws
}

/// Apple-anchored Developer ID signature from one bundle identifier and one team, plus Gatekeeper's
/// notarization assessment. The two certificate OIDs pin a Developer ID Application certificate, not
/// any other kind of Apple-issued signature.
public struct DeveloperIDVerifier: UpdateVerifier {
    public let bundleIdentifier: String
    public let teamIdentifier: String
    /// Ask Gatekeeper (`spctl --assess`) to accept the app as notarized. Keep true for release builds.
    public let requireNotarization: Bool
    private let runner: any ToolRunning

    /// - Throws: ``UpdateError/verificationFailed(_:)`` when an identifier is not well formed, so a
    ///   malformed value can never reach the requirement language.
    public init(
        bundleIdentifier: String, teamIdentifier: String, requireNotarization: Bool = true,
        runner: any ToolRunning = SystemToolRunner(),
    ) throws {
        guard Self.isBundleIdentifier(bundleIdentifier) else { throw UpdateError.verificationFailed("bundle identifier is not well formed") }
        guard Self.isTeamIdentifier(teamIdentifier) else { throw UpdateError.verificationFailed("team identifier is not well formed") }
        self.bundleIdentifier = bundleIdentifier
        self.teamIdentifier = teamIdentifier
        self.requireNotarization = requireNotarization
        self.runner = runner
    }

    private static func isASCIIUpper(_ s: Unicode.Scalar) -> Bool { s.value >= 65 && s.value <= 90 }
    private static func isASCIILower(_ s: Unicode.Scalar) -> Bool { s.value >= 97 && s.value <= 122 }
    private static func isASCIIDigit(_ s: Unicode.Scalar) -> Bool { s.value >= 48 && s.value <= 57 }

    /// Dotted reverse-DNS characters only (`A-Za-z0-9.-`), starting with a letter or digit.
    static func isBundleIdentifier(_ text: String) -> Bool {
        let scalars = Array(text.unicodeScalars)
        guard let first = scalars.first, isASCIIUpper(first) || isASCIILower(first) || isASCIIDigit(first) else { return false }
        return scalars.allSatisfy { isASCIIUpper($0) || isASCIILower($0) || isASCIIDigit($0) || $0 == "." || $0 == "-" }
    }

    /// Apple team identifiers are ten upper-case letters or digits.
    static func isTeamIdentifier(_ text: String) -> Bool {
        let scalars = Array(text.unicodeScalars)
        return scalars.count == 10 && scalars.allSatisfy { isASCIIUpper($0) || isASCIIDigit($0) }
    }

    /// The code requirement the app must satisfy.
    public var requirementText: String {
        """
        anchor apple generic and identifier "\(bundleIdentifier)" \
        and certificate 1[field.1.2.840.113635.100.6.2.6] \
        and certificate leaf[field.1.2.840.113635.100.6.1.13] \
        and certificate leaf[subject.OU] = "\(teamIdentifier)"
        """
    }

    public func verify(app: URL, expectedVersion: String) throws {
        guard let bundle = Bundle(url: app), bundle.bundleIdentifier == bundleIdentifier else {
            throw UpdateError.verificationFailed("bundle identifier differs")
        }
        let shipped = bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
        guard let a = SemanticVersion(shipped, allowShort: true), let b = SemanticVersion(expectedVersion, allowShort: true), a == b else {
            throw UpdateError.verificationFailed("the app says it is version \(shipped), not \(expectedVersion)")
        }
        var code: SecStaticCode?
        guard SecStaticCodeCreateWithPath(app as CFURL, [], &code) == errSecSuccess, let code else {
            throw UpdateError.verificationFailed("the code signature could not be read")
        }
        var requirement: SecRequirement?
        guard SecRequirementCreateWithString(requirementText as CFString, [], &requirement) == errSecSuccess, let requirement else {
            throw UpdateError.verificationFailed("the signature requirement could not be built")
        }
        let flags = SecCSFlags(rawValue: UInt32(kSecCSStrictValidate) | UInt32(kSecCSCheckAllArchitectures) | UInt32(kSecCSCheckNestedCode))
        var error: Unmanaged<CFError>?
        guard SecStaticCodeCheckValidityWithErrors(code, flags, requirement, &error) == errSecSuccess else {
            let reason = error?.takeRetainedValue().localizedDescription ?? "the signature does not match"
            throw UpdateError.verificationFailed("signature check failed: \(reason)")
        }
        if requireNotarization {
            do { try runner.run("/usr/sbin/spctl", ["--assess", "--type", "execute", app.path]) } catch {
                throw UpdateError.verificationFailed("Gatekeeper did not accept the app as notarized")
            }
        }
    }
}

/// Stages, installs and relaunches a macOS app bundle in place.
public struct MacAppInstaller: UpdateInstalling {
    public let installURL: URL
    private let verifier: any UpdateVerifier
    private let runner: any ToolRunning

    /// - Parameters:
    ///   - installURL: the fixed install path (for example `/Applications/Pulse.app`). The app is
    ///     replaced there, never moved to a versioned or temporary folder.
    ///   - verifier: required; an artifact it rejects is never installed.
    public init(installURL: URL, verifier: any UpdateVerifier, runner: any ToolRunning = SystemToolRunner()) {
        self.installURL = installURL
        self.verifier = verifier
        self.runner = runner
    }

    private var appName: String { installURL.lastPathComponent }

    public func stage(artifact: URL, release: UpdateRelease, currentVersion: String, workDirectory: URL) async throws -> StagedBundle {
        guard let target = SemanticVersion(release.version, allowShort: true), let current = SemanticVersion(currentVersion, allowShort: true),
              target > current else { throw UpdateError.notNewer }
        let fm = FileManager.default
        let staged = workDirectory.appendingPathComponent("staged", isDirectory: true).appendingPathComponent(appName, isDirectory: true)
        try fm.createDirectory(at: staged.deletingLastPathComponent(), withIntermediateDirectories: true)
        switch artifact.pathExtension.lowercased() {
        case "dmg":
            try withMounted(artifact, in: workDirectory) { mounted in
                let source = mounted.appendingPathComponent(appName, isDirectory: true)
                guard fm.fileExists(atPath: source.path) else {
                    throw UpdateError.verificationFailed("the disk image does not contain \(appName)")
                }
                try runner.run("/usr/bin/ditto", [source.path, staged.path])
            }
        case "zip":
            let extracted = workDirectory.appendingPathComponent("extracted", isDirectory: true)
            try fm.createDirectory(at: extracted, withIntermediateDirectories: true)
            try runner.run("/usr/bin/ditto", ["-x", "-k", artifact.path, extracted.path])
            guard let source = findApp(named: appName, in: extracted) else {
                throw UpdateError.verificationFailed("the archive does not contain \(appName)")
            }
            try runner.run("/usr/bin/ditto", [source.path, staged.path])
        default:
            throw UpdateError.verificationFailed("unsupported artifact type .\(artifact.pathExtension)")
        }
        // The copy is verified, not the mount, so the bytes installed are the bytes checked.
        try verifier.verify(app: staged, expectedVersion: release.version)
        return StagedBundle(appURL: staged, version: release.version, workDirectory: workDirectory)
    }

    public func install(_ staged: StagedBundle) async throws {
        let fm = FileManager.default
        let parent = installURL.deletingLastPathComponent()
        // D1: no `.previous` copy is kept; a stale one from an older release is deleted.
        try? fm.removeItem(at: URL(fileURLWithPath: installURL.path + ".previous"))
        let incoming = parent.appendingPathComponent(".\(installURL.deletingPathExtension().lastPathComponent)-incoming-\(UUID().uuidString).app", isDirectory: true)
        do {
            try runner.run("/usr/bin/ditto", [staged.appURL.path, incoming.path])
            try verifier.verify(app: incoming, expectedVersion: staged.version)
            if fm.fileExists(atPath: installURL.path) {
                // Atomic swap of the new copy over the old one; with no backup name the old copy is deleted.
                _ = try fm.replaceItemAt(installURL, withItemAt: incoming, backupItemName: nil, options: [])
            } else {
                try fm.moveItem(at: incoming, to: installURL)
            }
        } catch let error as UpdateError {
            try? fm.removeItem(at: incoming)
            throw error
        } catch {
            try? fm.removeItem(at: incoming)
            throw UpdateError.installFailed("the new copy could not be put in place")
        }
    }

    public func relaunch(_ staged: StagedBundle) throws {
        let script = staged.workDirectory.appendingPathComponent("relaunch.sh")
        let body = """
            #!/bin/sh
            pid="$1"; app="$2"; work="$3"
            waited=0
            while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 300 ]; do
                sleep 0.2
                waited=$((waited + 1))
            done
            /usr/bin/open "$app"
            rm -rf "$work"
            """
        do { try body.write(to: script, atomically: true, encoding: .utf8) } catch {
            throw UpdateError.installFailed("the relaunch helper could not be written")
        }
        let helper = Process()
        helper.executableURL = URL(fileURLWithPath: "/bin/sh")
        helper.arguments = [script.path, String(ProcessInfo.processInfo.processIdentifier), installURL.path, staged.workDirectory.path]
        helper.standardInput = FileHandle.nullDevice
        helper.standardOutput = FileHandle.nullDevice
        helper.standardError = FileHandle.nullDevice
        do { try helper.run() } catch { throw UpdateError.installFailed("the relaunch helper could not start") }
    }

    // MARK: Helpers

    private func withMounted(_ image: URL, in directory: URL, _ body: (URL) throws -> Void) throws {
        let mountPoint = directory.appendingPathComponent("mount", isDirectory: true)
        try FileManager.default.createDirectory(at: mountPoint, withIntermediateDirectories: true)
        try runner.run("/usr/bin/hdiutil", ["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint", mountPoint.path, image.path])
        defer {
            if (try? runner.run("/usr/bin/hdiutil", ["detach", mountPoint.path])) == nil {
                _ = try? runner.run("/usr/bin/hdiutil", ["detach", "-force", mountPoint.path])
            }
        }
        try body(mountPoint)
    }

    private func findApp(named name: String, in directory: URL) -> URL? {
        let fm = FileManager.default
        let direct = directory.appendingPathComponent(name, isDirectory: true)
        if fm.fileExists(atPath: direct.path) { return direct }
        let children = (try? fm.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.isDirectoryKey])) ?? []
        for child in children {
            let nested = child.appendingPathComponent(name, isDirectory: true)
            if fm.fileExists(atPath: nested.path) { return nested }
        }
        return nil
    }
}
#endif
