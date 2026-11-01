import CryptoKit
import Foundation

// Kind detection (docs/clipboard-spec.md 3.3), previews (2.2), digests (2.3) and the secrets
// filter (3.5 step 7). Pure static functions: no pasteboard, no disk, safe on any queue.
// Link, colour and code detection follows the shared rules; Pesty and Clippi (MIT) were the
// sources the spec names for the rules, no code was copied.

enum ClipDigest {
    static func sha256(_ data: Data) -> String {
        let hash = SHA256.hash(data: data)
        let digits = Array("0123456789abcdef".utf8)
        var bytes: [UInt8] = []
        bytes.reserveCapacity(64)
        for byte in hash {
            bytes.append(digits[Int(byte >> 4)])
            bytes.append(digits[Int(byte & 0x0f)])
        }
        return "sha256:" + String(decoding: bytes, as: UTF8.self)
    }

    /// `text`, `link`, `colour`, `code`.
    static func text(_ text: String) -> String { sha256(Data(text.utf8)) }

    /// `rich`: the plain text, one 0x00 byte, then the stored HTML (empty when none).
    static func rich(text: String, html: Data?) -> String {
        var data = Data(text.utf8)
        data.append(0)
        if let html { data.append(html) }
        return sha256(data)
    }

    /// `file`: the paths sorted by code point (UTF-8 byte order is the same), each ending in `\n`.
    static func files(_ paths: [String]) -> String {
        let sorted = paths.sorted { $0.utf8.lexicographicallyPrecedes($1.utf8) }
        var data = Data()
        for path in sorted {
            data.append(Data(path.utf8))
            data.append(0x0a)
        }
        return sha256(data)
    }
}

enum ClipPreview {
    /// `text`, `code`, `rich`: the first 2000 characters, `\r\n` turned into `\n`.
    static func text(_ text: String) -> String {
        String(text.replacingOccurrences(of: "\r\n", with: "\n").prefix(2000))
    }

    /// `file`: the names joined by ", ", up to 2000 characters.
    static func files(names: [String]) -> String {
        String(names.joined(separator: ", ").prefix(2000))
    }
}

enum ClipSecrets {
    private static let patterns: [NSRegularExpression] = [
        // A PEM private key block starts with its header; the whole text is the key.
        "^-----BEGIN [A-Z ]*PRIVATE KEY-----",
        "^(AKIA|ASIA)[0-9A-Z]{16}$",
        "^gh[pousr]_[A-Za-z0-9]{36,}$",
        "^xox[abpr]-[A-Za-z0-9-]{10,}$",
        "^eyJ[A-Za-z0-9_-]+\\.[A-Za-z0-9_-]+\\.[A-Za-z0-9_-]+$",
        "^[sr]k_live_[A-Za-z0-9]{20,}$",
        "^Bearer [A-Za-z0-9._~+/=-]{20,}$",
    ].compactMap { try? NSRegularExpression(pattern: $0) }

    /// 3.5 step 7: the whole trimmed text is one of the high-confidence patterns.
    static func matches(_ trimmed: String) -> Bool {
        let range = NSRange(trimmed.startIndex..., in: trimmed)
        return patterns.contains { $0.firstMatch(in: trimmed, options: [], range: range) != nil }
    }
}

enum ClipDetect {
    struct Result: Equatable, Sendable {
        var kind: ClipKind
        var meta: ClipMeta
        var preview: String
    }

    /// 3.3: the first rule that matches sets the kind.
    static func classify(_ text: String) -> Result {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        var meta = ClipMeta()
        if let hex = colour(trimmed) {
            meta.hex = hex
            meta.original = trimmed
            return Result(kind: .colour, meta: meta, preview: hex)
        }
        if let found = link(trimmed) {
            meta.host = found.host
            meta.scheme = found.scheme
            return Result(kind: .link, meta: meta, preview: trimmed)
        }
        let isCode = looksLikeCode(text)
        meta.chars = text.unicodeScalars.count
        meta.lines = lineCount(text)
        if isCode { meta.language = language(of: text) }
        return Result(kind: isCode ? .code : .text, meta: meta, preview: ClipPreview.text(text))
    }

    static func lineCount(_ text: String) -> Int {
        guard !text.isEmpty else { return 0 }
        return text.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n").count
    }

    // MARK: Colour

    private static let rgbPattern = try? NSRegularExpression(
        pattern: "^rgba?\\(\\s*(\\d{1,3})\\s*,\\s*(\\d{1,3})\\s*,\\s*(\\d{1,3})\\s*(?:,\\s*(\\d*\\.?\\d+)\\s*)?\\)$",
        options: [.caseInsensitive])
    private static let hslPattern = try? NSRegularExpression(
        pattern: "^hsla?\\(\\s*(\\d{1,3}(?:\\.\\d+)?)(?:deg)?\\s*,\\s*(\\d{1,3}(?:\\.\\d+)?)%\\s*,\\s*(\\d{1,3}(?:\\.\\d+)?)%\\s*(?:,\\s*(\\d*\\.?\\d+)\\s*)?\\)$",
        options: [.caseInsensitive])

