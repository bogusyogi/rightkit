import Foundation

/*
 Provenance (read-only donor): Bot @ 6951c40156ba0916a10b6e49e5051b7785198b93.
 - `ios/Sources/CompanionCore/Models.swift:292-301` — `TranscriptExport` bytes,
   filename, and content type.
 - `ios/Sources/CompanionCore/Client.swift:622-645` — `CompanionClient.export`,
   `format=markdown|json`, response filename, and content type.
 - `ios/App/Session.swift:1008-1019` — temporary-directory atomic write and URL
   returned for sharing.
 - `ios/App/ChatView.swift:498-517` — Markdown/JSON share actions.
 - `ios/App/ChatView.swift:830-843` — `ShareFile` identity and native activity
   sheet.

 CodeRight adaptation: local session state enters through
 `TranscriptExportInput`; no view, daemon client, or networking type is
 required by this port.
*/

public enum TranscriptExportFormat: String, Codable, Sendable {
    case markdown
    case json

    public var fileExtension: String {
        switch self {
        case .markdown: return "md"
        case .json: return "json"
        }
    }

    /// Matches response content types from the donor export route.
    public var contentType: String {
        switch self {
        case .markdown: return "text/markdown; charset=utf-8"
        case .json: return "application/json"
        }
    }
}

/// The smallest snapshot a session screen must provide to use this port.
/// `sessionID` is emitted as donor `threadId`; messages are already reduced to
/// donor export fields so image bytes never enter a share artifact.
public protocol TranscriptExportInput {
    var transcriptExportSessionID: String { get }
    var transcriptExportTitle: String { get }
    var transcriptExportUserName: String? { get }
    var transcriptExportAssistantName: String? { get }
    var transcriptExportMessages: [TranscriptExportMessage] { get }
}

public extension TranscriptExportInput {
    var transcriptExportUserName: String? { nil }
    var transcriptExportAssistantName: String? { nil }
}

/// Donor-compatible message projection. `png` and `mime` intentionally do not
/// exist: Bot's JSON export strips those screen pixels before encoding.
public struct TranscriptExportMessage: Codable, Equatable, Sendable {
    public enum Kind: String, Codable, Sendable {
        case text
        case options
        case activity
        case screen
        case unknown
    }

    public enum Role: String, Codable, Sendable {
        case bot
        case user
    }

    public var id: String
    public var role: Role
    public var kind: Kind
    /// Milliseconds since Unix epoch, matching donor `Message.at`.
    public var at: Double
    public var text: String?
    public var card: TranscriptExportCard?
    public var tool: TranscriptExportTool?
    public var parentId: String?
    public var from: TranscriptExportSender?
    public var reactions: [TranscriptExportReaction]?
    public var comm: TranscriptExportCommChip?
    public var hasImage: Bool?

    public init(
        id: String,
        role: Role,
        kind: Kind,
        at: Double = 0,
        text: String? = nil,
        card: TranscriptExportCard? = nil,
        tool: TranscriptExportTool? = nil,
        parentId: String? = nil,
        from: TranscriptExportSender? = nil,
        reactions: [TranscriptExportReaction]? = nil,
        comm: TranscriptExportCommChip? = nil,
        hasImage: Bool? = nil
    ) {
        self.id = id
        self.role = role
        self.kind = kind
        self.at = at
        self.text = text
        self.card = card
        self.tool = tool
        self.parentId = parentId
        self.from = from
        self.reactions = reactions
        self.comm = comm
        self.hasImage = hasImage
    }
}

public struct TranscriptExportCard: Codable, Equatable, Sendable {
    public var title: String
    public var subtitle: String
    public var options: [String]
    public var answered: String?
    public var dismissed: Bool?
    public var requestId: String?
    public var tool: String?
    public var held: String?
    public var allowKey: String?

    public init(
        title: String,
        subtitle: String = "",
        options: [String] = [],
        answered: String? = nil,
        dismissed: Bool? = nil,
        requestId: String? = nil,
        tool: String? = nil,
        held: String? = nil,
        allowKey: String? = nil
    ) {
        self.title = title
        self.subtitle = subtitle
        self.options = options
        self.answered = answered
        self.dismissed = dismissed
        self.requestId = requestId
        self.tool = tool
        self.held = held
        self.allowKey = allowKey
    }
}

public struct TranscriptExportTool: Codable, Equatable, Sendable {
    public var name: String
    public var ok: Bool?
    public var spoken: String?
    public var setup: Bool?

