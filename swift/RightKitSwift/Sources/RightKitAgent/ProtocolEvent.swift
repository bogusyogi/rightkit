import Foundation

/// One event from the daemon SSE stream `GET /sessions/{id}/events?after={seq}`.
///
/// This is a faithful Swift port of the PWA's `sessionEventFormat.ts` — a discriminated
/// union keyed on `$kind`. Rather than an enum with ~35 cases, it decodes leniently into a
/// flat struct of optional fields, matching how the reference client switches on the string
/// kind. Unknown kinds decode cleanly and simply produce no display message, so the client
/// never breaks when the daemon adds an event type.
public struct ProtocolEvent: Decodable, Sendable {
    public struct ToolCall: Decodable, Sendable {
        public let id: String
        public let name: String
        public let argumentsJson: String?
    }

    public struct TranscriptLikeMessage: Decodable, Sendable {
        public let content: String?
        public let summary: String?
        public let toolCallId: String?
        public let isError: Bool?
    }

    public struct QuestionItem: Decodable, Sendable {
        public let question: String
        public let header: String?
    }

    /// Structured lifecycle note emitted by shared engine paths. Deferred
    /// resolution & local inbox production both use this stable protocol
    /// shape, so iOS renders those events rather than silently dropping them.
    public struct OrchestrationObservation: Decodable, Sendable {
        public let id: String
        public let kind: String
        public let subject: String
        public let summary: String
        public let fields: [String: String]
    }

    public let kind: String
    public let seq: Int
    public let sessionId: String?

    // Streaming / content
    public let text: String?
    public let isReasoning: Bool?
    public let message: TranscriptLikeMessage?
    public let result: TranscriptLikeMessage?
    public let title: String?
    public let nonce: String?

    // Tool + approval
    public let call: ToolCall?
    public let riskLabel: String?
    public let proposedChange: String?
    public let toolCallId: String?
    public let approved: Bool?

    // Questions / plans
    public let questionId: String?
    public let questions: [QuestionItem]?
    public let planId: String?
    public let plan: String?

    // Verification / checks
    public let summary: String?
    public let passed: Bool?
    public let command: String?

    // Usage / budget
    public let promptTokens: Int?
    public let completionTokens: Int?
    public let cacheReadTokens: Int?
    public let cacheCreationTokens: Int?
    public let costUsd: Double?
    public let scope: String?
    public let currentUsd: Double?
    public let ceilingUsd: Double?
    public let hint: String?
    public let error: String?
    public let reason: String?

    // Sub-agents / goals
    public let subSessionId: String?
    public let role: String?
    public let taskTitle: String?
    public let success: Bool?
    public let cancelledBy: String?
    public let goalId: String?
    public let status: String?
    public let tokensUsed: Int?

    // Engine lifecycle / async delivery
    public let observation: OrchestrationObservation?

    enum CodingKeys: String, CodingKey {
        case kind = "$kind"
        case seq, sessionId, text, isReasoning, message, result, title, nonce
        case call, riskLabel, proposedChange, toolCallId, approved
        case questionId, questions, planId, plan
        case summary, passed, command
        case promptTokens, completionTokens, cacheReadTokens, cacheCreationTokens, costUsd
        case scope, currentUsd, ceilingUsd, hint, error, reason
        case subSessionId, role, taskTitle, success, cancelledBy, goalId, status, tokensUsed
        case observation
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = (try? c.decode(String.self, forKey: .kind)) ?? "unknown"
        seq = (try? c.decode(Int.self, forKey: .seq)) ?? 0
        sessionId = try? c.decodeIfPresent(String.self, forKey: .sessionId)
        text = try? c.decodeIfPresent(String.self, forKey: .text)
        isReasoning = try? c.decodeIfPresent(Bool.self, forKey: .isReasoning)
        message = try? c.decodeIfPresent(TranscriptLikeMessage.self, forKey: .message)
        result = try? c.decodeIfPresent(TranscriptLikeMessage.self, forKey: .result)
        title = try? c.decodeIfPresent(String.self, forKey: .title)
        nonce = try? c.decodeIfPresent(String.self, forKey: .nonce)
        call = try? c.decodeIfPresent(ToolCall.self, forKey: .call)
        riskLabel = try? c.decodeIfPresent(String.self, forKey: .riskLabel)
        proposedChange = try? c.decodeIfPresent(String.self, forKey: .proposedChange)
        toolCallId = (try? c.decodeIfPresent(String.self, forKey: .toolCallId)) ?? result?.toolCallId
        approved = try? c.decodeIfPresent(Bool.self, forKey: .approved)
        questionId = try? c.decodeIfPresent(String.self, forKey: .questionId)
        questions = try? c.decodeIfPresent([QuestionItem].self, forKey: .questions)
        planId = try? c.decodeIfPresent(String.self, forKey: .planId)
        plan = try? c.decodeIfPresent(String.self, forKey: .plan)
        summary = try? c.decodeIfPresent(String.self, forKey: .summary)
        passed = try? c.decodeIfPresent(Bool.self, forKey: .passed)
        command = try? c.decodeIfPresent(String.self, forKey: .command)
        promptTokens = try? c.decodeIfPresent(Int.self, forKey: .promptTokens)
        completionTokens = try? c.decodeIfPresent(Int.self, forKey: .completionTokens)
        cacheReadTokens = try? c.decodeIfPresent(Int.self, forKey: .cacheReadTokens)
        cacheCreationTokens = try? c.decodeIfPresent(Int.self, forKey: .cacheCreationTokens)
        costUsd = try? c.decodeIfPresent(Double.self, forKey: .costUsd)
        scope = try? c.decodeIfPresent(String.self, forKey: .scope)
        currentUsd = try? c.decodeIfPresent(Double.self, forKey: .currentUsd)
        ceilingUsd = try? c.decodeIfPresent(Double.self, forKey: .ceilingUsd)
        hint = try? c.decodeIfPresent(String.self, forKey: .hint)
        error = try? c.decodeIfPresent(String.self, forKey: .error)
        reason = try? c.decodeIfPresent(String.self, forKey: .reason)
        subSessionId = try? c.decodeIfPresent(String.self, forKey: .subSessionId)
        role = try? c.decodeIfPresent(String.self, forKey: .role)
        taskTitle = try? c.decodeIfPresent(String.self, forKey: .taskTitle)
        success = try? c.decodeIfPresent(Bool.self, forKey: .success)
        cancelledBy = try? c.decodeIfPresent(String.self, forKey: .cancelledBy)
        goalId = try? c.decodeIfPresent(String.self, forKey: .goalId)
        status = try? c.decodeIfPresent(String.self, forKey: .status)
        tokensUsed = try? c.decodeIfPresent(Int.self, forKey: .tokensUsed)
        observation = try? c.decodeIfPresent(OrchestrationObservation.self, forKey: .observation)
    }
}