    /// `#RRGGBB` or `#RRGGBBAA` (alpha only when below 1), upper-case; nil when `trimmed` is
    /// not a colour.
    static func colour(_ trimmed: String) -> String? {
        if let hex = hexColour(trimmed) { return hex }
        if let parts = captures(rgbPattern, in: trimmed) {
            guard let r = Int(parts[0]), let g = Int(parts[1]), let b = Int(parts[2]),
                  r <= 255, g <= 255, b <= 255 else { return nil }
            guard let alpha = alphaByte(parts[3]) else { return nil }
            return format(r, g, b, alpha)
        }
        if let parts = captures(hslPattern, in: trimmed) {
            guard let h = Double(parts[0]), let s = Double(parts[1]), let l = Double(parts[2]),
                  h <= 360, s <= 100, l <= 100 else { return nil }
            guard let alpha = alphaByte(parts[3]) else { return nil }
            let (r, g, b) = hslToRGB(h: h, s: s / 100, l: l / 100)
            return format(r, g, b, alpha)
        }
        return nil
    }

    private static func isHexByte(_ byte: UInt8) -> Bool {
        (byte >= 48 && byte <= 57) || (byte >= 65 && byte <= 70) || (byte >= 97 && byte <= 102)
    }

    private static func hexColour(_ text: String) -> String? {
        let hashed = text.hasPrefix("#")
        let body = hashed ? String(text.dropFirst()) : text
        let bytes = Array(body.utf8)
        guard bytes.allSatisfy(isHexByte) else { return nil }
        if hashed {
            guard [3, 4, 6, 8].contains(bytes.count) else { return nil }
        } else {
            // Bare: exactly six digits and at least one letter a to f, so 123456 is text.
            guard bytes.count == 6, bytes.contains(where: { $0 > 57 }) else { return nil }
        }
        var digits = body.uppercased()
        if bytes.count <= 4 { digits = digits.map { "\($0)\($0)" }.joined() }
        if digits.count == 8, digits.hasSuffix("FF") { digits = String(digits.prefix(6)) }
        return "#" + digits
    }

    /// nil when the alpha text is out of range; `255` (opaque) when absent.
    private static func alphaByte(_ text: String) -> Int? {
        if text.isEmpty { return 255 }
        guard let alpha = Double(text), alpha >= 0, alpha <= 1 else { return nil }
        return Int((alpha * 255).rounded())
    }

    private static func format(_ r: Int, _ g: Int, _ b: Int, _ a: Int) -> String {
        var out = String(format: "#%02lX%02lX%02lX", r, g, b)
        if a < 255 { out += String(format: "%02lX", a) }
        return out
    }

    private static func hslToRGB(h: Double, s: Double, l: Double) -> (Int, Int, Int) {
        let hue = h.truncatingRemainder(dividingBy: 360) / 360
        let q = l < 0.5 ? l * (1 + s) : l + s - l * s
        let p = 2 * l - q
        func channel(_ offset: Double) -> Int {
            var t = hue + offset
            if t < 0 { t += 1 }
            if t > 1 { t -= 1 }
            let value: Double
            if t < 1.0 / 6 { value = p + (q - p) * 6 * t }
            else if t < 0.5 { value = q }
            else if t < 2.0 / 3 { value = p + (q - p) * (2.0 / 3 - t) * 6 }
            else { value = p }
            return Int((value * 255).rounded())
        }
        if s == 0 {
            let grey = Int((l * 255).rounded())
            return (grey, grey, grey)
        }
        return (channel(1.0 / 3), channel(0), channel(-1.0 / 3))
    }

    /// The capture groups; a group that did not take part is "".
    private static func captures(_ regex: NSRegularExpression?, in text: String) -> [String]? {
        guard let regex,
              let match = regex.firstMatch(in: text, options: [], range: NSRange(text.startIndex..., in: text))
        else { return nil }
        return (1..<match.numberOfRanges).map { index in
            let range = match.range(at: index)
            guard range.location != NSNotFound, let swiftRange = Range(range, in: text) else { return "" }
            return String(text[swiftRange])
        }
    }

    // MARK: Link

    private static let linkSchemes: Set<String> = [
        "http", "https", "ftp", "ftps", "mailto", "tel", "file", "ssh", "git",
        "slack", "zoommtg", "vscode", "obsidian", "notion", "spotify",
    ]

