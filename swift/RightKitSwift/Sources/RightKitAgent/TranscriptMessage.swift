import Foundation

/// UI message role, mirroring the PWA's `Role` union.
public enum MessageRole: String, Sendable {
    case user, assistant, tool, approval, error
}

/// One prior body a message carried before it was edited or retried (atom 30). Kept as a
/// snapshot rather than a diff against the current body so version navigation never has to
/// replay a chain to render an arbitrary past version.
public struct TranscriptMessageVersion: Identifiable, Equatable, Sendable {
    /// Why this snapshot was captured: the user edited the message, or the app retried it
    /// (e.g. re-ran a failed assistant turn). Purely informational for the UI's version label.
    public enum Reason: String, Sendable, Equatable {
        case edit
        case retry
    }

    public let id: String
    public let text: String
    public let diff: String?
    public let reason: Reason
    public let createdAt: Date

    public init(
        id: String = UUID().uuidString,
        text: String,
        diff: String? = nil,
        reason: Reason = .edit,
        createdAt: Date = Date()
    ) {
        self.id = id
        self.text = text
        self.diff = diff
        self.reason = reason
        self.createdAt = createdAt
    }
}

/// A rendered transcript row. The stream applies events into an ordered list of these,
/// mirroring `SessionContext.applyEvents` in the PWA.
public struct TranscriptMessage: Identifiable, Equatable, Sendable {
    public let id: String
    public var role: MessageRole
    public var text: String
    public var toolCallId: String?
    public var streaming: Bool
    public var nonce: String?
    /// Unified-diff payload carried separately from `text` so the UI can render it as a real
    /// diff (colored +/- rows) instead of a text blob. Populated from `proposedChange` on
    /// tool-approval events when the payload actually parses as a unified diff.
    public var diff: String?
    /// Optional compact tool projection. Kept separate from text so tool details remain inert
    /// plain text and can be expanded without changing transcript semantics.
    public var toolActivity: ToolActivity?
    /// Optional blocking interaction projection for approvals, questions, and plans.
    public var interaction: ApprovalInteractionState?
    /// Prior bodies this message carried before an edit or retry replaced them (atom 30),
    /// oldest first. Empty for a message that has never been edited or retried — the default
    /// keeps every existing call site (positional or labeled, omitting this parameter) source
    /// compatible.
    public var priorVersions: [TranscriptMessageVersion]
    /// True when this row marks the point where an auto-compaction folded everything before it
    /// out of the live context (atom 52). Purely a UI marker — it carries no content of its
    /// own and does not change how the row's `text`/`role` are interpreted.
    public var isCompactionBoundary: Bool
    /// Daemon delivery receipt for an outgoing user message. Nil for ordinary sessions and every other row.
    public var delivery: MessageDeliveryReceipt? = nil

    public init(
        id: String, role: MessageRole, text: String,
        toolCallId: String? = nil, streaming: Bool = false, nonce: String? = nil,
        diff: String? = nil,
        toolActivity: ToolActivity? = nil,
        interaction: ApprovalInteractionState? = nil,
        priorVersions: [TranscriptMessageVersion] = [],
        isCompactionBoundary: Bool = false
    ) {
        self.id = id
        self.role = role
        self.text = text
        self.toolCallId = toolCallId
        self.streaming = streaming
        self.nonce = nonce
        self.diff = diff
        self.toolActivity = toolActivity ?? transcriptToolProjection(
            role: role, text: text, toolCallId: toolCallId
        )
        self.interaction = interaction ?? transcriptInteractionProjection(
            id: id, role: role, text: text, toolCallId: toolCallId
        )
        self.priorVersions = priorVersions
        self.isCompactionBoundary = isCompactionBoundary
    }

    /// True once this message has been edited or retried at least once.
    public var hasVersionHistory: Bool { !priorVersions.isEmpty }

    /// Snapshot the current body into `priorVersions`, then replace it with `newText`/`newDiff`.
    /// Used by both edit (user rewrites their own message) and retry (app re-runs a turn and
    /// replaces its result) — `reason` is the only thing that differs between the two callers.
    public mutating func pushVersion(
        newText: String, newDiff: String? = nil, reason: TranscriptMessageVersion.Reason
    ) {
        priorVersions.append(TranscriptMessageVersion(text: text, diff: diff, reason: reason, createdAt: Date()))
        text = newText
        diff = newDiff
    }
}

/// A display projection of a single event, mirroring `messageFromProtocolEvent`.
public struct DisplayEventMessage: Sendable {
    public let role: MessageRole
    public let text: String
    public let toolCallId: String?
    public var diff: String?
    public var toolActivity: ToolActivity?
    public var interaction: ApprovalInteractionState?

