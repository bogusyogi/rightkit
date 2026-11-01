import Foundation

enum TranscriptHistoryTools {
    static func filter(_ records: [SharedDisplayTranscript], query: String) -> [SharedDisplayTranscript] {
        let normalizedQuery = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalizedQuery.isEmpty else { return records }
        return records.filter {
            $0.text.range(of: normalizedQuery, options: [.caseInsensitive, .diacriticInsensitive]) != nil
        }
    }

    static func exportText(_ records: [SharedDisplayTranscript]) -> String {
        guard !records.isEmpty else { return "the app transcript history\n\nNo transcripts." }
        var lines = ["the app transcript history", ""]
        for record in records {
            lines.append(record.createdAt.formatted(.iso8601.year().month().day().time(includingFractionalSeconds: false)))
            lines.append(record.text)
            lines.append("")
        }
        return lines.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
