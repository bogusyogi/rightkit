import Foundation

// Update-check owner for native Apple apps: the Swift counterpart of `@rightkit/app-update`
// (version, channels, check gate, probation) over the same release feeds, plus the macOS
// download, signature verification and in-place install that the TypeScript package delegates to
// Tauri. Nothing here installs without a verifier: an artifact that cannot be verified is refused.
// Per settled decision D1 (issue 771) no install step keeps a `.previous` copy.

/// A semantic version (`1.2.3`, `v1.2.3-beta.1+build`), ordered by semver precedence like
/// `compareVersions` in `@rightkit/app-update`. Build metadata is ignored.
public struct SemanticVersion: Sendable, Hashable, Comparable, CustomStringConvertible {
    public enum Identifier: Sendable, Hashable, Comparable {
        case number(Int)
        case text(String)

        public static func < (lhs: Identifier, rhs: Identifier) -> Bool {
            switch (lhs, rhs) {
            case let (.number(a), .number(b)): return a < b
            case let (.text(a), .text(b)): return a < b
            case (.number, .text): return true
            case (.text, .number): return false
            }
        }
    }

    public let major: Int
    public let minor: Int
    public let patch: Int
    public let prerelease: [Identifier]

    /// Parses `[v]MAJOR.MINOR.PATCH[-pre][+build]`. With `allowShort`, `1.2` and `1` read as `1.2.0`
    /// and `1.0.0` (App Store versions). Anything else is nil, so a malformed tag never looks like an update.
    public init?(_ text: String, allowShort: Bool = false) {
        var rest = Substring(text.trimmingCharacters(in: .whitespacesAndNewlines))
        if let first = rest.first, first == "v" || first == "V" { rest = rest.dropFirst() }
        if let plus = rest.firstIndex(of: "+") {
            let build = rest[rest.index(after: plus)...]
            guard Self.validIdentifiers(build) else { return nil }
            rest = rest[..<plus]
        }
        var pre: [Identifier] = []
        if let dash = rest.firstIndex(of: "-") {
            let tail = rest[rest.index(after: dash)...]
            guard Self.validIdentifiers(tail) else { return nil }
            pre = tail.split(separator: ".", omittingEmptySubsequences: false).map { (part) -> Identifier in
                if part.allSatisfy(\.isASCIIDigit), let n = Int(part) { return .number(n) }
                return .text(String(part))
            }
            rest = rest[..<dash]
        }
        let parts = rest.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.allSatisfy({ !$0.isEmpty && $0.allSatisfy(\.isASCIIDigit) }) else { return nil }
        let numbers = parts.compactMap { Int($0) }
        guard numbers.count == parts.count else { return nil }
        guard numbers.count == 3 || (allowShort && (1...3).contains(numbers.count)) else { return nil }
        major = numbers[0]
        minor = numbers.count > 1 ? numbers[1] : 0
        patch = numbers.count > 2 ? numbers[2] : 0
        prerelease = pre
    }

    private static func validIdentifiers(_ text: Substring) -> Bool {
        let parts = text.split(separator: ".", omittingEmptySubsequences: false)
        return !parts.isEmpty && parts.allSatisfy { !$0.isEmpty && $0.allSatisfy { $0.isASCIIDigit || $0.isASCIILetter || $0 == "-" } }
    }

    public var description: String {
        let core = "\(major).\(minor).\(patch)"
        guard !prerelease.isEmpty else { return core }
        return core + "-" + prerelease.map { id in
            switch id {
            case let .number(n): return String(n)
            case let .text(t): return t
            }
        }.joined(separator: ".")
    }

    public static func < (lhs: SemanticVersion, rhs: SemanticVersion) -> Bool {
        if lhs.major != rhs.major { return lhs.major < rhs.major }
        if lhs.minor != rhs.minor { return lhs.minor < rhs.minor }
        if lhs.patch != rhs.patch { return lhs.patch < rhs.patch }
        switch (lhs.prerelease.isEmpty, rhs.prerelease.isEmpty) {
        case (true, true): return false
        case (true, false): return false
        case (false, true): return true
        case (false, false): break
        }
        for index in 0..<max(lhs.prerelease.count, rhs.prerelease.count) {
            guard index < lhs.prerelease.count else { return true }
            guard index < rhs.prerelease.count else { return false }
            if lhs.prerelease[index] != rhs.prerelease[index] { return lhs.prerelease[index] < rhs.prerelease[index] }
        }
        return false
    }

    /// True when `candidate` is strictly newer than `current`; false when either does not parse.
    public static func isNewer(_ candidate: String, than current: String, allowShort: Bool = false) -> Bool {
        guard let a = SemanticVersion(candidate, allowShort: allowShort), let b = SemanticVersion(current, allowShort: allowShort) else {
            return false
        }
        return a > b
    }
}

private extension Character {
    var isASCIIDigit: Bool { self >= "0" && self <= "9" }
    var isASCIILetter: Bool { (self >= "a" && self <= "z") || (self >= "A" && self <= "Z") }
}
