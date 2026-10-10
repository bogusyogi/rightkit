import CryptoKit
import Foundation
import zlib

/// What an artifact must be before it is allowed to become the current one.
public struct ArtifactDescriptor: Sendable, Equatable {
    /// The single directory the archive is allowed to contain. Anything outside it is an escape.
    public let topLevelDirectory: String
    /// Expected SHA-256 of the archive. Nil is refused unless `InstallOptions.allowUnverified` is set.
    public let expectedSHA256: String?
    /// Ceiling on the inflated tar, so a small archive cannot expand until the disk is gone.
    public let maxInflatedBytes: UInt64
    /// Recorded in the install manifest. The installer never parses it.
    public let version: String?

    public init(
        topLevelDirectory: String,
        expectedSHA256: String?,
        maxInflatedBytes: UInt64 = 4 << 30,
        version: String? = nil,
    ) {
        self.topLevelDirectory = topLevelDirectory
        self.expectedSHA256 = expectedSHA256
        self.maxInflatedBytes = maxInflatedBytes
        self.version = version
    }
}

public enum ArtifactError: Error, Equatable, Sendable {
    case checksumMismatch(expected: String, actual: String)
    case archiveReadFailed
    case truncatedArchive
    case emptyArchive
    case archiveEscapesDestination(String)
    case unsupportedArchiveMember(String)
    case invalidTopLevelDirectory(String)
    case inflatedArchiveTooLarge
    /// No expected digest and `InstallOptions.allowUnverified` is not set. Nothing was touched.
    case missingExpectedDigest
    case missingRequiredFile(String)
    case invalidRequiredFilePath(String)
    /// The post-activation hook threw. The previous install was restored; the error text is the hook's.
    case postActivationValidationFailed(String)
    /// Another install of the same top-level directory holds the install lock.
    case installInProgress
    case manifestUnreadable(String)
    /// The activation marker exists but cannot be decoded, so the interrupted state is unknown.
    case recoveryMarkerUnreadable
}

/// Options for `ArtifactInstaller.install`. The defaults are the strict ones.
public struct InstallOptions: Sendable {
    /// Permit installing an archive with no expected digest. Explicit opt-in; never the default.
    public var allowUnverified: Bool
    /// Paths relative to the top-level directory that must exist as files before activation.
    public var requiredFiles: [String]
    /// Runs on the activated directory. The previous install is kept aside until this returns.
    /// Throwing rolls back to the previous install.
    public var postActivationValidation: (@Sendable (URL) async throws -> Void)?

    public init(
        allowUnverified: Bool = false,
        requiredFiles: [String] = [],
        postActivationValidation: (@Sendable (URL) async throws -> Void)? = nil,
    ) {
        self.allowUnverified = allowUnverified
        self.requiredFiles = requiredFiles
        self.postActivationValidation = postActivationValidation
    }
}

/// Written inside the installed directory, so it moves into place atomically with the files it describes.
public struct ArtifactManifest: Codable, Equatable, Sendable {
    public static let fileName = ".rightkit-manifest.json"

    public let topLevelDirectory: String
    public let version: String?
    /// SHA-256 of the archive when it was checked against an expected digest. Nil when unverified.
    public let archiveSHA256: String?
    /// Every regular file under the top-level directory, excluding the manifest itself, sorted by path.
    public let files: [ManifestFile]

    public init(topLevelDirectory: String, version: String?, archiveSHA256: String?, files: [ManifestFile]) {
        self.topLevelDirectory = topLevelDirectory
        self.version = version
        self.archiveSHA256 = archiveSHA256
        self.files = files
    }
}

public struct ManifestFile: Codable, Equatable, Sendable {
    public let path: String
    public let sha256: String
    public let size: UInt64

    public init(path: String, sha256: String, size: UInt64) {
        self.path = path
        self.sha256 = sha256
        self.size = size
    }
}

