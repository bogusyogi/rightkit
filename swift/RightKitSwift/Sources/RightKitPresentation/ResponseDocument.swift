import Foundation

/// A response ready to display: the canonical text it came from plus ordered display blocks.
/// Display only: no command, URL to fetch, action or approval payload lives here.
public struct ResponseDocument: Sendable, Equatable {
    public let id: String
    public let revision: UInt64
    public let canonicalText: String
    public let blocks: [ResponseBlock]

    public init(id: String, revision: UInt64, canonicalText: String, blocks: [ResponseBlock]) {
        self.id = id
        self.revision = revision
        self.canonicalText = canonicalText
        self.blocks = blocks
    }
}

public struct ResponseBlock: Sendable, Equatable {
    public let id: String
    /// Plain text equivalent, for hosts or cases that cannot draw `content` faithfully.
    public let fallbackText: String
    public let content: ResponseContent

    public init(id: String, fallbackText: String, content: ResponseContent) {
        self.id = id
        self.fallbackText = fallbackText
        self.content = content
    }
}

public enum ResponseContent: Sendable, Equatable {
    case text(TextDocument)
    case rows([DisplayRow])
    case notice(DisplayNotice)
    case receipt(DisplayReceipt)
}

public struct DisplayRow: Sendable, Equatable, Identifiable {
    public let id: String
    public let title: String
    public let detail: String?
    public let status: String?

    public init(id: String, title: String, detail: String?, status: String?) {
        self.id = id
        self.title = title
        self.detail = detail
        self.status = status
    }
}

public struct DisplayTable: Sendable, Equatable {
    public let headers: [String]
    public let rows: [[String]]

    public init(headers: [String], rows: [[String]]) {
        self.headers = headers
        self.rows = rows
    }
}

public struct DisplayNotice: Sendable, Equatable {
    public enum Tone: Sendable, Equatable { case info, warning, problem }
    public let tone: Tone
    public let message: String

    public init(tone: Tone, message: String) {
        self.tone = tone
        self.message = message
    }
}

public struct DisplayReceipt: Sendable, Equatable {
    public let operation: String
    public let destination: String
    public let state: String
    public let detail: String?

    public init(operation: String, destination: String, state: String, detail: String?) {
        self.operation = operation
        self.destination = destination
        self.state = state
        self.detail = detail
    }
}
