import Foundation

/*
 Provenance (exact donor revision):
 - mjmkk/opencodex @ 1d34372bb51372b7cd61b716021654c9019dfb94
 - codex-worker-ios/Sources/CodexWorker/Models/Approval.swift:1-471
 - codex-worker-ios/Sources/CodexWorker/Features/ApprovalFeature/ApprovalFeature.swift:1-113

 Approval identity, risk classification, decision values, and submission
 lifecycle stay donor-shaped. CodeRight uses a pure reducer plus an outbound
 responder protocol, so this port does not import ComposableArchitecture,
 donor API clients, or CodeRight's shared networking/transcript types.
 */

public extension ToolInteractionsPort {
    // MARK: Approval model

    enum ApprovalKind: String, Codable, Sendable {
        case commandExecution = "command_execution"
        case fileChange = "file_change"

        public var iconName: String {
            switch self {
            case .commandExecution: return "terminal"
            case .fileChange: return "doc.text"
            }
        }

        public var title: String {
            switch self {
            case .commandExecution: return "Command execution approval"
            case .fileChange: return "File change approval"
            }
        }
    }

    /// Decisions accepted by the OpenCodex `/approve` endpoint. CodeRight's
    /// adapter maps these values to its daemon route without changing them.
    enum ApprovalDecision: String, Codable, Sendable {
        case accept
        case acceptForSession = "accept_for_session"
        case acceptWithExecpolicyAmendment = "accept_with_execpolicy_amendment"
        case decline
        case cancel

        public var displayText: String {
            switch self {
            case .accept: return "Approve"
            case .acceptForSession: return "Approve for session"
            case .acceptWithExecpolicyAmendment: return "Approve with change"
            case .decline: return "Decline"
            case .cancel: return "Cancel task"
            }
        }

        public var requiresAmendment: Bool {
            self == .acceptWithExecpolicyAmendment
        }
    }

    enum RiskLevel: Int, Codable, Sendable {
        case low = 0
        case medium = 1
        case high = 2

        public var label: String {
            switch self {
            case .low: return "Low risk"
            case .medium: return "Medium risk"
            case .high: return "High risk"
            }
        }

        public var description: String {
            switch self {
            case .low: return "Safe operation"
            case .medium: return "Review its impact"
            case .high: return "Confirm carefully"
            }
        }
    }

    /// Approval payload normalized from either camelCase or snake_case daemon
    /// fields. Optional metadata is retained so exact-effect presentation can
    /// be supplied by an adapter when the daemon provides it.
    struct Approval: Identifiable, Codable, Equatable, Sendable {
        public let approvalId: String
        public let jobId: String
        public let threadId: String
        public let turnId: String?
        public let itemId: String?
        public let kind: ApprovalKind
        public let requestMethod: String
        public let createdAt: String
        public var command: String?
        public var cwd: String?
        public var commandActions: [String]?
        public var reason: String?
        public var grantRoot: Bool?
        public var proposedExecpolicyAmendment: [String]?

        public var id: String { approvalId }

        public init(
            approvalId: String,
            jobId: String,
            threadId: String,
            turnId: String? = nil,
            itemId: String? = nil,
            kind: ApprovalKind,
            requestMethod: String,
            createdAt: String,
            command: String? = nil,
            cwd: String? = nil,
            commandActions: [String]? = nil,
            reason: String? = nil,
            grantRoot: Bool? = nil,
            proposedExecpolicyAmendment: [String]? = nil
        ) {
            self.approvalId = approvalId
            self.jobId = jobId
            self.threadId = threadId
            self.turnId = turnId
            self.itemId = itemId
            self.kind = kind
            self.requestMethod = requestMethod
            self.createdAt = createdAt
            self.command = command
            self.cwd = cwd
            self.commandActions = commandActions
            self.reason = reason
            self.grantRoot = grantRoot
            self.proposedExecpolicyAmendment = proposedExecpolicyAmendment
        }

