import Foundation

/// Work limits for structural formatting. Above either limit, or on any failure, the original text
/// is returned as one plain paragraph; the stored answer itself is never shortened.
public struct ParseLimits: Sendable, Equatable {
    public let maxBytes: Int
    public let maxNodes: Int

    public init(maxBytes: Int, maxNodes: Int) {
        self.maxBytes = maxBytes
        self.maxNodes = maxNodes
    }

    public static let `default` = ParseLimits(maxBytes: 128 * 1024, maxNodes: 500)
}

/// List markers for a `.list` element's flat items: top-level items are numbered by their
/// position among top-level items (a nested item between them does not take a number), and a
/// nested item gets `nil`, since its own marker is already its leading run.
public enum ListMarkers {
    public static func of(ordered: Bool, start: Int, items: [[InlineRun]]) -> [String?] {
        var next = start
        return items.map { runs in
            if runs.first?.text.hasPrefix("\u{2003}") == true { return nil }
            defer { next += 1 }
            return ordered ? "\(next)." : "\u{2022}"
        }
    }
}

public struct InlineRun: Sendable, Equatable {
    public let text: String
    public let emphasis: Bool
    public let strong: Bool
    public let code: Bool
    public let link: URL?

    public init(_ text: String, emphasis: Bool = false, strong: Bool = false, code: Bool = false, link: URL? = nil) {
        self.text = text
        self.emphasis = emphasis
        self.strong = strong
        self.code = code
        self.link = link
    }
}

public enum TextElement: Sendable, Equatable {
    case heading(level: Int, inline: [InlineRun])
    case paragraph([InlineRun])
    /// Nested items carry their depth in a leading indent and marker run, since items are flat.
    case list(ordered: Bool, start: Int, items: [[InlineRun]])
    case quote([InlineRun])
    case code(language: String?, text: String)
    case table(DisplayTable)
    case lineBreak
}

/// Ordered native text elements from a small, bounded Markdown grammar: paragraphs, headings,
/// lists, quotes, fenced code, simple pipe tables, and emphasis/strong/code/link inline styling.
/// Anything else (HTML, unsupported or malformed syntax) is literal text. Deterministic.
public struct TextDocument: Sendable, Equatable {
    public let elements: [TextElement]
    /// True when formatting was skipped (over a limit or failed) and `elements` is the complete original
    /// text as a single plain paragraph. Nothing is cut: this is "shown without formatting", never "shortened".
    public let unformatted: Bool

    public init(elements: [TextElement], unformatted: Bool) {
        self.elements = elements
        self.unformatted = unformatted
    }

    public static func parse(_ text: String, limits: ParseLimits = .default) -> TextDocument {
        if text.utf8.count > limits.maxBytes { return plainFallback(text) }
        var parser = BlockParser(text: text, maxNodes: limits.maxNodes)
        guard let elements = parser.run() else { return plainFallback(text) }
        return TextDocument(elements: elements, unformatted: false)
    }

    static func plainFallback(_ text: String) -> TextDocument {
        TextDocument(elements: text.isEmpty ? [] : [.paragraph([InlineRun(text)])], unformatted: true)
    }
}

// MARK: - Blocks

struct BlockParser {
    private let lines: [String]
    private let maxNodes: Int
    private var nodes = 0
    private var failed = false
    private var elements: [TextElement] = []

    init(text: String, maxNodes: Int) {
        let unified = text.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        self.lines = unified.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        self.maxNodes = maxNodes
    }

    private mutating func add(_ element: TextElement, weight: Int = 1) {
        nodes += weight
        if nodes > maxNodes { failed = true }
        elements.append(element)
    }

    mutating func run() -> [TextElement]? {
        var i = 0
        var blanks = 0
        while i < lines.count, !failed {
            let line = lines[i]
            let trimmed = Self.trim(line)
            if trimmed.isEmpty { blanks += 1; i += 1; continue }
            if blanks >= 2, !elements.isEmpty {
                for _ in 0..<min(blanks - 1, 3) { add(.lineBreak) }
            }
            blanks = 0

            if let fence = Self.fenceOpening(line) {
                i = fenced(from: i, fence: fence)
            } else if let (level, content) = Self.heading(trimmed) {
                add(.heading(level: level, inline: InlineParser.runs(content)))
                i += 1
            } else if let next = table(at: i) {
                i = next
            } else if trimmed.hasPrefix(">") {
                i = quote(from: i)
            } else if Self.marker(line) != nil {
                i = list(from: i)
            } else {
                i = paragraph(from: i)
            }
        }
        return failed ? nil : elements
    }

