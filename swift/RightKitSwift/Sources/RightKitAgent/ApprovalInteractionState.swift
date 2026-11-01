import Foundation

public enum ApprovalInteractionKind: String, Equatable, Codable, Sendable {
    case toolApproval
    case question
    case plan

    public var label: String {
        switch self {
        case .toolApproval: return "Tool approval"
        case .question: return "Question"
        case .plan: return "Plan approval"
        }
    }
}

public enum ApprovalInteractionStatus: Equatable, Codable, Sendable {
    case pending
    case submitting
    case resolved(approved: Bool)
    case failed(String)
    case stale

    public var label: String {
        switch self {
        case .pending: return "Needs response"
        case .submitting: return "Submitting"
        case .resolved(let approved): return approved ? "Approved" : "Declined"
        case .failed: return "Submission failed"
        case .stale: return "No longer available"
        }
    }

    public var isTerminal: Bool {
        switch self {
        case .resolved, .stale: return true
        case .pending, .submitting, .failed: return false
        }
    }
}

public struct ApprovalQuestion: Equatable, Codable, Sendable, Identifiable {
    public let id: String
    public let header: String?
    public let question: String

    public init(id: String, header: String? = nil, question: String) {
        self.id = id
        self.header = header
        self.question = question
    }
}

public enum ApprovalInteractionAction: Equatable, Sendable {
    case approve
    case decline
    case answer(String)
}

/// State machine for an interactive transcript card. Submission is deliberately one-way until
/// the daemon responds, preventing duplicate taps while keeping failure retryable.
public struct ApprovalInteractionState: Equatable, Codable, Sendable, Identifiable {
    public let id: String
    public let kind: ApprovalInteractionKind
    public let title: String
    public let detail: String
    public let risk: String?
    public let questions: [ApprovalQuestion]
    /// The exact concrete effect this approval authorizes — a literal file path or
    /// shell command, not a tool name. ENG-28: an approval binds to *this* effect
    /// only; when the daemon cannot supply one (e.g. a tool with no path/command
    /// argument), this stays nil and the card falls back to `detail` alone rather
    /// than inventing a target.
    public let effectTarget: String?
    public var status: ApprovalInteractionStatus

    public init(
        id: String,
        kind: ApprovalInteractionKind,
        title: String,
        detail: String = "",
        risk: String? = nil,
        questions: [ApprovalQuestion] = [],
        effectTarget: String? = nil,
        status: ApprovalInteractionStatus = .pending
    ) {
        self.id = id
        self.kind = kind
        self.title = title
        self.detail = detail
        self.risk = risk
        self.questions = questions
        self.effectTarget = effectTarget
        self.status = status
    }

    public static func toolApproval(
        id: String, title: String, risk: String?, detail: String?, effectTarget: String? = nil
    ) -> Self {
        Self(
            id: id,
            kind: .toolApproval,
            title: title,
            detail: detail ?? "",
            risk: risk,
            effectTarget: effectTarget
        )
    }

    public static func question(id: String, questions: [ProtocolEvent.QuestionItem]) -> Self {
        Self(
            id: id,
            kind: .question,
            title: "Answer required",
            detail: questions.map { item in
                if let header = item.header { return "\(header): \(item.question)" }
                return item.question
            }.joined(separator: "\n"),
            questions: questions.enumerated().map { index, item in
                ApprovalQuestion(id: "\(id)-\(index)", header: item.header, question: item.question)
            }
        )
    }

    public static func plan(id: String, detail: String) -> Self {
        Self(id: id, kind: .plan, title: "Plan approval", detail: detail)
    }

    public static func resolved(
        kind: ApprovalInteractionKind, id: String, approved: Bool
    ) -> Self {
        Self(
            id: id,
            kind: kind,
            title: kind.label,
            status: .resolved(approved: approved)
        )
    }

    public var canSubmit: Bool {
        if status == .pending { return true }
        if case .failed = status { return true }
        return false
    }

    @discardableResult
    public mutating func submit(_ action: ApprovalInteractionAction) -> Bool {
        guard canSubmit else { return false }
        if case .answer(let answer) = action,
           answer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return false
        }
        switch (kind, action) {
        case (.toolApproval, .approve), (.toolApproval, .decline),
             (.plan, .approve), (.plan, .decline), (.question, .answer):
            status = .submitting
            return true
        default:
            return false
        }
    }

    /// Applies daemon resolution only while this request is still current. Stale or duplicate
    /// responses are ignored so an older request cannot overwrite a newer visible card.
    @discardableResult
    public mutating func resolve(approved: Bool) -> Bool {
        guard status == .pending || status == .submitting else { return false }
        status = .resolved(approved: approved)
        return true
    }

    public mutating func fail(_ message: String) {
        guard !status.isTerminal else { return }
        status = .failed(message.isEmpty ? "Unable to submit response" : message)
    }

    public mutating func markStale() {
        guard !status.isTerminal else { return }
        status = .stale
    }
}
