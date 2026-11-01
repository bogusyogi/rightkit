import Foundation

enum IOSOpenTarget: String, Codable, CaseIterable, Equatable, Identifiable {
    case browser
    case mail
    case messages
    case maps
    case calendar
    case settings
    case shortcuts
    case phone
    case facetime
    case music
    case podcasts
    case appStore
    case whatsApp

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .browser:
            return "Browser"
        case .mail:
            return "Mail"
        case .messages:
            return "Messages"
        case .maps:
            return "Maps"
        case .calendar:
            return "Calendar"
        case .settings:
            return "Settings"
        case .shortcuts:
            return "Shortcuts"
        case .phone:
            return "Phone"
        case .facetime:
            return "FaceTime"
        case .music:
            return "Music"
        case .podcasts:
            return "Podcasts"
        case .appStore:
            return "App Store"
        case .whatsApp:
            return "WhatsApp"
        }
    }

    var url: URL {
        switch self {
        case .browser:
            return URL(string: "https://www.google.com")!
        case .mail:
            return URL(string: "mailto:")!
        case .messages:
            return URL(string: "sms:")!
        case .maps:
            return URL(string: "http://maps.apple.com/")!
        case .calendar:
            return URL(string: "calshow:")!
        case .settings:
            return URL(string: "app-settings:")!
        case .shortcuts:
            return URL(string: "shortcuts://")!
        case .phone:
            return URL(string: "tel:")!
        case .facetime:
            return URL(string: "facetime:")!
        case .music:
            return URL(string: "music://")!
        case .podcasts:
            return URL(string: "pcast://")!
        case .appStore:
            return URL(string: "itms-apps://")!
        case .whatsApp:
            return URL(string: "whatsapp://")!
        }
    }
}

public enum IOSVoiceCommandAction: Equatable {
    case cancelTranscript
    case copyTranscript
    case copyTranscriptAndPromptSend
    case openApp(IOSOpenTarget)
    case runShortcut(String)
}

struct IOSVoiceCommandResult: Equatable {
    var transcriptText: String
    var action: IOSVoiceCommandAction?
}

/// Static command catalog used by the "What can I say?" screen, the Shortcuts
/// descriptions, and the on-device parser smoke. Keeping the catalog in one
/// place means tests stay authoritative — extending the parser without adding
/// a row here is a defect.
struct IOSVoiceCommandExample: Identifiable, Equatable {
    enum Kind: Equatable {
        case tailWake(String)   // wake + verb at end of dictation, e.g. "zephyr stop"
        case standalone(String) // entire utterance IS the command
        case openApp            // "open <app>"
        case snippet            // user-defined snippet (placeholder)
    }

    let id: String
    let spoken: String
    let result: String
    let kind: Kind

    static let catalog: [IOSVoiceCommandExample] = {
        let wake = IOSVoiceCommandParser.wakeVariants.first ?? "zephyr"
        return [
            .init(id: "tail-stop", spoken: "remind me to call Maya \(wake) stop",
                  result: "Stops recording and copies the spoken text.", kind: .tailWake("stop")),
            .init(id: "tail-send", spoken: "release notes are live \(wake) send",
                  result: "Stops recording, copies, and prompts you to tap Send.", kind: .tailWake("send")),
            .init(id: "tail-cancel", spoken: "private note \(wake) cancel",
                  result: "Discards the in-progress recording without copying.", kind: .tailWake("cancel")),
            .init(id: "open-messages", spoken: "open messages",
                  result: "Opens the iOS Messages app.", kind: .openApp),
            .init(id: "open-mail", spoken: "open mail",
                  result: "Opens Mail.", kind: .openApp),
            .init(id: "open-maps", spoken: "open maps",
                  result: "Opens Maps.", kind: .openApp),
            .init(id: "open-calendar", spoken: "open calendar",
                  result: "Opens Calendar.", kind: .openApp),
            .init(id: "open-settings", spoken: "open settings",
                  result: "Opens HeardRight Settings.", kind: .openApp),
            .init(id: "open-shortcuts", spoken: "open shortcuts",
                  result: "Opens the Shortcuts app.", kind: .openApp),
            .init(id: "open-browser", spoken: "open browser",
                  result: "Opens Safari.", kind: .openApp),
            .init(id: "run-shortcut", spoken: "shortcut evening notes",
                  result: "Runs the named Apple Shortcut.", kind: .openApp),
            .init(id: "snippet", spoken: "say your snippet trigger",
                  result: "Your saved snippet expands into its replacement text.", kind: .snippet),
        ]
    }()
}

