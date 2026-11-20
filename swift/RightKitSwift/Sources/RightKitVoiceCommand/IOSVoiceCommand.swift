import Foundation

public enum IOSOpenTarget: String, Codable, CaseIterable, Equatable, Identifiable, Sendable {
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

    public var id: String { rawValue }

    public func url(browserURL: URL) -> URL {
        switch self {
        case .browser:
            return browserURL
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

public enum IOSVoiceCommandAction: Equatable, Sendable {
    case cancelTranscript
    case copyTranscript
    case copyTranscriptAndPromptSend
    case openApp(IOSOpenTarget)
    case runShortcut(String)
}

public struct IOSVoiceCommandResult: Equatable, Sendable {
    public var transcriptText: String
    public var action: IOSVoiceCommandAction?

    public init(transcriptText: String, action: IOSVoiceCommandAction?) {
        self.transcriptText = transcriptText
        self.action = action
    }
}

public struct IOSVoiceCommandParser: Sendable {
    /// Host supplies wake tokens & any product-specific aliases. Empty tokens are ignored.
    public let wakeVariants: [String]
    public let additionalAliases: [IOSOpenTarget: [String]]

    public init(wakeVariants: [String], additionalAliases: [IOSOpenTarget: [String]] = [:]) {
        self.wakeVariants = wakeVariants.map { $0.lowercased().trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        self.additionalAliases = additionalAliases.mapValues { aliases in
            aliases.map(Self.normalizedCommandText).filter { !$0.isEmpty }
        }
    }

    /// Privacy-safe diagnostics: whether any wake variant appears anywhere in
    /// the decoded text. Lets dogfood traces separate "ASR heard the wake
    /// token but KWS/tail parsing missed it" from "ASR never heard it",
    /// without logging transcript content.
    public func containsWakeVariant(_ rawText: String) -> Bool {
        let lower = rawText.lowercased()
        return wakeVariants.contains { lower.contains($0) }
    }

    public func parse(_ rawText: String) -> IOSVoiceCommandResult {
        let normalized = Self.normalizedCommandText(rawText)
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

    private func standaloneAction(for normalized: String) -> IOSVoiceCommandAction? {
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

    private func parseTailCommand(_ rawText: String) -> IOSVoiceCommandResult? {
        guard !wakeVariants.isEmpty else { return nil }
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

    private func wakeAlternationPattern() -> String {
        wakeVariants.map(NSRegularExpression.escapedPattern(for:)).joined(separator: "|")
    }

    private func wakePrefixedTokens(_ normalized: String) -> [String]? {
        for variant in wakeVariants where normalized.hasPrefix("\(variant) ") {
            let tail = String(normalized.dropFirst(variant.count + 1))
            let tokens = tail.split(separator: " ").map(String.init)
            if !tokens.isEmpty { return tokens }
        }
        return nil
    }

    private func actionFor(tokens: [String]) -> IOSVoiceCommandAction? {
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

    private func openTarget(named name: String) -> IOSOpenTarget? {
        let aliases: [(IOSOpenTarget, [String])] = [
            (.browser, ["safari", "browser", "web", "web browser"]),
            (.mail, ["mail", "email"]),
            (.messages, ["messages", "message", "text", "sms"]),
            (.maps, ["maps", "map", "apple maps"]),
            (.calendar, ["calendar", "cal"]),
            (.settings, ["settings", "ios settings"]),
            (.shortcuts, ["shortcuts", "shortcut"]),
            (.phone, ["phone", "telephone", "dialer"]),
            (.facetime, ["facetime", "face time"]),
            (.music, ["music", "apple music"]),
            (.podcasts, ["podcasts", "podcast", "apple podcasts"]),
            (.appStore, ["app store", "apps store"]),
            (.whatsApp, ["whatsapp", "whats app", "what s app", "whats up", "what's up"]),
        ]
        let configuredAliases = aliases.map { (target, names) in
            (target, names + (additionalAliases[target] ?? []))
        }
        if let exact = configuredAliases.first(where: { $0.1.contains(name) }) {
            return exact.0
        }
        var bestTarget: IOSOpenTarget?
        var bestDistance = Int.max
        var bestLength = Int.max
        for entry in configuredAliases {
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

    private func shortcutName(in normalized: String) -> String? {
        for prefix in ["shortcut ", "run shortcut ", "run the shortcut "] where normalized.hasPrefix(prefix) {
            let name = normalized.dropFirst(prefix.count).trimmingCharacters(in: .whitespacesAndNewlines)
            return name.isEmpty ? nil : name
        }
        return nil
    }

    public static func shortcutURL(named name: String) -> URL? {
        var components = URLComponents()
        components.scheme = "shortcuts"
        components.host = "run-shortcut"
        components.queryItems = [URLQueryItem(name: "name", value: name)]
        return components.url
    }

    private func editDistance(_ lhs: String, _ rhs: String) -> Int {
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