        /// Builds an approval from a decoded JSON object without imposing a
        /// dependency on one daemon event decoder.
        public init?(payload: [String: Any], fallbackJobId: String) {
            guard
                let approvalId = Self.nonEmptyString(in: payload, keys: ["approvalId", "approval_id"]),
                let threadId = Self.nonEmptyString(in: payload, keys: ["threadId", "thread_id"]),
                let kindRaw = Self.nonEmptyString(in: payload, keys: ["kind"]),
                let kind = ApprovalKind(rawValue: kindRaw),
                let requestMethod = Self.nonEmptyString(
                    in: payload, keys: ["requestMethod", "request_method"]
                ),
                let createdAt = Self.nonEmptyString(
                    in: payload, keys: ["createdAt", "created_at"]
                )
            else {
                return nil
            }

            self.init(
                approvalId: approvalId,
                jobId: Self.nonEmptyString(in: payload, keys: ["jobId", "job_id"])
                    ?? fallbackJobId,
                threadId: threadId,
                turnId: Self.nonEmptyString(in: payload, keys: ["turnId", "turn_id"]),
                itemId: Self.nonEmptyString(in: payload, keys: ["itemId", "item_id"]),
                kind: kind,
                requestMethod: requestMethod,
                createdAt: createdAt,
                command: Self.nonEmptyString(in: payload, keys: ["command"]),
                cwd: Self.nonEmptyString(in: payload, keys: ["cwd"]),
                commandActions: Self.stringArray(
                    in: payload, keys: ["commandActions", "command_actions"]
                ),
                reason: Self.nonEmptyString(in: payload, keys: ["reason"]),
                grantRoot: Self.bool(in: payload, keys: ["grantRoot", "grant_root"]),
                proposedExecpolicyAmendment: Self.stringArray(
                    in: payload,
                    keys: ["proposedExecpolicyAmendment", "proposed_execpolicy_amendment"]
                )
            )
        }

        /// Donor-compatible command/path risk classification. This is display
        /// metadata only; the daemon remains the authority for authorization.
        public var riskLevel: RiskLevel {
            guard let command else { return .low }
            let lowerCommand = command.lowercased()

            let highRiskPatterns = [
                "rm -rf", "rm -r", "sudo ", "chmod 777", "dd if=", "> /dev/", "mkfs",
                "fdisk", "shutdown", "reboot", "init 0", "kill -9",
            ]
            if highRiskPatterns.contains(where: { lowerCommand.contains($0) }) {
                return .high
            }

            let mediumRiskPatterns = [
                "git push", "git push --force", "npm publish", "cargo publish", "pip upload",
                "docker push", "git reset --hard", "git clean -fd", ":(){ :|:& };:",
            ]
            if mediumRiskPatterns.contains(where: { lowerCommand.contains($0) }) {
                return .medium
            }

            if let cwd {
                let criticalPaths = ["/etc", "/usr", "/bin", "/sbin", "/System"]
                if criticalPaths.contains(where: { cwd.hasPrefix($0) }) {
                    return .high
                }
            }
            return .low
        }

        public var isHighRisk: Bool {
            riskLevel == .high
        }

        public var formattedCommand: String {
            guard let command else { return "" }
            return command.count > 200 ? String(command.prefix(200)) + "..." : command
        }

        private static func nonEmptyString(
            in payload: [String: Any], keys: [String]
        ) -> String? {
            for key in keys {
                guard let value = payload[key] as? String else { continue }
                let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty { return trimmed }
            }
            return nil
        }

        private static func bool(in payload: [String: Any], keys: [String]) -> Bool? {
            for key in keys {
                if let value = payload[key] as? Bool { return value }
                if let value = payload[key] as? NSNumber { return value.boolValue }
            }
            return nil
        }

        private static func stringArray(
            in payload: [String: Any], keys: [String]
        ) -> [String]? {
            for key in keys {
                guard let values = payload[key] as? [Any] else { continue }
                let result = values
                    .compactMap { $0 as? String }
                    .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .filter { !$0.isEmpty }
                if !result.isEmpty { return result }
            }
            return nil
        }
    }

    // MARK: Question and plan requests

    struct QuestionItem: Codable, Equatable, Sendable, Identifiable {
        public let id: String
        public let header: String?
        public let question: String

        public init(id: String, header: String? = nil, question: String) {
            self.id = id
            self.header = header
            self.question = question
        }
    }

    struct QuestionRequest: Equatable, Sendable, Identifiable {
        public let id: String
        public let questions: [QuestionItem]

        public init(id: String, questions: [QuestionItem]) {
            self.id = id
            self.questions = questions
        }
    }

    struct PlanRequest: Equatable, Sendable, Identifiable {
        public let id: String
        public let detail: String

        public init(id: String, detail: String) {
            self.id = id
            self.detail = detail
        }
    }

    enum InteractionKind: String, Codable, Sendable {
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

    enum PendingInteraction: Equatable, Sendable {
        case approval(Approval)
        case question(QuestionRequest)
        case plan(PlanRequest)

        public var id: String {
            switch self {
            case .approval(let approval): return approval.approvalId
            case .question(let request): return request.id
            case .plan(let request): return request.id
            }
        }

