// The Tauri tour's inventory, the label matching between it and the SwiftUI hub's controls, and the
// parity report (parity.json, parity.md). QA only.

import Foundation

/// True once the tour has run to its end; read by the exit hook, which runs outside any actor.
nonisolated(unsafe) var tourEndedNormally = false

// MARK: Tauri inventory

/// `$PULSE_QA_SHOTS/inventory.json` as hub/qa-e2e/tests/ui.rs writes it.
struct TauriInventory: Decodable {
    let platform: String?
    let controls: [TauriControl]
}

struct TauriControl: Decodable {
    let surface: String
    let label: String
    let role: String
    let slug: String
    let clicked: Bool
    let disabled: Bool?
    let action: String?
    let after: String?

    enum CodingKeys: String, CodingKey {
        case surface, label, role, slug, clicked, disabled, action, after
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        surface = (try? c.decode(String.self, forKey: .surface)) ?? ""
        label = (try? c.decode(String.self, forKey: .label)) ?? ""
        role = (try? c.decode(String.self, forKey: .role)) ?? ""
        slug = (try? c.decode(String.self, forKey: .slug)) ?? ""
        clicked = (try? c.decode(Bool.self, forKey: .clicked)) ?? false
        disabled = try? c.decode(Bool.self, forKey: .disabled)
        action = try? c.decode(String.self, forKey: .action)
        after = try? c.decode(String.self, forKey: .after)
    }
}

// MARK: Label matching

enum Match {
    /// Trimmed, whitespace collapsed, cut to 80 characters as the Tauri tour's inventory is
    /// (hub/qa-e2e `clean`), lowercased.
    static func normalize(_ text: String) -> String {
        String(text.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ").prefix(80)).lowercased()
    }

    /// The normalised label without a trailing count: "Updates (2)" and "Updates 2" both give "updates".
    static func withoutCount(_ text: String) -> String {
        var result = normalize(text)
        if let range = result.range(of: #"\s*\(\d+\)$"#, options: .regularExpression) {
            result.removeSubrange(range)
        } else if let range = result.range(of: #"\s+\d+$"#, options: .regularExpression) {
            result.removeSubrange(range)
        }
        return result
    }

    /// Labels whose text is a moment of a state, not a control: the Tauri tour caught them while a
    /// refresh or check ran; the SwiftUI hub has settled by the time it is read (#472).
    static func isTransient(_ label: String) -> Bool {
        let key = normalize(label)
        return key == "refreshing…" || key == "checking…" || key == "refreshing..." || key == "checking..."
    }

    /// The label with live ages ("5m ago", "2 hr", "3 days ago") removed, punctuation that only joins
    /// parts ("," and "·") turned into spaces, and whitespace collapsed.
    static func stable(_ text: String) -> String {
        var result = normalize(text)
        result = result.replacingOccurrences(
            of: #"\b\d+\s?(seconds?|secs?|minutes?|mins?|hours?|hrs?|days?|weeks?|[smhdw])\b(\s+ago)?"#,
            with: " ", options: .regularExpression)
        result = result.replacingOccurrences(of: "·", with: " ").replacingOccurrences(of: ",", with: " ")
        return result.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
    }

    /// Verbs whose control is named for a thing that differs by machine or fixture ("Rename <account>"):
    /// the verb alone pairs them.
    private static let dynamicVerbs: Set<String> = ["rename", "actions for"]

    private static func dynamicVerb(_ key: String) -> String? {
        for verb in dynamicVerbs where key.hasPrefix(verb + " ") { return verb }
        return nil
    }

    /// The indices (into `labels`) of the labels that name the same control as `label`: the exact
    /// normalised matches, else the matches without a trailing count, else the matches with live ages
    /// and joining punctuation removed or one a whole-word prefix of the other (a row read as one
    /// comma-joined label), else, for a dynamic verb, the labels with the same verb.
    static func candidates(for label: String, in labels: [String]) -> [Int] {
        let key = normalize(label)
        guard !key.isEmpty else { return [] }
        let exact = labels.indices.filter { normalize(labels[$0]) == key }
        if !exact.isEmpty { return exact }
        let bare = withoutCount(label)
        guard !bare.isEmpty else { return [] }
        let counted = labels.indices.filter { withoutCount(labels[$0]) == bare }
        if !counted.isEmpty { return counted }
        let steady = stable(label)
        guard !steady.isEmpty else { return [] }
        let aged = labels.indices.filter { stable(labels[$0]) == steady }
        if !aged.isEmpty { return aged }
        let prefixed = labels.indices.filter { index in
            let other = stable(labels[index])
            guard min(other.count, steady.count) >= 8 else { return false }
            return (other + " ").hasPrefix(steady + " ") || (steady + " ").hasPrefix(other + " ")
                || other.hasPrefix(steady) || steady.hasPrefix(other)
        }
        if !prefixed.isEmpty { return prefixed }
        if let verb = dynamicVerb(key) {
            return labels.indices.filter { dynamicVerb(normalize(labels[$0])) == verb }
        }
        return []
    }
}

// MARK: Report

struct PairReport: Codable {
    var label: String
    var tauriRole: String
    var swiftLabel: String
    var swiftRole: String
    var swiftValue: String
    var tauriClicked: Bool
    var pressed: Bool
    /// What happened: "pressed", "focused", "press returned false", "not pressed: ..." and so on.
    var note: String
    var tauriSlug: String
    var tauriAfter: String?
    var swiftAfter: String?
}

struct MissingReport: Codable {
    var label: String
    var role: String
    var clicked: Bool
    var tauriAction: String
}

struct ExtraReport: Codable {
    var label: String
    var role: String
    var value: String
}

struct SurfaceReport: Codable {
    var surface: String
    var title: String
    var tauriControls = 0
    var swiftControls = 0
    var matched: [PairReport] = []
    var missing: [MissingReport] = []
    var extra: [ExtraReport] = []
    var screenshot: String?
    var settled = true
    var note: String?
    /// How long the section's prepared view took to load, and what happened.
    var loadSeconds: Double?
    var loadNote: String?
    /// The accessibility strategy that found controls, and every strategy's counts.
    var axStrategy: String?
    var axCounts: String?
}

struct ParityTotals: Codable {
    var surfaces = 0
    var tauriControls = 0
    var matched = 0
    var missingInSwiftUI = 0
    var swiftUIOnly = 0
    var tauriClicked = 0
    var pressedPairs = 0
    var pressFailed = 0
}

struct ParityReport: Codable {
    var tauriPlatform: String
    var surfaces: [SurfaceReport] = []
    var totals = ParityTotals()
    var harnessNotes: [String] = []