    // Fenced code

    private struct Fence { let char: Character; let length: Int; let indent: Int; let language: String? }

    private static func fenceOpening(_ line: String) -> Fence? {
        let indent = line.prefix { $0 == " " }.count
        guard indent < 4 else { return nil }
        let rest = line.dropFirst(indent)
        guard let first = rest.first, first == "`" || first == "~" else { return nil }
        let length = rest.prefix { $0 == first }.count
        guard length >= 3 else { return nil }
        let info = trim(String(rest.dropFirst(length)))
        if first == "`", info.contains("`") { return nil }
        let language = info.split(whereSeparator: { $0 == " " || $0 == "\t" }).first.map(String.init)
        return Fence(char: first, length: length, indent: indent, language: language)
    }

    private mutating func fenced(from start: Int, fence: Fence) -> Int {
        var body: [String] = []
        var i = start + 1
        while i < lines.count {
            let line = lines[i]
            let stripped = Self.trim(line)
            if stripped.count >= fence.length, stripped.allSatisfy({ $0 == fence.char }) {
                i += 1
                break
            }
            body.append(Self.dropIndent(line, upTo: fence.indent))
            i += 1
            if body.count > 100_000 { failed = true; break }
        }
        add(.code(language: fence.language, text: body.joined(separator: "\n")))
        return i
    }

    private static func dropIndent(_ line: String, upTo count: Int) -> String {
        var remaining = count
        var out = Substring(line)
        while remaining > 0, out.first == " " { out = out.dropFirst(); remaining -= 1 }
        return String(out)
    }

    // Headings