        public var kind: InteractionKind {
            switch self {
            case .approval: return .toolApproval
            case .question: return .question
            case .plan: return .plan
            }
        }
    }

    // MARK: Feature state and response seam

    enum InteractionStatus: Equatable, Codable, Sendable {
        case pending
        case submitting
        case submitted
        case resolved(approved: Bool)
        case failed(String)
        case stale

        public var label: String {
            switch self {
            case .pending: return "Needs response"
            case .submitting: return "Submitting"
            case .submitted: return "Submitted; waiting for daemon"
            case .resolved(let approved): return approved ? "Approved" : "Declined"
            case .failed: return "Submission failed"
            case .stale: return "No longer available"
            }
        }

        public var isTerminal: Bool {
            switch self {
            case .resolved, .stale: return true
            case .pending, .submitting, .submitted, .failed: return false
            }
        }
    }

    struct ApprovalResponse: Codable, Equatable, Sendable {
        public let approvalId: String
        public let status: String
        public let decision: String?

        public init(approvalId: String, status: String, decision: String? = nil) {
            self.approvalId = approvalId
            self.status = status
            self.decision = decision
        }
    }

    /// One outbound response, normalized across command approvals, questions,
    /// and plans. The daemon/client adapter owns endpoint selection.
    struct ApprovalResponseRequest: Equatable, Sendable {
        public let kind: InteractionKind
        public let id: String
        public let jobId: String?
        public let decision: ApprovalDecision?
        public let answers: [String]
        public let execPolicyAmendment: [String]?
        public let declineReason: String?

        public init(
            kind: InteractionKind,
            id: String,
            jobId: String? = nil,
            decision: ApprovalDecision? = nil,
            answers: [String] = [],
            execPolicyAmendment: [String]? = nil,
            declineReason: String? = nil
        ) {
            self.kind = kind
            self.id = id
            self.jobId = jobId
            self.decision = decision
            self.answers = answers
            self.execPolicyAmendment = execPolicyAmendment
            self.declineReason = declineReason
        }

        public static func approval(
            _ approval: Approval,
            decision: ApprovalDecision,
            declineReason: String?
        ) -> Self {
            Self(
                kind: .toolApproval,
                id: approval.approvalId,
                jobId: approval.jobId,
                decision: decision,
                execPolicyAmendment: nil,
                declineReason: decision == .decline ? declineReason : nil
            )
        }

        public static func question(_ request: QuestionRequest, answers: [String]) -> Self {
            Self(kind: .question, id: request.id, answers: answers)
        }

        public static func plan(_ request: PlanRequest, approved: Bool) -> Self {
            Self(
                kind: .plan,
                id: request.id,
                decision: approved ? .accept : .decline
            )
        }
    }

    enum ApprovalSubmissionResult: Equatable, Sendable {
        case success(ApprovalResponse)
        case failure(String)
    }

    /// Main-actor seam around an existing CodeRight client. The port never
    /// selects an endpoint or silently swallows an error.
    @MainActor
    protocol ApprovalResponseSending {
        func send(_ request: ApprovalResponseRequest) async throws -> ApprovalResponse
    }

    struct ApprovalState: Equatable, Sendable {
        public var current: PendingInteraction?
        public var isSubmitting: Bool
        public var errorMessage: String?
        public var submittedDecision: ApprovalDecision?
        public var declineReasonInput: String
        public var answerInput: String
        public var status: InteractionStatus

        public init() {
            current = nil
            isSubmitting = false
            errorMessage = nil
            submittedDecision = nil
            declineReasonInput = ""
            answerInput = ""
            status = .pending
        }

        public var isPresented: Bool {
            current != nil
        }

        public var currentApproval: Approval? {
            guard case .approval(let approval) = current else { return nil }
            return approval
        }

        public var currentQuestion: QuestionRequest? {
            guard case .question(let request) = current else { return nil }
            return request
        }

        public var currentPlan: PlanRequest? {
            guard case .plan(let request) = current else { return nil }
            return request
        }

        public var canSubmit: Bool {
            switch status {
            case .pending: return current != nil && !isSubmitting
            case .failed: return current != nil && !isSubmitting
            case .submitting, .submitted, .resolved, .stale: return false
            }
        }
    }

    enum ApprovalAction: Equatable, Sendable {
        case present(PendingInteraction)
        case dismiss
        case declineReasonChanged(String)
        case answerChanged(String)
        case submitTapped(ApprovalDecision)
        case submitQuestion([String])
        case submitPlan(Bool)
        case submitResponse(ApprovalSubmissionResult)
        case resolutionReceived(id: String, approved: Bool)
        case markStale(id: String)
        case clearError
    }