enum IOSVoiceCommandParser {
    /// Conservative spoken variants of the wake token. Matched only in the wake
    /// slot (start of a standalone command or immediately before an action verb),
    /// never as a free substring — keeps collisions with real words down.
    static let wakeVariants: [String] = [
        "zephyr", "zephir", "zephre", "zephur", "zeph", "zef", "zeff", "cepher", "zefur",
    ]

    /// Privacy-safe diagnostics: whether any wake variant appears anywhere in
    /// the decoded text. Lets dogfood traces separate "ASR heard the wake
    /// token but KWS/tail parsing missed it" from "ASR never heard it",
    /// without logging transcript content.
    static func containsWakeVariant(_ rawText: String) -> Bool {
        let lower = rawText.lowercased()
        return wakeVariants.contains { lower.contains($0) }
    }

    static func parse(_ rawText: String) -> IOSVoiceCommandResult {
        let normalized = normalizedCommandText(rawText)
        guard !normalized.isEmpty else {
            return IOSVoiceCommandResult(transcriptText: "", action: nil)
        }

        if let action = standaloneAction(for: normalized) {
            return IOSVoiceCommandResult(transcriptText: "", action: action)
        }

        if let tail = parseTailCommand(rawText) {
            return tail
        }

        return IOSVoiceCommandResult(transcriptText: rawText, action: nil)
    }

    private static func standaloneAction(for normalized: String) -> IOSVoiceCommandAction? {
        if let tokens = wakePrefixedTokens(normalized),
           let action = actionFor(tokens: tokens) {
            return action
        }

        switch normalized {
        case "cancel", "cancel dictation", "discard", "discard dictation":
            return .cancelTranscript
        case "stop", "copy", "copy that", "copy transcript", "copy latest", "copy latest transcript":
            return .copyTranscript
        case "send", "send it", "copy and send":
            return .copyTranscriptAndPromptSend
        default:
            break
        }

        if let name = shortcutName(in: normalized) {
            return .runShortcut(name)
        }

        let launchPrefixes = ["open ", "launch ", "start ", "run "]
        guard let prefix = launchPrefixes.first(where: normalized.hasPrefix) else { return nil }
        let targetName = String(normalized.dropFirst(prefix.count))
        guard let target = openTarget(named: targetName) else { return nil }
        return .openApp(target)
    }