    public init(
        role: MessageRole,
        text: String,
        toolCallId: String?,
        diff: String? = nil,
        toolActivity: ToolActivity? = nil,
        interaction: ApprovalInteractionState? = nil
    ) {
        self.role = role
        self.text = text
        self.toolCallId = toolCallId
        self.diff = diff
        self.toolActivity = toolActivity
        self.interaction = interaction
    }
}

/// The daemon's tool previews (`tool_preview` → `unified_diff`) emit standard unified diffs.
/// Other tools may preview as prose — only treat the payload as a diff when it looks like one.
public func looksLikeUnifiedDiff(_ text: String) -> Bool {
    text.hasPrefix("--- ") || text.hasPrefix("+++ ")
        || text.contains("\n@@ ") || text.hasPrefix("@@ ")
}

private func transcriptToolProjection(
    role: MessageRole, text: String, toolCallId: String?
) -> ToolActivity? {
    guard role == .tool, let toolCallId else { return nil }
    let resolved = text.hasPrefix("\u{2713}") || text.hasPrefix("\u{2717}")
    let skipped = text.hasPrefix("\u{2717}")
    let name = text.trimmingCharacters(in: CharacterSet(charactersIn: "\u{2713}\u{2717} "))
    return ToolActivity(
        id: toolCallId,
        name: name,
        status: resolved ? (skipped ? .failed("Tool was skipped") : .succeeded) : .running
    )
}

private func transcriptInteractionProjection(
    id: String, role: MessageRole, text: String, toolCallId: String?
) -> ApprovalInteractionState? {
    guard role == .approval, let toolCallId else { return nil }
    if id.hasPrefix("ap") {
        return .toolApproval(id: toolCallId, title: text, risk: nil, detail: "")
    }
    if id.hasPrefix("userQuestionAsked-") {
        return ApprovalInteractionState(
            id: toolCallId,
            kind: .question,
            title: "Answer required",
            detail: text,
            questions: [ApprovalQuestion(id: "\(toolCallId)-0", question: text)]
        )
    }
    if id.hasPrefix("planPresented-") {
        return .plan(id: toolCallId, detail: text)
    }
    return nil
}