    /// Donor-shaped feature namespace for integrations that prefer
    /// `ApprovalFeature.State` / `ApprovalFeature.Action` names.
    enum ApprovalFeature {
        public typealias State = ApprovalState
        public typealias Action = ApprovalAction

        @discardableResult
        public static func reduce(
            _ state: inout State,
            _ action: Action
        ) -> ApprovalResponseRequest? {
            ApprovalReducer.reduce(&state, action)
        }
    }

    /// Pure equivalent of OpenCodex's `@Reducer` feature. A successful
    /// submission only ends the submit phase; the daemon resolution event
    /// remains authoritative for closing the visible request.
    enum ApprovalReducer {
        @discardableResult
        public static func reduce(
            _ state: inout ApprovalState,
            _ action: ApprovalAction
        ) -> ApprovalResponseRequest? {
            switch action {
            case .present(let interaction):
                state.current = interaction
                state.isSubmitting = false
                state.errorMessage = nil
                state.submittedDecision = nil
                state.declineReasonInput = ""
                state.answerInput = ""
                state.status = .pending
                return nil

            case .dismiss:
                state = ApprovalState()
                return nil

            case .declineReasonChanged(let value):
                state.declineReasonInput = value
                return nil

            case .answerChanged(let value):
                state.answerInput = value
                return nil

            case .submitTapped(let decision):
                guard case .approval(let approval) = state.current,
                      state.canSubmit else { return nil }
                let approvalId = approval.approvalId.trimmingCharacters(
                    in: .whitespacesAndNewlines
                )
                guard !approvalId.isEmpty else {
                    state.errorMessage = "Approval request is missing approvalId. Retry or refresh the thread."
                    state.status = .failed(state.errorMessage ?? "Invalid approval request")
                    return nil
                }
                state.isSubmitting = true
                state.errorMessage = nil
                state.submittedDecision = decision
                state.status = .submitting
                let declineReason = state.declineReasonInput.trimmingCharacters(
                    in: .whitespacesAndNewlines
                )
                return ApprovalResponseRequest(
                    kind: .toolApproval,
                    id: approvalId,
                    jobId: approval.jobId,
                    decision: decision,
                    // OpenCodex sends nil until its optional amendment editor
                    // is shown; preserving that source behavior keeps this
                    // port safe for ordinary approve/decline actions.
                    execPolicyAmendment: nil,
                    declineReason: decision == .decline && !declineReason.isEmpty
                        ? declineReason : nil
                )

            case .submitQuestion(let answers):
                guard case .question(let request) = state.current,
                      state.canSubmit else { return nil }
                let trimmed = answers.map {
                    $0.trimmingCharacters(in: .whitespacesAndNewlines)
                }
                guard trimmed.contains(where: { !$0.isEmpty }) else {
                    state.errorMessage = "Enter an answer before sending."
                    return nil
                }
                state.answerInput = trimmed.joined(separator: "\n")
                state.isSubmitting = true
                state.errorMessage = nil
                state.status = .submitting
                return .question(request, answers: trimmed)

            case .submitPlan(let approved):
                guard case .plan(let request) = state.current,
                      state.canSubmit else { return nil }
                state.isSubmitting = true
                state.errorMessage = nil
                state.submittedDecision = approved ? .accept : .decline
                state.status = .submitting
                return .plan(request, approved: approved)

            case .submitResponse(.success):
                // Resolution is delivered separately by `approval.resolved`,
                // so keep the request visible while ending network activity.
                state.isSubmitting = false
                state.status = .submitted
                return nil

            case .submitResponse(.failure(let message)):
                let detail = message.trimmingCharacters(in: .whitespacesAndNewlines)
                let resolved = detail.isEmpty ? "Unable to submit response" : detail
                state.isSubmitting = false
                state.errorMessage = resolved
                state.status = .failed(resolved)
                return nil

            case .resolutionReceived(let id, let approved):
                guard state.current?.id == id else { return nil }
                state.isSubmitting = false
                state.errorMessage = nil
                state.status = .resolved(approved: approved)
                return nil

            case .markStale(let id):
                guard state.current?.id == id else { return nil }
                state.isSubmitting = false
                state.status = .stale
                return nil

            case .clearError:
                state.errorMessage = nil
                if case .failed = state.status { state.status = .pending }
                return nil
            }
        }
    }
}