    private static func heading(_ trimmed: String) -> (Int, String)? {
        let hashes = trimmed.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes) else { return nil }
        let rest = trimmed.dropFirst(hashes)
        guard rest.isEmpty || rest.first == " " || rest.first == "\t" else { return nil }
        var content = trim(String(rest))
        // An optional closing run of # (preceded by a space) is a marker, not text.
        if let range = content.range(of: #"\s+#+$"#, options: .regularExpression) {
            content.removeSubrange(range)
        } else if content.allSatisfy({ $0 == "#" }) {
            content = ""
        }
        return (hashes, content)
    }

    // Tables

    private mutating func table(at start: Int) -> Int? {
        guard lines[start].contains("|"), start + 1 < lines.count else { return nil }
        let headers = Self.cells(lines[start])
        let maxColumns = 32
        let maxCells = 4096
        guard headers.count <= maxColumns else { failed = true; return start + 1 }
        let separator = lines[start + 1]
        guard separator.contains("-"), Self.isSeparator(separator), Self.cells(separator).count == headers.count else {
            return nil
        }
        var rows: [[String]] = []
        var i = start + 2
        while i < lines.count, !Self.trim(lines[i]).isEmpty, lines[i].contains("|") {
            let cells = Self.cells(lines[i])
            guard cells.count <= maxColumns else { failed = true; return i + 1 }
            rows.append(cells)
            i += 1
            if rows.count > maxNodes { failed = true; break }
        }
        // Never drop a column: a longer row widens the table, a shorter one pads with empty cells.
        let width = max(headers.count, rows.map(\.count).max() ?? 0)
        guard width <= maxColumns, rows.count + 1 <= maxCells / max(width, 1) else {
            failed = true
            return i
        }
        let pad: ([String]) -> [String] = { $0 + Array(repeating: "", count: width - $0.count) }
        add(.table(DisplayTable(headers: pad(headers), rows: rows.map(pad))), weight: 1 + rows.count)
        return i
    }

    private static func cells(_ line: String) -> [String] {
        var text = trim(line)
        if text.hasPrefix("|") { text.removeFirst() }
        if text.hasSuffix("|"), !text.hasSuffix("\\|") { text.removeLast() }
        var out: [String] = []
        var current = ""
        var escaped = false
        for ch in text {
            if escaped {
                if ch != "|" { current.append("\\") }
                current.append(ch)
                escaped = false
            } else if ch == "\\" {
                escaped = true
            } else if ch == "|" {
                out.append(trim(current))
                current = ""
            } else {
                current.append(ch)
            }
        }
        if escaped { current.append("\\") }
        out.append(trim(current))
        return out
    }

    private static func isSeparator(_ line: String) -> Bool {
        let parts = cells(line)
        guard !parts.isEmpty else { return false }
        return parts.allSatisfy { cell in
            var body = Substring(cell)
            if body.first == ":" { body = body.dropFirst() }
            if body.last == ":" { body = body.dropLast() }
            return !body.isEmpty && body.allSatisfy { $0 == "-" }
        }
    }

    // Quotes

    private mutating func quote(from start: Int) -> Int {
        var parts: [String] = []
        var i = start
        while i < lines.count {
            let trimmed = Self.trim(lines[i])
            guard trimmed.hasPrefix(">") else { break }
            var body = trimmed.dropFirst()
            if body.first == " " { body = body.dropFirst() }
            parts.append(String(body))
            i += 1
        }
        add(.quote(InlineParser.runs(parts.joined(separator: "\n"))))
        return i
    }

    // Lists

    private struct Marker { let indent: Int; let ordered: Bool; let number: Int; let content: String }

    private static func marker(_ line: String) -> Marker? {
        var indent = 0
        var index = line.startIndex
        while index < line.endIndex, line[index] == " " || line[index] == "\t" {
            indent += line[index] == "\t" ? 4 : 1
            index = line.index(after: index)
        }
        guard indent < 8, index < line.endIndex else { return nil }
        let rest = line[index...]
        let first = rest.first!
        if "-*+".contains(first) {
            let after = rest.dropFirst()
            guard after.isEmpty || after.first == " " || after.first == "\t" else { return nil }
            let content = trim(String(after))
            // A line of only repeated markers ("***", "- - -") is a rule, not an item.
            if content.isEmpty, rest.count > 1 { return nil }
            return Marker(indent: indent, ordered: false, number: 0, content: content)
        }
        let digits = rest.prefix { $0.isASCII && $0.isNumber }
        guard (1...9).contains(digits.count) else { return nil }
        let after = rest.dropFirst(digits.count)
        guard let delimiter = after.first, delimiter == "." || delimiter == ")" else { return nil }
        let tail = after.dropFirst()
        guard tail.isEmpty || tail.first == " " || tail.first == "\t" else { return nil }
        return Marker(indent: indent, ordered: true, number: Int(digits) ?? 1, content: trim(String(tail)))
    }

    private mutating func list(from start: Int) -> Int {
        let first = Self.marker(lines[start])!
        struct Item { var depth: Int; var ordered: Bool; var number: Int; var text: String }
        var items: [Item] = []
        var i = start
        while i < lines.count, !failed {
            let line = lines[i]
            let trimmed = Self.trim(line)
            if trimmed.isEmpty {
                // A blank line continues the list only when the next line is another item of it.
                var j = i + 1
                while j < lines.count, Self.trim(lines[j]).isEmpty { j += 1 }
                if j < lines.count, let next = Self.marker(lines[j]),
                   next.indent >= first.indent + 2 || (next.indent < first.indent + 2 && next.ordered == first.ordered) {
                    i = j
                    continue
                }
                break
            }
            if let marker = Self.marker(line) {
                let nested = marker.indent >= first.indent + 2
                if !nested, marker.ordered != first.ordered { break }
                let depth = nested ? min(max((marker.indent - first.indent) / 2, 1), 4) : 0
                items.append(Item(depth: depth, ordered: marker.ordered, number: marker.number, text: marker.content))
                i += 1
                continue
            }
            let indent = line.prefix { $0 == " " || $0 == "\t" }.count
            if indent > first.indent, !items.isEmpty {
                items[items.count - 1].text += "\n" + trimmed
                i += 1
                continue
            }
            break
        }
        let built: [[InlineRun]] = items.map { item in
            var runs = InlineParser.runs(item.text)
            if item.depth > 0 {
                let indent = String(repeating: "\u{2003}", count: item.depth)
                let mark = item.ordered ? "\(item.number). " : "\u{2013} "
                runs.insert(InlineRun(indent + mark), at: 0)
            }
            return runs
        }
        add(.list(ordered: first.ordered, start: first.ordered ? first.number : 1, items: built),
            weight: 1 + items.count)
        return i
    }

    // Paragraphs

    private mutating func paragraph(from start: Int) -> Int {
        var parts: [String] = [Self.trim(lines[start])]
        var i = start + 1
        while i < lines.count {
            let line = lines[i]
            let trimmed = Self.trim(line)
            if trimmed.isEmpty || Self.fenceOpening(line) != nil || Self.heading(trimmed) != nil
                || trimmed.hasPrefix(">") || Self.marker(line) != nil { break }
            if line.contains("|"), i + 1 < lines.count, Self.isSeparator(lines[i + 1]), lines[i + 1].contains("-") { break }
            parts.append(trimmed)
            i += 1
        }
        add(.paragraph(InlineParser.runs(parts.joined(separator: "\n"))))
        return i
    }

    static func trim(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespaces)
    }
}