    private static func parseTailCommand(_ rawText: String) -> IOSVoiceCommandResult? {
        let wakeGroup = wakeAlternationPattern()
        let patterns: [(String, IOSVoiceCommandAction)] = [
            (#"(?i)\b(?:\#(wakeGroup))\s+(?:cancel|discard)\s*[.!?]*\s*$"#, .cancelTranscript),
            (#"(?i)\b(?:\#(wakeGroup))\s+send\s*[.!?]*\s*$"#, .copyTranscriptAndPromptSend),
            (#"(?i)\b(?:\#(wakeGroup))\s+(?:stop|copy)\s*[.!?]*\s*$"#, .copyTranscript),
        ]

        for (pattern, action) in patterns {
            guard let range = rawText.range(of: pattern, options: .regularExpression) else { continue }
            let cleaned = String(rawText[..<range.lowerBound]).trimmingCharacters(in: .whitespacesAndNewlines)
            return IOSVoiceCommandResult(transcriptText: cleaned, action: action)
        }
        return nil
    }

    private static func wakeAlternationPattern() -> String {
        wakeVariants.joined(separator: "|")
    }

    private static func wakePrefixedTokens(_ normalized: String) -> [String]? {
        for variant in wakeVariants where normalized.hasPrefix("\(variant) ") {
            let tail = String(normalized.dropFirst(variant.count + 1))
            let tokens = tail.split(separator: " ").map(String.init)
            if !tokens.isEmpty { return tokens }
        }
        return nil
    }

    private static func actionFor(tokens: [String]) -> IOSVoiceCommandAction? {
        guard tokens.count <= 3 else { return nil }
        let verbs = tokens.joined(separator: " ")
        switch verbs {
        case "cancel", "cancel dictation", "discard", "discard dictation":
            return .cancelTranscript
        case "stop", "copy", "copy that", "copy transcript", "copy latest", "copy latest transcript":
            return .copyTranscript
        case "send", "send it", "copy and send":
            return .copyTranscriptAndPromptSend
        default:
            return nil
        }
    }

    private static func openTarget(named name: String) -> IOSOpenTarget? {
        let aliases: [(IOSOpenTarget, [String])] = [
            (.browser, ["safari", "browser", "web", "web browser"]),
            (.mail, ["mail", "email"]),
            (.messages, ["messages", "message", "text", "sms"]),
            (.maps, ["maps", "map", "apple maps"]),
            (.calendar, ["calendar", "cal"]),
            (.settings, ["settings", "ios settings", "heardright settings"]),
            (.shortcuts, ["shortcuts", "shortcut"]),
            (.phone, ["phone", "telephone", "dialer"]),
            (.facetime, ["facetime", "face time"]),
            (.music, ["music", "apple music"]),
            (.podcasts, ["podcasts", "podcast", "apple podcasts"]),
            (.appStore, ["app store", "apps store"]),
            (.whatsApp, ["whatsapp", "whats app", "what s app", "whats up", "what's up"]),
        ]
        if let exact = aliases.first(where: { $0.1.contains(name) }) {
            return exact.0
        }
        var bestTarget: IOSOpenTarget?
        var bestDistance = Int.max
        var bestLength = Int.max
        for entry in aliases {
            for alias in entry.1 {
                let distance = editDistance(name, alias)
                if distance < bestDistance || (distance == bestDistance && alias.count < bestLength) {
                    bestTarget = entry.0
                    bestDistance = distance
                    bestLength = alias.count
                }
            }
        }
        guard let bestTarget else { return nil }
        let tolerance = name.count >= 8 ? 2 : 1
        return bestDistance <= tolerance ? bestTarget : nil
    }

    private static func shortcutName(in normalized: String) -> String? {
        for prefix in ["shortcut ", "run shortcut ", "run the shortcut "] where normalized.hasPrefix(prefix) {
            let name = normalized.dropFirst(prefix.count).trimmingCharacters(in: .whitespacesAndNewlines)
            return name.isEmpty ? nil : name
        }
        return nil
    }

    static func shortcutURL(named name: String) -> URL? {
        var components = URLComponents()
        components.scheme = "shortcuts"
        components.host = "run-shortcut"
        components.queryItems = [URLQueryItem(name: "name", value: name)]
        return components.url
    }

    private static func editDistance(_ lhs: String, _ rhs: String) -> Int {
        let a = Array(lhs)
        let b = Array(rhs)
        if a.isEmpty { return b.count }
        if b.isEmpty { return a.count }
        var previous = Array(0...b.count)
        for (i, left) in a.enumerated() {
            var current = [i + 1] + Array(repeating: 0, count: b.count)
            for (j, right) in b.enumerated() {
                current[j + 1] = min(
                    current[j] + 1,
                    previous[j + 1] + 1,
                    previous[j] + (left == right ? 0 : 1)
                )
            }
            previous = current
        }
        return previous[b.count]
    }

    private static func normalizedCommandText(_ text: String) -> String {
        var tokens = text
            .lowercased()
            .replacingOccurrences(of: #"[^\p{L}\p{N}\s]"#, with: " ", options: .regularExpression)
            .split(whereSeparator: \.isWhitespace)
            .map(String.init)
        let fillers: Set<String> = ["and", "so", "okay", "ok", "please"]
        while tokens.first.isSomeAnd({ fillers.contains($0) }) { tokens.removeFirst() }
        while tokens.last.isSomeAnd({ fillers.contains($0) }) { tokens.removeLast() }
        return tokens.joined(separator: " ")
    }
}

private extension Optional {
    func isSomeAnd(_ predicate: (Wrapped) -> Bool) -> Bool {
        map(predicate) ?? false
    }
}