    mutating func finish() {
        var totals = ParityTotals()
        totals.surfaces = surfaces.count
        for surface in surfaces {
            totals.tauriControls += surface.tauriControls
            totals.matched += surface.matched.count
            totals.missingInSwiftUI += surface.missing.count
            totals.swiftUIOnly += surface.extra.count
            totals.tauriClicked += surface.matched.filter(\.tauriClicked).count + surface.missing.filter(\.clicked).count
            totals.pressedPairs += surface.matched.filter(\.pressed).count
            totals.pressFailed += surface.matched.filter { $0.tauriClicked && !$0.pressed }.count
        }
        self.totals = totals
    }

    func json() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(self)
    }

    func markdown() -> String {
        func cell(_ text: String) -> String {
            text.replacingOccurrences(of: "|", with: "/").replacingOccurrences(of: "\n", with: " ")
        }
        var md = "# SwiftUI hub parity with the Tauri hub\n\n"
        md += "Tauri inventory platform: \(tauriPlatform). Matching is by label (trimmed, case-insensitive, trailing count ignored). "
        md += "A Tauri control with no SwiftUI counterpart is a **functional gap**. Only controls the Tauri tour clicked are pressed.\n\n"
        md += "## Totals\n\n"
        md += "| Measure | Count |\n|---|---|\n"
        md += "| Surfaces | \(totals.surfaces) |\n"
        md += "| Tauri controls | \(totals.tauriControls) |\n"
        md += "| Matched in SwiftUI | \(totals.matched) |\n"
        md += "| Functional gaps (Tauri control missing in SwiftUI) | \(totals.missingInSwiftUI) |\n"
        md += "| SwiftUI controls with no Tauri counterpart | \(totals.swiftUIOnly) |\n"
        md += "| Tauri controls the Tauri tour clicked | \(totals.tauriClicked) |\n"
        md += "| Pairs pressed in SwiftUI | \(totals.pressedPairs) |\n"
        md += "| Clicked-in-Tauri pairs the SwiftUI press did not reach | \(totals.pressFailed) |\n\n"
        for note in harnessNotes { md += "- \(note)\n" }
        if !harnessNotes.isEmpty { md += "\n" }
        for surface in surfaces {
            md += "## \(surface.title) (`\(surface.surface)`)\n\n"
            md += "Tauri controls \(surface.tauriControls), SwiftUI controls \(surface.swiftControls), matched \(surface.matched.count), "
            md += "missing \(surface.missing.count), SwiftUI only \(surface.extra.count)."
            if let shot = surface.screenshot { md += " Picture: `\(shot)`." }
            if let seconds = surface.loadSeconds {
                md += " Data: \(surface.loadNote ?? "") in \(String(format: "%.2f", seconds)) s."
            }
            if let strategy = surface.axStrategy { md += " Accessibility: \(strategy) (\(surface.axCounts ?? ""))." }
            if !surface.settled { md += " The view did not settle within 20 s." }
            if let note = surface.note { md += " \(note)" }
            md += "\n\n"
            if !surface.matched.isEmpty {
                md += "### Matched\n\n| Tauri control | Tauri role | SwiftUI control | SwiftUI role | Value | Pressed | Result | Tauri after | SwiftUI after |\n|---|---|---|---|---|---|---|---|---|\n"
                for pair in surface.matched {
                    md += "| \(cell(pair.label)) | \(pair.tauriRole) | \(cell(pair.swiftLabel)) | \(pair.swiftRole) | \(cell(pair.swiftValue)) | "
                    md += "\(pair.pressed ? "yes" : (pair.tauriClicked ? "NO" : "not clicked in Tauri")) | \(cell(pair.note)) | "
                    md += "\(pair.tauriAfter.map { "`\($0)`" } ?? "-") | \(pair.swiftAfter.map { "`\($0)`" } ?? "-") |\n"
                }
                md += "\n"
            }
            if !surface.missing.isEmpty {
                md += "### Functional gaps: in Tauri, missing in SwiftUI\n\n| Tauri control | Role | Clicked in Tauri | Tauri observed |\n|---|---|---|---|\n"
                for gap in surface.missing {
                    md += "| \(cell(gap.label)) | \(gap.role) | \(gap.clicked ? "yes" : "no") | \(cell(gap.tauriAction)) |\n"
                }
                md += "\n"
            }
            if !surface.extra.isEmpty {
                md += "### In SwiftUI only\n\n| SwiftUI control | Role | Value |\n|---|---|---|\n"
                for extra in surface.extra {
                    md += "| \(cell(extra.label)) | \(extra.role) | \(cell(extra.value)) |\n"
                }
                md += "\n"
            }
        }
        return md
    }
}