// MARK: - Inline

/// Inline styling over scalars: code spans, strong, emphasis and links to http, https and mailto.
/// An unmatched or unsupported marker stays as literal text; images keep their literal source.
enum InlineParser {
    private struct Flags: Equatable {
        var emphasis = false
        var strong = false
        var code = false
        var link: URL?
    }

    private struct Builder {
        var runs: [InlineRun] = []
        var pending = String.UnicodeScalarView()
        var flags = Flags()

        mutating func add(_ scalar: Unicode.Scalar, _ style: Flags) {
            if style != flags { flush(); flags = style }
            pending.append(scalar)
        }

        mutating func add<S: Sequence>(_ scalars: S, _ style: Flags) where S.Element == Unicode.Scalar {
            for scalar in scalars { add(scalar, style) }
        }

        mutating func flush() {
            guard !pending.isEmpty else { return }
            let text = String(pending)
            pending = String.UnicodeScalarView()
            if let last = runs.last, !flags.code, !last.code, last.emphasis == flags.emphasis,
               last.strong == flags.strong, last.link == flags.link {
                runs[runs.count - 1] = InlineRun(last.text + text, emphasis: last.emphasis,
                                                 strong: last.strong, code: false, link: last.link)
            } else {
                runs.append(InlineRun(text, emphasis: flags.emphasis, strong: flags.strong,
                                      code: flags.code, link: flags.link))
            }
        }
    }

    private static let reach = 2000
    private static let maxDepth = 6

    static func runs(_ text: String) -> [InlineRun] {
        let scalars = Array(text.unicodeScalars)
        var builder = Builder()
        parse(scalars, 0, scalars.count, Flags(), &builder, depth: 0)
        builder.flush()
        return builder.runs
    }

    private static func isPunct(_ s: Unicode.Scalar) -> Bool {
        "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~".unicodeScalars.contains(s)
    }

    private static func isSpace(_ s: Unicode.Scalar) -> Bool {
        s == " " || s == "\t" || s == "\n"
    }

    private static func isWordChar(_ s: Unicode.Scalar) -> Bool {
        CharacterSet.alphanumerics.contains(s)
    }

    private static func parse(_ s: [Unicode.Scalar], _ from: Int, _ to: Int, _ style: Flags,
                              _ out: inout Builder, depth: Int) {
        var i = from
        while i < to {
            let c = s[i]
            switch c {
            case "\\":
                if i + 1 < to, isPunct(s[i + 1]) { out.add(s[i + 1], style); i += 2 } else { out.add(c, style); i += 1 }
            case "`":
                var n = 0
                while i + n < to, s[i + n] == "`" { n += 1 }
                if let close = closingTicks(s, from: i + n, to: to, count: n) {
                    var content = Array(s[(i + n)..<close])
                    if content.count >= 2, content.first == " ", content.last == " ", !content.allSatisfy({ $0 == " " }) {
                        content = Array(content.dropFirst().dropLast())
                    }
                    var codeStyle = style
                    codeStyle.code = true
                    out.add(content, codeStyle)
                    i = close + n
                } else {
                    out.add(s[i..<(i + n)], style)
                    i += n
                }
            case "!":
                if i + 1 < to, s[i + 1] == "[", let label = bracketEnd(s, open: i + 1, to: to),
                   label + 1 < to, s[label + 1] == "(", let paren = parenEnd(s, open: label + 1, to: to) {
                    // An image is never fetched: its alt text and literal reference stay as text.
                    out.add(s[i...paren], style)
                    i = paren + 1
                } else {
                    out.add(c, style); i += 1
                }
            case "[":
                if style.link == nil, depth < maxDepth, let label = bracketEnd(s, open: i, to: to),
                   label + 1 < to, s[label + 1] == "(", let paren = parenEnd(s, open: label + 1, to: to),
                   let url = linkURL(Array(s[(label + 2)..<paren])) {
                    var linked = style
                    linked.link = url
                    parse(s, i + 1, label, linked, &out, depth: depth + 1)
                    i = paren + 1
                } else {
                    out.add(c, style); i += 1
                }
            case "*", "_":
                i = emphasis(s, i, to, style, &out, depth: depth)
            default:
                out.add(c, style); i += 1
            }
        }
    }

