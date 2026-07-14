import CryptoKit
import Foundation
import zlib

/// What an artifact must be before it is allowed to become the current one.
public struct ArtifactDescriptor: Sendable, Equatable {
    /// The single directory the archive is allowed to contain. Anything outside it is an escape.
    public let topLevelDirectory: String
    public let expectedSHA256: String?
    /// Ceiling on the inflated tar, so a small archive cannot expand until the disk is gone.
    public let maxInflatedBytes: UInt64

    public init(topLevelDirectory: String, expectedSHA256: String?, maxInflatedBytes: UInt64 = 4 << 30) {
        self.topLevelDirectory = topLevelDirectory
        self.expectedSHA256 = expectedSHA256
        self.maxInflatedBytes = maxInflatedBytes
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
}

/// Verify → stage → activate → (on any failure) roll back. The invariant that matters: a failed
/// install can never delete or expose a partially installed current artifact.
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

    public static func verifyChecksum(_ fileURL: URL, expected: String?) throws {
        guard let expected else { return }
        let actual = try sha256(of: fileURL)
        guard actual.caseInsensitiveCompare(expected) == .orderedSame else {
            throw ArtifactError.checksumMismatch(expected: expected, actual: actual)
        }
    }

    /// Install `archiveURL` (a .tar.gz holding exactly one top-level directory) as the current
    /// artifact under `destinationRoot`. The previous install is kept until the new one is in place,
    /// and restored if anything fails.
    @discardableResult
    public static func install(
        archive archiveURL: URL,
        descriptor: ArtifactDescriptor,
        destinationRoot: URL,
    ) throws -> URL {
        try validateTopLevelName(descriptor.topLevelDirectory)
        try verifyChecksum(archiveURL, expected: descriptor.expectedSHA256)

        let fileManager = FileManager.default
        let staging = destinationRoot.appendingPathComponent(".staging-\(UUID().uuidString)", isDirectory: true)
        let destination = destinationRoot.appendingPathComponent(descriptor.topLevelDirectory, isDirectory: true)
        let backup = destinationRoot.appendingPathComponent(".rollback-\(UUID().uuidString)", isDirectory: true)

        try fileManager.createDirectory(at: staging, withIntermediateDirectories: true)
        defer { try? fileManager.removeItem(at: staging) }

        do {
            try extractTarGz(archiveURL, into: staging, descriptor: descriptor)
        } catch {
            throw error  // nothing has touched the current install yet
        }

        let staged = staging.appendingPathComponent(descriptor.topLevelDirectory, isDirectory: true)
        guard fileManager.fileExists(atPath: staged.path) else { throw ArtifactError.emptyArchive }

        let hadPrevious = fileManager.fileExists(atPath: destination.path)
        if hadPrevious { try fileManager.moveItem(at: destination, to: backup) }

        do {
            try fileManager.moveItem(at: staged, to: destination)
        } catch {
            // Activation failed: put the old artifact back rather than leaving the app with nothing.
            if hadPrevious { try? fileManager.moveItem(at: backup, to: destination) }
            throw error
        }

        if hadPrevious { try? fileManager.removeItem(at: backup) }
        excludeFromBackup(destination)
        return destination
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
