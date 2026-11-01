import Foundation

/*
 Provenance (read-only donor): OpenMausBot @ 6951c40156ba0916a10b6e49e5051b7785198b93,
 `ios/App/Session.swift:1008-1019` (`Session.export`). The donor takes the
 server artifact filename's last path component, writes bytes with `.atomic` to
 `FileManager.default.temporaryDirectory`, and returns that URL without eager
 cleanup so native share consumers can finish reading it.
*/

public struct CodeRightTranscriptShareFile: Identifiable, Sendable {
    public let url: URL
    public let contentType: String

    public var id: String { url.path }

    public init(url: URL, contentType: String) {
        self.url = url
        self.contentType = contentType
    }
}

public enum TranscriptExportWriterError: Error, LocalizedError, Sendable {
    case emptyFilename

    public var errorDescription: String? {
        switch self {
        case .emptyFilename: return "The transcript export did not include a filename."
        }
    }
}

/// Writes one donor-format artifact into a temporary shareable file.
public struct TranscriptExportWriter {
    public let directory: URL
    private let fileManager: FileManager

    public init(
        directory: URL = FileManager.default.temporaryDirectory,
        fileManager: FileManager = .default
    ) {
        self.directory = directory
        self.fileManager = fileManager
    }

    @discardableResult
    public func write(
        input: any CodeRightTranscriptExportInput,
        format: CodeRightTranscriptExportFormat
    ) throws -> CodeRightTranscriptShareFile {
        let artifact = try CodeRightTranscriptExportFormatter.artifact(from: input, format: format)
        // Keep donor's path-boundary guard even though formatter filenames are
        // already restricted to basename-safe characters.
        let filename = URL(fileURLWithPath: artifact.filename).lastPathComponent
        guard !filename.isEmpty, filename != ".", filename != ".." else {
            throw TranscriptExportWriterError.emptyFilename
        }

        if !fileManager.fileExists(atPath: directory.path) {
            try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        }
        let url = directory.appendingPathComponent(filename, isDirectory: false)
        try artifact.data.write(to: url, options: .atomic)
        return CodeRightTranscriptShareFile(url: url, contentType: artifact.contentType)
    }
}