extension ProtocolEvent {
    // Port of `messageFromProtocolEvent` from the PWA. Returns the row this event should
    // append to the transcript, or `nil` for events consumed by streaming/usage handling.
    // A flat mapping table over every protocol event kind — its length is the number of
    // kinds, not accidental complexity; splitting it would only scatter the contract.
    // swiftlint:disable:next function_body_length
    public func displayMessage() -> DisplayEventMessage? {
        switch kind {
        case "sessionStarted", "sessionTitleUpdated", "sessionContextFolded",
             "selfReviewProposalReceived", "assistantDelta", "usageUpdated",
             "dispatchDecided", "modelRouteDecision", "userMessageSubmitted":
            return nil
        case "assistantMessageCompleted":
            guard let content = message?.content, !content.isEmpty else { return nil }
            return DisplayEventMessage(role: .assistant, text: content, toolCallId: nil)
        case "toolCallStarted":
            guard let call else { return nil }
            return DisplayEventMessage(
                role: .tool,
                text: call.name,
                toolCallId: call.id,
                toolActivity: ToolActivity(
                    id: call.id,
                    name: call.name,
                    input: call.argumentsJson ?? "",
                    status: .running
                )
            )
        case "toolApprovalRequested":
            guard let call else { return nil }
            // A diff-shaped preview rides in `diff` so the card renders it as a real diff;
            // prose previews stay appended to the text as before.
            if let change = proposedChange, looksLikeUnifiedDiff(change) {
                return DisplayEventMessage(
                    role: .approval,
                    text: "\(call.name) (\(riskLabel ?? "risk"))",
                    toolCallId: call.id,
                    diff: change,
                    interaction: ApprovalInteractionState.toolApproval(
                        id: call.id,
                        title: call.name,
                        risk: riskLabel,
                        detail: change
                    )
                )
            }
            let change = proposedChange.map { "\n\($0)" } ?? ""
            return DisplayEventMessage(
                role: .approval,
                text: "\(call.name) (\(riskLabel ?? "risk"))\(change)",
                toolCallId: call.id,
                interaction: ApprovalInteractionState.toolApproval(
                    id: call.id,
                    title: call.name,
                    risk: riskLabel,
                    detail: proposedChange ?? call.argumentsJson
                )
            )
        case "toolApprovalResolved":
            let ok = approved ?? false
            return DisplayEventMessage(
                role: .tool,
                text: "\(ok ? "Approved" : "Skipped") tool call \(toolCallId ?? "")",
                toolCallId: toolCallId,
                interaction: toolCallId.map {
                    ApprovalInteractionState.resolved(
                        kind: .toolApproval, id: $0, approved: ok
                    )
                }
            )
        case "toolResultAdded":
            return DisplayEventMessage(
                role: result?.isError == true ? .error : .tool,
                text: result?.content ?? result?.summary ?? "Tool result",
                toolCallId: toolCallId
            )
        case "verificationFinished":
            let ok = passed ?? false
            return DisplayEventMessage(
                role: ok ? .tool : .error,
                text: "\(ok ? "Verification passed" : "Verification failed"): \(summary ?? "")",
                toolCallId: nil
            )
        case "sessionCompacted":
            return DisplayEventMessage(role: .tool, text: "Session compacted: \(summary ?? "")", toolCallId: nil)
        case "runInterrupted":
            return DisplayEventMessage(
                role: .error,
                text: "Interrupted: \(reason ?? "")".trimmingCharacters(in: .whitespaces),
                toolCallId: nil
            )
        case "runFailed":
            let parts = [error, hint].compactMap { $0 }.filter { !$0.isEmpty }
            return DisplayEventMessage(role: .error, text: parts.joined(separator: "\n"), toolCallId: nil)
        case "budgetExceeded":
            let scopeText = scope.map { " (\($0))" } ?? ""
            let hintText = hint.map { "\n\($0)" } ?? ""
            return DisplayEventMessage(
                role: .error,
                text: String(
                    format: "Budget exceeded%@: $%.4f of $%.4f%@",
                    scopeText, currentUsd ?? 0, ceilingUsd ?? 0, hintText
                ),
                toolCallId: nil
            )
        case "subAgentSpawned":
            return DisplayEventMessage(
                role: .tool,
                text: "Sub-agent started: \(role ?? "") — \(taskTitle ?? "")",
                toolCallId: subSessionId
            )
        case "subAgentCompleted":
            let ok = success ?? false
            return DisplayEventMessage(
                role: ok ? .tool : .error,
                text: "Sub-agent \(role ?? "") completed: \(summary ?? "")",
                toolCallId: subSessionId
            )
        case "subAgentCancelled":
            return DisplayEventMessage(
                role: .error,
                text: "Sub-agent \(role ?? "") cancelled by \(cancelledBy ?? "")",
                toolCallId: subSessionId
            )
        case "userQuestionAsked":
            let joined = (questions ?? []).map { item -> String in
                if let h = item.header { return "\(h): \(item.question)" }
                return item.question
            }.joined(separator: "\n")
            return DisplayEventMessage(
                role: .approval,
                text: joined,
                toolCallId: questionId,
                interaction: questionId.map {
                    ApprovalInteractionState.question(id: $0, questions: questions ?? [])
                }
            )
        case "userQuestionAnswered":
            return DisplayEventMessage(
                role: .tool,
                text: "Question answered: \(questionId ?? "")",
                toolCallId: questionId,
                interaction: questionId.map {
                    ApprovalInteractionState.resolved(kind: .question, id: $0, approved: true)
                }
            )
        case "planPresented":
            return DisplayEventMessage(
                role: .approval,
                text: plan ?? "",
                toolCallId: planId,
                interaction: planId.map {
                    ApprovalInteractionState.plan(id: $0, detail: plan ?? "")
                }
            )
        case "checkRunCompleted":
            let ok = passed ?? false
            return DisplayEventMessage(
                role: ok ? .tool : .error,
                text: "Check \(ok ? "passed" : "failed"): \(command ?? "")",
                toolCallId: nil
            )
        case "goalProgressUpdated":
            return DisplayEventMessage(
                role: .tool,
                text: "Goal \(status ?? ""): \(tokensUsed ?? 0) token(s) used",
                toolCallId: goalId
            )
        case "orchestrationObservationRecorded":
            guard let observation else { return nil }
            let label: String
            switch observation.kind {
            case "deferred_tool_resolution":
                label = "Deferred work resolved"
            case "async_inbox_message":
                label = "Async inbox message"
            default:
                label = "Runtime observation"
            }
            let detail = observation.summary.isEmpty ? observation.kind : observation.summary
            return DisplayEventMessage(
                role: .tool,
                text: "\(label): \(detail)",
                toolCallId: observation.fields["toolCallId"]
            )
        default:
            return nil
        }
    }
}