    /// A single token that is a URL of a known scheme, or a `www.` address (3.3 rule 2).
    static func link(_ trimmed: String) -> (host: String?, scheme: String?)? {
        guard !trimmed.isEmpty, trimmed.count <= 2048,
              trimmed.rangeOfCharacter(from: .whitespacesAndNewlines) == nil else { return nil }
        let lower = trimmed.lowercased()
        if lower.hasPrefix("www.") {
            guard lower.dropFirst(4).contains(".") else { return nil }
            return (host(of: lower), nil)
        }
        guard let colon = trimmed.firstIndex(of: ":") else { return nil }
        let scheme = lower[..<colon]
        guard let first = scheme.unicodeScalars.first, CharacterSet.letters.contains(first),
              scheme.unicodeScalars.allSatisfy({
                  CharacterSet.alphanumerics.contains($0) || "+-.".unicodeScalars.contains($0)
              }), linkSchemes.contains(String(scheme)) else { return nil }
        let rest = trimmed[trimmed.index(after: colon)...]
        guard !rest.isEmpty else { return nil }
        let name = String(scheme)
        switch name {
        case "http", "https":
            guard rest.hasPrefix("//"), let found = host(of: lower), found.contains(".") else { return nil }
            return (found, name)
        case "ftp", "ftps":
            guard rest.hasPrefix("//"), let found = host(of: lower), !found.isEmpty else { return nil }
            return (found, name)
        default:
            return (rest.hasPrefix("//") ? host(of: lower) : nil, name)
        }
    }

    /// The host of an address with or without a scheme, lower-case, no `www.`, no port or login.
    private static func host(of address: String) -> String? {
        var text = Substring(address)
        if let range = text.range(of: "//") {
            text = text[range.upperBound...]
        }
        let end = text.firstIndex(where: { "/?#".contains($0) }) ?? text.endIndex
        var authority = text[..<end]
        if let at = authority.lastIndex(of: "@") { authority = authority[authority.index(after: at)...] }
        if !authority.hasPrefix("["), let colon = authority.lastIndex(of: ":"),
           authority[authority.index(after: colon)...].allSatisfy({ $0.isASCII && $0.isNumber }) {
            authority = authority[..<colon]
        }
        var host = String(authority).lowercased()
        if host.hasPrefix("www.") { host = String(host.dropFirst(4)) }
        return host.isEmpty ? nil : host
    }

    // MARK: Code

    private static let keywordPattern = try? NSRegularExpression(
        pattern: "^\\s*(func|fn|def|class|struct|enum|impl|import|from|const|let|var|public|private|#include|using|package|return|if|for|while)\\b",
        options: [.anchorsMatchLines])

    /// 3.3 rule 3: two or more lines and at least two of the four signs.
    static func looksLikeCode(_ text: String) -> Bool {
        let normalised = text.replacingOccurrences(of: "\r\n", with: "\n")
        var lines = normalised.components(separatedBy: "\n")
        while let last = lines.last, last.trimmingCharacters(in: .whitespaces).isEmpty { lines.removeLast() }
        guard lines.count >= 2 else { return false }
        let filled = lines.filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
        guard !filled.isEmpty else { return false }
        var signs = 0
        let indented = filled.filter { $0.hasPrefix("  ") || $0.hasPrefix("\t") }.count
        if Double(indented) / Double(filled.count) >= 0.3 { signs += 1 }
        let endings = filled.filter { line in
            let end = line.trimmingCharacters(in: .whitespaces)
            return end.hasSuffix(";") || end.hasSuffix("{") || end.hasSuffix("}") || end.hasSuffix(")")
        }.count
        if endings >= 2 { signs += 1 }
        if let keywordPattern,
           keywordPattern.firstMatch(in: normalised, options: [], range: NSRange(normalised.startIndex..., in: normalised)) != nil {
            signs += 1
        }
        if ["=>", "->", "::", "</", "/>"].contains(where: { normalised.contains($0) })
            || (normalised.contains("{ ") && normalised.contains(" }")) {
            signs += 1
        }
        return signs >= 2
    }

    /// A cosmetic guess from the first line (3.3).
    static func language(of text: String) -> String? {
        let first = text.components(separatedBy: .newlines).first?
            .trimmingCharacters(in: .whitespaces) ?? ""
        let lower = first.lowercased()
        if lower.hasPrefix("#!") {
            if lower.contains("python") { return "python" }
            if lower.contains("node") { return "javascript" }
            if lower.contains("ruby") { return "ruby" }
            if lower.contains("perl") { return "perl" }
            if lower.contains("zsh") { return "zsh" }
            return lower.contains("bash") ? "bash" : "sh"
        }
        if lower.hasPrefix("<?php") { return "php" }
        if lower.hasPrefix("<!doctype html") { return "html" }
        if first == "{", text.contains("\"") { return "json" }
        return nil
    }
}
