import CoreTransferable
import Foundation
import UniformTypeIdentifiers

/// A result as a Markdown file, built only from what the library already
/// stores. The desktop's `.md` is the extractor's own markdown variant with no
/// header, and iOS has no such variant, so this keeps a minimal front section
/// (title, source, date, engine when ASR produced the text) above the text.
struct MarkdownExport: Transferable {
    var title: String
    var sourceInput: String
    var createdAt: Date
    var engineName: String?
    var text: String

    init(artifact: Artifact, text: String) {
        self.title = artifact.title
        self.sourceInput = artifact.sourceInput
        self.createdAt = artifact.createdAt
        self.engineName = artifact.asrEngine?.displayName
        self.text = text
    }

    static let markdownType = UTType("net.daringfireball.markdown") ?? .plainText

    var markdown: String {
        let heading = title.components(separatedBy: .newlines).joined(separator: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        // A local-file job's input is its picked path; only a URL is a useful source line.
        // Pasted text has no source: its input is the whole paste.
        let isURL = URL(string: sourceInput)?.scheme?.hasPrefix("http") == true
        let isSingleLine = sourceInput.rangeOfCharacter(from: .newlines) == nil
        var lines = ["# \(heading.isEmpty ? "Untitled" : heading)", ""]
        if isURL {
            lines.append("- Source: \(sourceInput)")
        } else if isSingleLine, sourceInput.hasPrefix("/") || sourceInput.hasPrefix("file:") {
            lines.append("- Source: \((sourceInput as NSString).lastPathComponent)")
        }
        lines.append("- Date: \(createdAt.formatted(.iso8601.year().month().day()))")
        if let engineName { lines.append("- Engine: \(engineName)") }
        lines.append(contentsOf: ["", "---", "", text.trimmingCharacters(in: .whitespacesAndNewlines), ""])
        return lines.joined(separator: "\n")
    }

    /// File-name-safe stem from the title.
    var fileName: String {
        let cleaned = title.components(separatedBy: CharacterSet(charactersIn: "/\\:*?\"<>|\n\r")).joined(separator: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return String((cleaned.isEmpty ? "ScrapeRight result" : cleaned).prefix(60))
    }

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(exportedContentType: markdownType) { export in
            let directory = FileManager.default.temporaryDirectory
                .appendingPathComponent("md-export-\(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let url = directory.appendingPathComponent("\(export.fileName).md")
            try Data(export.markdown.utf8).write(to: url, options: .atomic)
            return SentTransferredFile(url)
        }
    }
}