    private static func emphasis(_ s: [Unicode.Scalar], _ i: Int, _ to: Int, _ style: Flags,
                                 _ out: inout Builder, depth: Int) -> Int {
        let c = s[i]
        var run = 0
        while i + run < to, s[i + run] == c { run += 1 }
        let after = i + run < to ? s[i + run] : nil
        let opens = after.map { !isSpace($0) } ?? false
        let intraword = c == "_" && i > 0 && isWordChar(s[i - 1])
        if depth < maxDepth, opens, !intraword {
            for d in stride(from: min(run, 3), through: 1, by: -1) {
                if let close = closer(s, c, d, from: i + d, to: to) {
                    var inner = style
                    if d >= 2 { inner.strong = true }
                    if d == 1 || d == 3 { inner.emphasis = true }
                    // Any leftover delimiters of the run stay literal text inside.
                    if run > d { out.add(s[(i + d)..<(i + run)], inner) }
                    parse(s, i + run, close, inner, &out, depth: depth + 1)
                    return close + d
                }
            }
        }
        out.add(s[i..<(i + run)], style)
        return i + run
    }

    private static func closer(_ s: [Unicode.Scalar], _ c: Unicode.Scalar, _ d: Int, from: Int, to: Int) -> Int? {
        let limit = min(to, from + reach)
        var j = from
        while j < limit {
            let x = s[j]
            if x == "\\" { j += 2; continue }
            if x == "`" {
                var n = 0
                while j + n < to, s[j + n] == "`" { n += 1 }
                if let close = closingTicks(s, from: j + n, to: to, count: n) { j = close + n } else { j += n }
                continue
            }
            if x == c {
                var m = 0
                while j + m < to, s[j + m] == c { m += 1 }
                let precededOK = j > from && !isSpace(s[j - 1])
                let accepts = d == 1 ? m == 1 : m >= d
                if accepts, precededOK {
                    let following = j + d < to ? s[j + d] : nil
                    if c == "_", let f = following, isWordChar(f) { j += m; continue }
                    return j
                }
                j += m
                continue
            }
            j += 1
        }
        return nil
    }

    private static func closingTicks(_ s: [Unicode.Scalar], from: Int, to: Int, count: Int) -> Int? {
        var j = from
        let limit = min(to, from + reach)
        while j < limit {
            if s[j] == "`" {
                var n = 0
                while j + n < to, s[j + n] == "`" { n += 1 }
                if n == count { return j }
                j += n
            } else {
                j += 1
            }
        }
        return nil
    }

    private static func bracketEnd(_ s: [Unicode.Scalar], open: Int, to: Int) -> Int? {
        var depth = 0
        var j = open
        let limit = min(to, open + reach)
        while j < limit {
            switch s[j] {
            case "\\": j += 1
            case "[": depth += 1
            case "]":
                depth -= 1
                if depth == 0 { return j }
            default: break
            }
            j += 1
        }
        return nil
    }

    private static func parenEnd(_ s: [Unicode.Scalar], open: Int, to: Int) -> Int? {
        var depth = 0
        var j = open
        let limit = min(to, open + reach)
        while j < limit {
            switch s[j] {
            case "\n": return nil
            case "\\": j += 1
            case "(": depth += 1
            case ")":
                depth -= 1
                if depth == 0 { return j }
            default: break
            }
            j += 1
        }
        return nil
    }

    /// Only http, https and mailto links with a usable target become links; the rest stay text.
    private static func linkURL(_ raw: [Unicode.Scalar]) -> URL? {
        var text = String(String.UnicodeScalarView(raw)).trimmingCharacters(in: .whitespaces)
        if let space = text.firstIndex(where: { $0 == " " || $0 == "\t" }) { text = String(text[..<space]) }
        if text.hasPrefix("<"), text.hasSuffix(">") { text = String(text.dropFirst().dropLast()) }
        guard !text.isEmpty, let url = URL(string: text), let scheme = url.scheme?.lowercased() else { return nil }
        switch scheme {
        case "http", "https":
            guard let host = url.host, !host.isEmpty else { return nil }
            return url
        case "mailto":
            return text.count > 7 ? url : nil
        default:
            return nil
        }
    }

    /// Plain text of a line's inline runs: display-only markers removed, code and punctuation kept.
    static func plain(_ text: String) -> String {
        runs(text).map(\.text).joined()
    }
}
