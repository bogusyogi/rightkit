import Foundation
import UniformTypeIdentifiers

/// What a share extension accepts, in priority order within one attachment.
public struct ShareAcceptedTypes: Sendable {
    public var text: Bool
    public var url: Bool
    /// File-backed types in priority order, with the extension used when the provider supplies none.
    public var files: [(type: UTType, fileExtension: String)]

    public init(text: Bool = true, url: Bool = true, files: [(type: UTType, fileExtension: String)]) {
        self.text = text
        self.url = url
        self.files = files
    }

    public static let documentsAndMedia = ShareAcceptedTypes(files: [
        (.audio, "audio"), (.movie, "movie"), (.pdf, "pdf"), (.image, "image"),
    ])
}

/// Fast, memory-safe extraction of a share-sheet item into a `PendingSharePayload`.
///
/// Files are always streamed to disk through `loadFileRepresentation` (never read into `Data`), are
/// size-capped, and a partially copied file is removed so the app never consumes a truncated item.
/// Isolated to the main actor so non-Sendable `NSItemProvider` handles never cross isolation.
@MainActor
public struct ShareItemExtractor {
    private let configuration: ShareIntakeConfiguration
    private let accepted: ShareAcceptedTypes

    public init(configuration: ShareIntakeConfiguration, accepted: ShareAcceptedTypes) {
        self.configuration = configuration
        self.accepted = accepted
    }

    /// First payload any attachment of the first item yields (single-import share sheets).
    public func extractFirst(from items: [NSExtensionItem]) async -> PendingSharePayload? {
        guard let item = items.first else { return nil }
        for provider in item.attachments ?? [] {
            if let payload = await payload(for: provider) { return payload }
        }
        return nil
    }

    /// One payload per attachment that yields one, across every item (multi-select share sheets).
    public func extractAll(from items: [NSExtensionItem]) async -> [PendingSharePayload] {
        var payloads: [PendingSharePayload] = []
        for item in items {
            for provider in item.attachments ?? [] {
                if let payload = await payload(for: provider) { payloads.append(payload) }
            }
        }
        return payloads
    }

    public func payload(for provider: NSItemProvider) async -> PendingSharePayload? {
        let config = configuration
        if accepted.text, provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier),
           let value = try? await provider.loadItem(forTypeIdentifier: UTType.plainText.identifier, options: nil),
           let text = (value as? String)?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty {
            return PendingSharePayload(
                text: String(text.prefix(config.maxTextCharacters)), fileURL: nil, typeIdentifier: UTType.plainText.identifier
            )
        }
        // A file URL also conforms to public.url; treating it as a web link would hand the app a
        // `file://` string instead of a staged copy, so file-backed providers skip this branch.
        if accepted.url, provider.hasItemConformingToTypeIdentifier(UTType.url.identifier),
           !provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier),
           let value = try? await provider.loadItem(forTypeIdentifier: UTType.url.identifier, options: nil),
           let url = value as? URL {
            return PendingSharePayload(text: url.absoluteString, fileURL: nil, typeIdentifier: UTType.url.identifier)
        }
        for (type, fileExtension) in accepted.files where provider.hasItemConformingToTypeIdentifier(type.identifier) {
            if let staged = try? await Self.stageRepresentation(from: provider, typeIdentifier: type.identifier, fileExtension: fileExtension, configuration: config) {
                return PendingSharePayload(
                    text: nil, fileURL: staged.url.absoluteString, typeIdentifier: type.identifier, originalName: staged.originalName
                )
            }
            // Fallback for providers that only answer `loadItem`: accept a file URL, still capped.
            if let value = try? await provider.loadItem(forTypeIdentifier: type.identifier, options: nil),
               let url = value as? URL,
               let staged = try? Self.stageExternalFile(
                   from: url, fileExtension: url.pathExtension.isEmpty ? fileExtension : url.pathExtension, configuration: config
               ) {
                return PendingSharePayload(
                    text: nil, fileURL: staged.absoluteString, typeIdentifier: type.identifier, originalName: url.lastPathComponent
                )
            }
        }
        return nil
    }

    // MARK: Staging

    /// The provider-owned URL is only valid inside the completion callback, so the copy happens
    /// there; only the staged URL and name cross back through the continuation.
    private static func stageRepresentation(
        from provider: NSItemProvider,
        typeIdentifier: String,
        fileExtension: String,
        configuration: ShareIntakeConfiguration
    ) async throws -> (url: URL, originalName: String) {
        try await withCheckedThrowingContinuation { continuation in
            provider.loadFileRepresentation(forTypeIdentifier: typeIdentifier) { url, error in
                if let error { continuation.resume(throwing: error); return }
                guard let url else { continuation.resume(throwing: CocoaError(.fileReadUnknown)); return }
                do {
                    let staged = try stageExternalFile(from: url, fileExtension: fileExtension, configuration: configuration)
                    continuation.resume(returning: (staged, url.lastPathComponent))
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    /// Copies `url` into the staging directory under a hard byte cap. The pre-copy size is only a
    /// fast reject: it can be missing or wrong (pipes, providers, a file that grows), so the cap is
    /// enforced on the bytes actually copied. Bytes go to a unique partial file that is removed on
    /// any failure and published with an atomic rename, so no oversized or partial item survives.
    nonisolated static func stageExternalFile(from url: URL, fileExtension: String, configuration: ShareIntakeConfiguration) throws -> URL {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        if let size = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize, size > configuration.maxFileBytes {
            throw CocoaError(.fileReadTooLarge)
        }
        let root = configuration.containerRoot ?? FileManager.default.temporaryDirectory
        let directory = root.appendingPathComponent(configuration.stagingDirectoryName, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let name = UUID().uuidString
        let partial = directory.appendingPathComponent(".\(name).partial")
        let destination = directory.appendingPathComponent("\(name).\(fileExtension)")
        do {
            try boundedCopy(from: url, to: partial, maxBytes: configuration.maxFileBytes)
            // rename(2) publishes the complete file in one atomic step.
            guard rename(partial.path, destination.path) == 0 else { throw CocoaError(.fileWriteUnknown) }
            return destination
        } catch {
            try? FileManager.default.removeItem(at: partial)
            throw error
        }
    }

    /// Streams `source` into a new file at `destination`, never writing more than `maxBytes + 1`
    /// bytes before failing with `.fileReadTooLarge`.
    nonisolated static func boundedCopy(from source: URL, to destination: URL, maxBytes: Int) throws {
        let input = try FileHandle(forReadingFrom: source)
        defer { try? input.close() }
        let fd = open(destination.path, O_WRONLY | O_CREAT | O_EXCL, 0o600)
        guard fd >= 0 else { throw CocoaError(.fileWriteUnknown) }
        defer { close(fd) }
        let chunk = 64 * 1024
        var total = 0
        while true {
            // Never ask for more than one byte past the cap: reaching it proves the source is over.
            let want = min(chunk, maxBytes - total + 1)
            guard let data = try input.read(upToCount: max(want, 1)), !data.isEmpty else { break }
            total += data.count
            if total > maxBytes { throw CocoaError(.fileReadTooLarge) }
            try data.withUnsafeBytes { raw in
                var offset = 0
                while offset < raw.count {
                    let n = write(fd, raw.baseAddress! + offset, raw.count - offset)
                    if n < 0 && errno == EINTR { continue }
                    guard n > 0 else { throw CocoaError(.fileWriteUnknown) }
                    offset += n
                }
            }
        }
        guard fsync(fd) == 0 else { throw CocoaError(.fileWriteUnknown) }
    }
}
