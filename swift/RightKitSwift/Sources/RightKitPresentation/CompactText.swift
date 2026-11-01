import Foundation

/// Conservative excerpts for compact surfaces: the first substantive text, display-only Markdown
/// markers removed, meaningful newlines kept, code punctuation untouched. An excerpt, not a summary.
public enum CompactText {
    public static func excerpt(_ text: String, limit: Int) -> String {
        guard limit > 0 else { return "" }
        let unified = text.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        let lines = unified.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        var out: [String] = []
        var length = 0
        var fence: (char: Character, length: Int)?
        var i = 0
        while i < lines.count {
            let line = lines[i]
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            i += 1
            if let open = fence {
                if trimmed.count >= open.length, trimmed.allSatisfy({ $0 == open.char }) { fence = nil; continue }
                // Code is shown exactly as written, minus trailing blanks.
                guard !trimmed.isEmpty else { continue }
                append(String(line.reversed().drop(while: { $0 == " " || $0 == "\t" }).reversed()), &out, &length, limit)
            } else if let first = trimmed.first, first == "`" || first == "~",
                      let ticks = Optional(trimmed.prefix { $0 == first }.count), ticks >= 3,
                      !(first == "`" && trimmed.dropFirst(ticks).contains("`")) {
                fence = (first, ticks)
                continue
            } else if trimmed.isEmpty || isRule(trimmed) || isTableSeparator(trimmed) {
                continue
            } else {
                append(display(trimmed), &out, &length, limit)
            }
            if length > limit { break }
        }
        return clip(out.joined(separator: "\n"), limit)
    }

    private static func append(_ line: String, _ out: inout [String], _ length: inout Int, _ limit: Int) {
        guard !line.isEmpty else { return }
        out.append(line)
        length += line.count + (out.count > 1 ? 1 : 0)
    }

    /// One non-code line without its block marker or inline markers.
    private static func display(_ line: String) -> String {
        var text = Substring(line)
        // Heading marker.
        let hashes = text.prefix { $0 == "#" }.count
        if (1...6).contains(hashes), text.dropFirst(hashes).first == " " {
            text = text.dropFirst(hashes)
            while text.first == " " { text = text.dropFirst() }
        }
        // Block quote markers.
        while text.first == ">" {
            text = text.dropFirst()
            if text.first == " " { text = text.dropFirst() }
        }
        var prefix = ""
        // Bullet: kept as a bullet glyph so the list still reads as a list.
        if let first = text.first, "-*+".contains(first), text.dropFirst().first == " " {
            text = text.dropFirst(2)
            prefix = "\u{2022} "
        }
        // Table row: outer pipes dropped, cells separated by a middle dot.
        var body = String(text)
        if body.hasPrefix("|"), body.hasSuffix("|"), body.count > 1 {
            body = String(body.dropFirst().dropLast())
                .split(separator: "|", omittingEmptySubsequences: false)
                .map { $0.trimmingCharacters(in: .whitespaces) }
                .joined(separator: " \u{00B7} ")
        }
        return prefix + InlineParser.plain(body).trimmingCharacters(in: .whitespaces)
    }

    private static func isRule(_ line: String) -> Bool {
        let marks = line.filter { $0 != " " }
        guard marks.count >= 3, let first = marks.first, "-*_=".contains(first) else { return false }
        return marks.allSatisfy { $0 == first }
    }

    private static func isTableSeparator(_ line: String) -> Bool {
        guard line.contains("-"), line.contains("|") else { return false }
        return line.allSatisfy { "|-: ".contains($0) }
    }

    /// Cuts at a word boundary when one is near the end, and marks the cut with an ellipsis.
    private static func clip(_ text: String, _ limit: Int) -> String {
        guard text.count > limit else { return text }
        let head = Array(text.prefix(limit - 1))
        var end = head.count
        let floor = head.count * 3 / 5
        while end > floor, !head[end - 1].isWhitespace, !head[end - 1].isNewline { end -= 1 }
        if end <= floor { end = head.count }
        var cut = String(head.prefix(end))
        while let last = cut.last, last.isWhitespace || last.isNewline { cut.removeLast() }
        return cut + "\u{2026}"
    }
}