public enum ArtifactRecoveryOutcome: Equatable, Sendable {
    /// No interrupted activation was found.
    case noInterruptedInstall
    /// The interrupted activation was undone. The previous install is active again, or none is.
    case rolledBack
    /// The interrupted activation was finished: the new install is active and the previous one discarded.
    case completed
}

/// Verify → stage → activate → validate → finalize, with the previous install kept aside until the new
/// one is accepted. A failed install can never delete or expose a partially installed current artifact,
/// and a crash at any point is repaired by `recoverInterruptedInstall` on the next launch.
///
/// On-disk layout under the destination root, for top-level directory `T`:
/// - `T/` the active install, holding `.rightkit-manifest.json`
/// - `.aside-T/` the previous install, present only while an activation is unresolved
/// - `.pending-T.json` the activation marker: phase `activating` or `validating`, and whether there was a previous install
/// - `.staging-T-*/` and `.trash-T-*/` scratch, removed on success and swept on recovery
/// - `.install-T.lock` the install lock, held for the duration of an install or recovery
public enum ArtifactInstaller {
    /// Streaming digest: artifacts are hundreds of MB, and reading one into memory to hash it is how
    /// a model install OOMs a phone.
    public static func sha256(of fileURL: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: fileURL)
        defer { try? handle.close() }
        var hasher = SHA256()
        while true {
            let chunk = handle.readData(ofLength: 1 << 20)
            if chunk.isEmpty { break }
            hasher.update(data: chunk)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    /// Checks a digest when one is given. A nil digest passes here; `install` is the fail-closed entry point.
    public static func verifyChecksum(_ fileURL: URL, expected: String?) throws {
        guard let expected else { return }
        let actual = try sha256(of: fileURL)
        guard actual.caseInsensitiveCompare(expected) == .orderedSame else {
            throw ArtifactError.checksumMismatch(expected: expected, actual: actual)
        }
    }

    /// Install `archiveURL` (a .tar.gz holding exactly one top-level directory) as the current artifact
    /// under `destinationRoot`.
    ///
    /// A nil `descriptor.expectedSHA256` throws `missingExpectedDigest` before the archive is read, unless
    /// `options.allowUnverified` is set. An interrupted earlier install is recovered first, with
    /// `options.postActivationValidation` as the validator for an activation that had not finished.
    @discardableResult
    public static func install(
        archive archiveURL: URL,
        descriptor: ArtifactDescriptor,
        destinationRoot: URL,
        options: InstallOptions = InstallOptions(),
    ) async throws -> URL {
        try await performInstall(
            archive: archiveURL,
            descriptor: descriptor,
            destinationRoot: destinationRoot,
            options: options,
            fault: nil,
        )
    }

    /// Resolves an activation that a crash or kill interrupted. Call it on launch before trusting the
    /// installed directory. `install` also calls it. With no validator, an activation that had reached
    /// validation is rolled back, never trusted unvalidated.
    @discardableResult
    public static func recoverInterruptedInstall(
        destinationRoot: URL,
        topLevelDirectory: String,
        postActivationValidation: (@Sendable (URL) async throws -> Void)? = nil,
    ) async throws -> ArtifactRecoveryOutcome {
        try validateTopLevelName(topLevelDirectory)
        try FileManager.default.createDirectory(at: destinationRoot, withIntermediateDirectories: true)
        let lock = try acquireInstallLock(destinationRoot: destinationRoot, name: topLevelDirectory)
        defer { releaseInstallLock(lock) }
        return try await recoverLocked(
            destinationRoot: destinationRoot,
            name: topLevelDirectory,
            validate: postActivationValidation,
        )
    }

    /// The install manifest of the active install, or nil when the install has none.
    public static func readManifest(destinationRoot: URL, topLevelDirectory: String) throws -> ArtifactManifest? {
        try validateTopLevelName(topLevelDirectory)
        let installed = destinationRoot.appendingPathComponent(topLevelDirectory, isDirectory: true)
        return try readManifest(installedAt: installed)
    }

    // MARK: - Install

    /// Internal so tests can inject a fault at an activation step and snapshot the disk as a crash would leave it.
    static func performInstall(
        archive archiveURL: URL,
        descriptor: ArtifactDescriptor,
        destinationRoot: URL,
        options: InstallOptions,
        fault: FaultInjector?,
    ) async throws -> URL {
        let name = descriptor.topLevelDirectory
        try validateTopLevelName(name)

        // Fail closed first: without a digest or an explicit opt-in the archive is not read and the
        // destination is not touched, not even created.
        let archiveSHA256: String?
        if let expected = descriptor.expectedSHA256 {
            let actual = try sha256(of: archiveURL)
            guard actual.caseInsensitiveCompare(expected) == .orderedSame else {
                throw ArtifactError.checksumMismatch(expected: expected, actual: actual)
            }
            archiveSHA256 = actual.lowercased()
        } else {
            guard options.allowUnverified else { throw ArtifactError.missingExpectedDigest }
            archiveSHA256 = nil
        }

        let fileManager = FileManager.default
        try fileManager.createDirectory(at: destinationRoot, withIntermediateDirectories: true)

        let lock = try acquireInstallLock(destinationRoot: destinationRoot, name: name)
        defer { releaseInstallLock(lock) }

        _ = try await recoverLocked(
            destinationRoot: destinationRoot,
            name: name,
            validate: options.postActivationValidation,
        )

        let staging = destinationRoot.appendingPathComponent(".staging-\(name)-\(UUID().uuidString)", isDirectory: true)
        try fileManager.createDirectory(at: staging, withIntermediateDirectories: true)
        defer { try? fileManager.removeItem(at: staging) }

        try extractTarGz(archiveURL, into: staging, descriptor: descriptor)

        let staged = staging.appendingPathComponent(name, isDirectory: true)
        guard fileManager.fileExists(atPath: staged.path) else { throw ArtifactError.emptyArchive }

        try requireFiles(options.requiredFiles, in: staged)
        let manifest = ArtifactManifest(
            topLevelDirectory: name,
            version: descriptor.version,
            archiveSHA256: archiveSHA256,
            files: try manifestFiles(under: staged),
        )
        try writeManifest(manifest, into: staged)

        let dest = destinationRoot.appendingPathComponent(name, isDirectory: true)
        let marker = markerURL(destinationRoot: destinationRoot, name: name)
        let hadPrevious = fileManager.fileExists(atPath: dest.path)

        // Activation. Each state change is preceded by a marker write, so a crash at any point leaves
        // a marker that `recoverLocked` can act on.
        try writeMarker(ActivationMarker(phase: .activating, hadPrevious: hadPrevious), to: marker)
        do {
            if hadPrevious { try fileManager.moveItem(at: dest, to: asideURL(destinationRoot, name)) }
            try fault?(.afterAsideMove, destinationRoot)
            try fileManager.moveItem(at: staged, to: dest)
            try fault?(.afterActivation, destinationRoot)
            try writeMarker(ActivationMarker(phase: .validating, hadPrevious: hadPrevious), to: marker)
            try fault?(.beforeValidation, destinationRoot)
        } catch {
            // Restore first. The marker stays if restoring fails, so recovery finishes the job later.
            try rollBack(destinationRoot: destinationRoot, name: name, dest: dest, hadPrevious: hadPrevious)
            try? fileManager.removeItem(at: marker)
            throw error
        }

        if let validate = options.postActivationValidation {
            do {
                try await validate(dest)
            } catch {
                try rollBack(destinationRoot: destinationRoot, name: name, dest: dest, hadPrevious: hadPrevious)
                try? fileManager.removeItem(at: marker)
                throw ArtifactError.postActivationValidationFailed(String(describing: error))
            }
        }

        try finalize(destinationRoot: destinationRoot, name: name)
        excludeFromBackup(dest)
        return dest
    }

    // MARK: - Recovery

    /// The shared recovery state machine. The caller holds the install lock.
    private static func recoverLocked(
        destinationRoot: URL,
        name: String,
        validate: (@Sendable (URL) async throws -> Void)?,
    ) async throws -> ArtifactRecoveryOutcome {
        let fileManager = FileManager.default
        let dest = destinationRoot.appendingPathComponent(name, isDirectory: true)
        let aside = asideURL(destinationRoot, name)
        let marker = markerURL(destinationRoot: destinationRoot, name: name)

        // Trash and staging are never trusted; they are sweepable on any run.
        try sweepScratch(destinationRoot: destinationRoot, name: name)

        guard fileManager.fileExists(atPath: marker.path) else { return .noInterruptedInstall }
        let state = try readMarker(marker)

        switch state.phase {
        case .activating:
            // The new install was never accepted: restore the previous one, or leave none.
            try rollBack(destinationRoot: destinationRoot, name: name, dest: dest, hadPrevious: state.hadPrevious)
            try? fileManager.removeItem(at: marker)
            return .rolledBack

        case .validating:
            // The aside is removed only after validation accepted the new install. Aside gone with a
            // previous install means the activation was accepted and only the marker removal was missed.
            if state.hadPrevious && !fileManager.fileExists(atPath: aside.path) {
                try? fileManager.removeItem(at: marker)
                return .completed
            }
            guard let validate, fileManager.fileExists(atPath: dest.path), try verifyInstalled(at: dest) else {
                try rollBack(destinationRoot: destinationRoot, name: name, dest: dest, hadPrevious: state.hadPrevious)
                try? fileManager.removeItem(at: marker)
                return .rolledBack
            }
            do {
                try await validate(dest)
            } catch {
                try rollBack(destinationRoot: destinationRoot, name: name, dest: dest, hadPrevious: state.hadPrevious)
                try? fileManager.removeItem(at: marker)
                return .rolledBack
            }
            try finalize(destinationRoot: destinationRoot, name: name)
            excludeFromBackup(dest)
            return .completed
        }
    }

    /// Puts the previous install back when there is one. Otherwise removes an unaccepted new install.
    /// With no previous install and nothing aside, a live `dest` is the old one and is left alone.
    private static func rollBack(destinationRoot: URL, name: String, dest: URL, hadPrevious: Bool) throws {
        let fileManager = FileManager.default
        let aside = asideURL(destinationRoot, name)
        if fileManager.fileExists(atPath: aside.path) {
            if fileManager.fileExists(atPath: dest.path) { try discard(dest, destinationRoot: destinationRoot, name: name) }
            try fileManager.moveItem(at: aside, to: dest)
        } else if !hadPrevious, fileManager.fileExists(atPath: dest.path) {
            try discard(dest, destinationRoot: destinationRoot, name: name)
        }
    }

    /// Drops the aside (the previous install) and the marker, in an order that a crash cannot turn
    /// into a restore of a stale install: the aside is renamed out of the way first, and the marker is
    /// removed only once no previous install remains in place.
    private static func finalize(destinationRoot: URL, name: String) throws {
        let aside = asideURL(destinationRoot, name)
        if FileManager.default.fileExists(atPath: aside.path) {
            try discard(aside, destinationRoot: destinationRoot, name: name)
        }
        try? FileManager.default.removeItem(at: markerURL(destinationRoot: destinationRoot, name: name))
    }

    /// Renames `url` into the trash first, so a crash mid-delete leaves an item that is swept, not half-deleted.
    private static func discard(_ url: URL, destinationRoot: URL, name: String) throws {
        let fileManager = FileManager.default
        let trash = destinationRoot.appendingPathComponent(".trash-\(name)-\(UUID().uuidString)", isDirectory: true)
        do {
            try fileManager.moveItem(at: url, to: trash)
        } catch {
            try fileManager.removeItem(at: url)
            return
        }
        try? fileManager.removeItem(at: trash)
    }

    private static func sweepScratch(destinationRoot: URL, name: String) throws {
        let fileManager = FileManager.default
        let prefixes = [".trash-\(name)-", ".staging-\(name)-"]
        let entries = try fileManager.contentsOfDirectory(at: destinationRoot, includingPropertiesForKeys: nil)
        for entry in entries where prefixes.contains(where: { entry.lastPathComponent.hasPrefix($0) }) {
            try? fileManager.removeItem(at: entry)
        }
    }

    /// Every manifest file is present with the recorded size and digest.
    private static func verifyInstalled(at dest: URL) throws -> Bool {
        guard let manifest = try readManifest(installedAt: dest) else { return false }
        for file in manifest.files {
            let url = dest.appendingPathComponent(file.path)
            var isDirectory: ObjCBool = false
            guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory), !isDirectory.boolValue else {
                return false
            }
            let size = (try FileManager.default.attributesOfItem(atPath: url.path)[.size] as? NSNumber)?.uint64Value
            guard size == file.size, try sha256(of: url).caseInsensitiveCompare(file.sha256) == .orderedSame else {
                return false
            }
        }
        return true
    }