    public init(name: String, ok: Bool? = nil, spoken: String? = nil, setup: Bool? = nil) {
        self.name = name
        self.ok = ok
        self.spoken = spoken
        self.setup = setup
    }
}

public struct TranscriptExportSender: Codable, Equatable, Sendable {
    public var botId: String
    public var name: String
    public var color: String

    public init(botId: String, name: String, color: String) {
        self.botId = botId
        self.name = name
        self.color = color
    }
}

public struct TranscriptExportReaction: Codable, Equatable, Sendable {
    public var emoji: String
    public var by: String

    public init(emoji: String, by: String) {
        self.emoji = emoji
        self.by = by
    }
}

public struct TranscriptExportCommChip: Codable, Equatable, Sendable {
    public var groupId: String
    public var withBotId: String
    public var withName: String
    public var withColor: String

    public init(groupId: String, withBotId: String, withName: String, withColor: String) {
        self.groupId = groupId
        self.withBotId = withBotId
        self.withName = withName
        self.withColor = withColor
    }
}

public struct TranscriptExportArtifact: Sendable {
    public var data: Data
    public var filename: String
    public var contentType: String

    public init(data: Data, filename: String, contentType: String) {
        self.data = data
        self.filename = filename
        self.contentType = contentType
    }
}

/// Formats the visible branch exactly like Bot's export route.
public enum TranscriptExportFormatter {
    public static func artifact(
        from input: any TranscriptExportInput,
        format: TranscriptExportFormat
    ) throws -> TranscriptExportArtifact {
        let title = normalizedTitle(input.transcriptExportTitle)
        let data: Data
        switch format {
        case .markdown:
            data = Data(markdown(from: input, title: title).utf8)
        case .json:
            let envelope = ExportEnvelope(
                name: title,
                threadId: input.transcriptExportSessionID,
                messages: input.transcriptExportMessages
            )
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted]
            data = try encoder.encode(envelope)
        }

        return TranscriptExportArtifact(
            data: data,
            filename: filename(title: title, format: format),
            contentType: format.contentType
        )
    }

    public static func markdown(
        from input: any TranscriptExportInput
    ) -> String {
        markdown(from: input, title: normalizedTitle(input.transcriptExportTitle))
    }

    public static func jsonData(
        from input: any TranscriptExportInput
    ) throws -> Data {
        try artifact(from: input, format: .json).data
    }

    /// Donor `title.replace(/[^\w\- ]+/g, "").trim().slice(0, 60)`.
    public static func filename(
        title: String,
        format: TranscriptExportFormat
    ) -> String {
        let filtered = String.UnicodeScalarView(title.unicodeScalars.filter { scalar in
            (scalar.value >= 48 && scalar.value <= 57)
                || (scalar.value >= 65 && scalar.value <= 90)
                || (scalar.value >= 97 && scalar.value <= 122)
                || scalar == "_"
                || scalar == "-"
                || scalar == " "
        })
        let stem = String(filtered).trimmingCharacters(in: .whitespacesAndNewlines)
        let safeStem = String((stem.isEmpty ? "conversation" : stem).prefix(60))
        return "\(safeStem).\(format.fileExtension)"
    }

    private static func markdown(
        from input: any TranscriptExportInput,
        title: String
    ) -> String {
        let userName = nonEmpty(input.transcriptExportUserName) ?? "User"
        let assistantName = nonEmpty(input.transcriptExportAssistantName)
        var lines = ["# \(title)", ""]

        for message in input.transcriptExportMessages {
            let who = message.role == .user
                ? userName
                : (nonEmpty(message.from?.name) ?? assistantName ?? "Bot")
            switch message.kind {
            case .text:
                guard let text = message.text, !text.isEmpty else { continue }
                lines += ["**\(who):**", "", text, ""]
            case .activity:
                guard let tool = message.tool else { continue }
                lines += ["> \(tool.name)", ""]
            case .screen:
                lines += ["> [screen capture]", ""]
            case .options:
                guard let card = message.card else { continue }
                let answer = nonEmpty(card.answered).map { " — answered: \($0)" } ?? ""
                lines += ["> \(card.title)\(answer)", ""]
            case .unknown:
                continue
            }
        }

        return lines.joined(separator: "\n")
    }

    private static func normalizedTitle(_ title: String) -> String {
        nonEmpty(title) ?? "conversation"
    }

    private static func nonEmpty(_ value: String?) -> String? {
        guard let value else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private struct ExportEnvelope: Encodable {
        let name: String
        let threadId: String
        let messages: [TranscriptExportMessage]
    }
}
