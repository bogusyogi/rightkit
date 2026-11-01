#if os(iOS)
// Provenance: direct behavioral transplant from Bot commit
// 6951c40156ba0916a10b6e49e5051b7785198b93, ios/Shared/BotActivity.swift.
// CodeRight-specific names are isolated here; field semantics stay donor-compatible.

import ActivityKit
import AppIntents
import Foundation

/// The Live Activity contract compiled into both CodeRight app and widget targets.
/// One activity represents one bot/session that is working or waiting for a response.
public struct BotActivityAttributes: ActivityAttributes, Sendable {
    public struct ContentState: Codable, Hashable, Sendable {
        /// Adapter-provided face state. The raw value remains portable across targets.
        public var face: String
        /// "needsYou" | "working" | "toReview".
        public var kind: String
        /// "Scout needs you", "Forge is working".
        public var headline: String
        /// The question or current work description.
        public var line: String
        /// Pending interaction identifier, when kind == "needsYou".
        public var requestId: String?
        /// Options offered by the pending interaction.
        public var options: [String]
        /// True when a pending interaction is a permission allow/deny card.
        public var isPermission: Bool
        /// When this kind began; drives the island timer and progress ring.
        public var since: Date

        public init(
            face: String,
            kind: String,
            headline: String,
            line: String,
            requestId: String? = nil,
            options: [String] = [],
            isPermission: Bool = false,
            since: Date = Date()
        ) {
            self.face = face
            self.kind = kind
            self.headline = headline
            self.line = line
            self.requestId = requestId
            self.options = options
            self.isPermission = isPermission
            self.since = since
        }
    }

    public var botId: String
    public var threadId: String
    public var name: String
    /// Adapter-provided CodeRight palette key or hex value.
    public var color: String

    public init(botId: String, threadId: String, name: String, color: String) {
        self.botId = botId
        self.threadId = threadId
        self.name = name
        self.color = color
    }
}

/// Answers from Dynamic Island or lock screen. The app wires `handler` at launch;
/// the widget extension never performs a network request itself.
public struct AnswerApprovalIntent: LiveActivityIntent {
    public static let title: LocalizedStringResource = "Answer"
    public static let isDiscoverable = false

    @Parameter(title: "Thread") public var threadId: String
    @Parameter(title: "Request") public var requestId: String
    @Parameter(title: "Choice") public var choice: String
    @Parameter(title: "Permission") public var isPermission: Bool

    public init() {}

    public init(threadId: String, requestId: String, choice: String, isPermission: Bool) {
        self.threadId = threadId
        self.requestId = requestId
        self.choice = choice
        self.isPermission = isPermission
    }

    /// Set by the app at launch. It is nil in the widget extension.
    nonisolated(unsafe) public static var handler:
        ((_ threadId: String, _ requestId: String, _ choice: String, _ isPermission: Bool) async -> Void)?

    public func perform() async throws -> some IntentResult {
        await Self.handler?(threadId, requestId, choice, isPermission)
        return .result()
    }
}
#endif