    // MARK: - Manifest

    private static func readManifest(installedAt dest: URL) throws -> ArtifactManifest? {
        let url = dest.appendingPathComponent(ArtifactManifest.fileName)
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        do {
            return try JSONDecoder().decode(ArtifactManifest.self, from: Data(contentsOf: url))
        } catch {
            throw ArtifactError.manifestUnreadable(url.lastPathComponent)
        }
    }

    private static func writeManifest(_ manifest: ArtifactManifest, into staged: URL) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .prettyPrinted]
        try encoder.encode(manifest).write(to: staged.appendingPathComponent(ArtifactManifest.fileName), options: .atomic)
    }

    /// Synchronous on purpose: `FileManager`'s enumerator cannot be iterated from an async context.
    private static func manifestFiles(under root: URL) throws -> [ManifestFile] {
        guard let enumerator = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: [.isRegularFileKey, .fileSizeKey],
            options: [],
        ) else { throw ArtifactError.archiveReadFailed }

        let prefix = root.standardizedFileURL.path + "/"
        var files: [ManifestFile] = []
        for case let url as URL in enumerator {
            let values = try url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
            guard values.isRegularFile == true else { continue }
            let path = url.standardizedFileURL.path
            guard path.hasPrefix(prefix) else { throw ArtifactError.archiveEscapesDestination(path) }
            let relative = String(path.dropFirst(prefix.count))
            if relative == ArtifactManifest.fileName { continue }
            files.append(ManifestFile(
                path: relative,
                sha256: try sha256(of: url).lowercased(),
                size: UInt64(values.fileSize ?? 0),
            ))
        }
        return files.sorted { $0.path < $1.path }
    }

    /// Synchronous for the same reason as `manifestFiles`.
    private static func requireFiles(_ paths: [String], in staged: URL) throws {
        for path in paths {
            let parts = path.split(separator: "/", omittingEmptySubsequences: false)
            guard !path.isEmpty, !path.hasPrefix("/"),
                  !parts.contains(where: { $0.isEmpty || $0 == "." || $0 == ".." })
            else { throw ArtifactError.invalidRequiredFilePath(path) }

            var isDirectory: ObjCBool = false
            guard FileManager.default.fileExists(atPath: staged.appendingPathComponent(path).path, isDirectory: &isDirectory),
                  !isDirectory.boolValue
            else { throw ArtifactError.missingRequiredFile(path) }
        }
    }

    // MARK: - Marker and lock

    private enum ActivationPhase: String, Codable, Sendable {
        case activating
        case validating
    }

    private struct ActivationMarker: Codable, Sendable {
        let phase: ActivationPhase
        let hadPrevious: Bool
    }

    /// Crash points for tests. The injector runs after the step's on-disk effect, before the next step.
    enum ActivationFault: Sendable {
        case afterAsideMove
        case afterActivation
        case beforeValidation
    }

    typealias FaultInjector = @Sendable (ActivationFault, URL) throws -> Void

    private static func markerURL(destinationRoot: URL, name: String) -> URL {
        destinationRoot.appendingPathComponent(".pending-\(name).json")
    }

    private static func asideURL(_ destinationRoot: URL, _ name: String) -> URL {
        destinationRoot.appendingPathComponent(".aside-\(name)", isDirectory: true)
    }

    private static func writeMarker(_ marker: ActivationMarker, to url: URL) throws {
        try JSONEncoder().encode(marker).write(to: url, options: .atomic)
    }

    private static func readMarker(_ url: URL) throws -> ActivationMarker {
        guard let data = try? Data(contentsOf: url),
              let marker = try? JSONDecoder().decode(ActivationMarker.self, from: data)
        else { throw ArtifactError.recoveryMarkerUnreadable }
        return marker
    }

    /// Non-blocking: a second install of the same directory fails at once instead of queueing behind a
    /// long download or validation.
    private static func acquireInstallLock(destinationRoot: URL, name: String) throws -> Int32 {
        let fd = open(destinationRoot.appendingPathComponent(".install-\(name).lock").path, O_RDWR | O_CREAT, 0o600)
        guard fd >= 0 else { throw ArtifactError.installInProgress }
        guard flock(fd, LOCK_EX | LOCK_NB) == 0 else {
            close(fd)
            throw ArtifactError.installInProgress
        }
        return fd
    }

    private static func releaseInstallLock(_ fd: Int32) {
        flock(fd, LOCK_UN)
        close(fd)
    }

    // MARK: - Archive handling

    private static func validateTopLevelName(_ name: String) throws {
        guard !name.isEmpty, !name.contains("/"), name != ".", name != ".." else {
            throw ArtifactError.invalidTopLevelDirectory(name)
        }
    }

    private static func extractTarGz(_ archiveURL: URL, into staging: URL, descriptor: ArtifactDescriptor) throws {
        let tarURL = staging.appendingPathComponent("archive.tar")
        try inflateGzip(archiveURL, to: tarURL, limit: descriptor.maxInflatedBytes)
        defer { try? FileManager.default.removeItem(at: tarURL) }
        try unpackTar(tarURL, into: staging, expectedTopLevel: descriptor.topLevelDirectory)
    }

    private static func inflateGzip(_ sourceURL: URL, to destURL: URL, limit: UInt64) throws {
        FileManager.default.createFile(atPath: destURL.path, contents: nil)
        let output = try FileHandle(forWritingTo: destURL)
        defer { try? output.close() }

        guard let gz = sourceURL.path.withCString({ gzopen($0, "rb") }) else {
            throw ArtifactError.archiveReadFailed
        }
        defer { gzclose(gz) }

        var buffer = [UInt8](repeating: 0, count: 1 << 20)
        var total: UInt64 = 0
        while true {
            let count = gzread(gz, &buffer, UInt32(buffer.count))
            if count < 0 { throw ArtifactError.archiveReadFailed }
            if count == 0 { break }
            total += UInt64(count)
            guard total <= limit else { throw ArtifactError.inflatedArchiveTooLarge }
            try output.write(contentsOf: Data(buffer.prefix(Int(count))))
        }
    }

    private static func unpackTar(_ tarURL: URL, into staging: URL, expectedTopLevel: String) throws {
        let input = try FileHandle(forReadingFrom: tarURL)
        defer { try? input.close() }
        var sawMember = false

        while true {
            let headerData = input.readData(ofLength: 512)
            if headerData.isEmpty { break }
            guard headerData.count == 512 else { throw ArtifactError.truncatedArchive }
            let header = [UInt8](headerData)
            if header.allSatisfy({ $0 == 0 }) { break }

            let memberPath = tarField(header, offset: 0, length: 100)
            let prefix = tarField(header, offset: 345, length: 155)
            let fullPath = prefix.isEmpty ? memberPath : "\(prefix)/\(memberPath)"
            let size = try tarSize(header)
            let type = header[156]

            // pax/global extended headers carry metadata, not content.
            if type == UInt8(ascii: "x") || type == UInt8(ascii: "g") {
                try skipPayload(input, size: size)
                continue
            }

            guard let relativePath = try checkedMemberPath(fullPath, expectedTopLevel: expectedTopLevel) else {
                try skipPayload(input, size: size)
                continue
            }
            let out = staging.appendingPathComponent(relativePath, isDirectory: type == UInt8(ascii: "5"))

            if type == 0 || type == UInt8(ascii: "0") {
                try FileManager.default.createDirectory(
                    at: out.deletingLastPathComponent(),
                    withIntermediateDirectories: true,
                )
                FileManager.default.createFile(atPath: out.path, contents: nil)
                let output = try FileHandle(forWritingTo: out)
                do {
                    try copyPayload(input, size: size, to: output)
                    try output.close()
                } catch {
                    try? output.close()
                    throw error
                }
            } else if type == UInt8(ascii: "5") {
                try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
                try skipPayload(input, size: size)
            } else {
                // Symlinks ('2'), hardlinks ('1'), devices, fifos: an artifact is files and folders.
                // Failing closed here is what stops a member pointing at somewhere it should not.
                throw ArtifactError.unsupportedArchiveMember(fullPath)
            }
            sawMember = true
        }

        if !sawMember { throw ArtifactError.emptyArchive }
    }

    /// Returns nil for members that are ignorable macOS metadata; throws for anything that tries to
    /// leave `expectedTopLevel` — traversal, absolute paths, or a different root.
    private static func checkedMemberPath(_ path: String, expectedTopLevel: String) throws -> String? {
        let normalized = path.hasSuffix("/") ? String(path.dropLast()) : path
        let parts = normalized.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        if parts.contains(where: { $0 == "__MACOSX" || $0.hasPrefix("._") }) { return nil }
        guard parts.first == expectedTopLevel else {
            throw ArtifactError.archiveEscapesDestination(path)
        }
        for part in parts where part.isEmpty || part == "." || part == ".." {
            throw ArtifactError.archiveEscapesDestination(path)
        }
        return parts.joined(separator: "/")
    }

    private static func tarField(_ header: [UInt8], offset: Int, length: Int) -> String {
        let bytes = header[offset..<(offset + length)].prefix { $0 != 0 }
        return String(decoding: bytes, as: UTF8.self).trimmingCharacters(in: .whitespaces)
    }

    private static func tarSize(_ header: [UInt8]) throws -> UInt64 {
        let field = tarField(header, offset: 124, length: 12)
        guard let size = UInt64(field, radix: 8) else { throw ArtifactError.truncatedArchive }
        return size
    }

    private static func copyPayload(_ input: FileHandle, size: UInt64, to output: FileHandle) throws {
        var remaining = size
        while remaining > 0 {
            let chunkSize = min(Int(remaining), 1 << 20)
            let chunk = input.readData(ofLength: chunkSize)
            guard chunk.count == chunkSize else { throw ArtifactError.truncatedArchive }
            try output.write(contentsOf: chunk)
            remaining -= UInt64(chunk.count)
        }
        try skipPadding(input, size: size)
    }

    private static func skipPayload(_ input: FileHandle, size: UInt64) throws {
        try skipBytes(input, count: size)
        try skipPadding(input, size: size)
    }

    private static func skipPadding(_ input: FileHandle, size: UInt64) throws {
        try skipBytes(input, count: (512 - (size % 512)) % 512)
    }

    private static func skipBytes(_ input: FileHandle, count: UInt64) throws {
        var remaining = count
        while remaining > 0 {
            let chunkSize = min(Int(remaining), 1 << 20)
            let chunk = input.readData(ofLength: chunkSize)
            guard chunk.count == chunkSize else { throw ArtifactError.truncatedArchive }
            remaining -= UInt64(chunk.count)
        }
    }

    private static func excludeFromBackup(_ url: URL) {
        var url = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)
    }
}
