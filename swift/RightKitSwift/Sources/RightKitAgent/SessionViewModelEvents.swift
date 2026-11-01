#if os(iOS)
import Foundation

extension SessionViewModel {
    // MARK: - applyEvents (pure, testable without a live network)

    func apply(_ events: [ProtocolEvent]) {
        for event in events {
            guard event.kind != "streamResyncRequired", event.seq > eventCursor else { continue }
            advanceEventCursor(to: event.seq)

            switch event.kind {
            case "assistantDelta":
                guard event.isReasoning != true else { continue }
                appendOrExtendStreamingAssistant(event)
            case "assistantMessageCompleted":
                finalizeStreamingOrAppend(event)
            case "toolCallStarted":
                if let call = event.call {
                    rememberTunnelToolCall(call)
                    appendDisplay(event, id: "t\(call.id)")
                }
            case "toolApprovalRequested":
                if let call = event.call {
                    appendDisplay(event, id: "ap\(call.id)")
                }
            case "toolApprovalResolved":
                markToolApprovalResolved(event)
            case "toolResultAdded":
                captureTunnelPreviewResult(event)
                if !completeLatestRunningTool(event) {
                    appendDisplay(event, id: "\(event.kind)-\(event.seq)")
                }
            case "userQuestionAnswered":
                if let questionId = event.questionId {
                    resolveInteraction(id: questionId, kind: .question, approved: true)
                }
            case "runFailed":
                failLatestRunningTool(event.error ?? event.hint ?? "Run failed", matching: event.toolCallId)
                appendDisplay(event, id: "e\(event.seq)")
            case "runInterrupted":
                failLatestRunningTool(event.reason ?? "Run interrupted", matching: event.toolCallId)
                appendDisplay(event, id: "i\(event.seq)")
            case "userMessageSubmitted":
                applyUserMessageSubmitted(event)
            case "usageUpdated":
                applyUsage(event)
            default:
                appendDisplay(event, id: "\(event.kind)-\(event.seq)")
            }
        }
        syncLiveActivity()
    }

    func appendOrExtendStreamingAssistant(_ event: ProtocolEvent) {
        let delta = event.text ?? ""
        if let last = msgs.last, last.role == .assistant, last.streaming {
            msgs[msgs.count - 1].text += delta
        } else {
            msgs.append(TranscriptMessage(id: "a\(event.seq)", role: .assistant, text: delta, streaming: true))
        }
    }

    func finalizeStreamingOrAppend(_ event: ProtocolEvent) {
        if let last = msgs.last, last.streaming {
            var updated = last
            if let content = event.message?.content, !content.isEmpty {
                updated.text = content
            }
            updated.streaming = false
            msgs[msgs.count - 1] = updated
        } else if let display = event.displayMessage() {
            msgs.append(TranscriptMessage(
                id: "a\(event.seq)", role: display.role, text: display.text, toolCallId: display.toolCallId
            ))
        }
    }

    func appendDisplay(_ event: ProtocolEvent, id: String) {
        guard let display = event.displayMessage() else { return }
        msgs.append(TranscriptMessage(
            id: id, role: display.role, text: display.text, toolCallId: display.toolCallId,
            diff: display.diff, toolActivity: display.toolActivity, interaction: display.interaction
        ))
    }

    func markToolApprovalResolved(_ event: ProtocolEvent) {
        guard let toolCallId = event.toolCallId else { return }
        let approved = event.approved ?? false
        resolveInteraction(id: toolCallId, kind: .toolApproval, approved: approved)
    }

    func completeLatestRunningTool(_ event: ProtocolEvent) -> Bool {
        guard let idx = runningToolIndex(matching: event.toolCallId), var activity = msgs[idx].toolActivity else {
            return false
        }
        let output = event.result?.content ?? event.result?.summary ?? "Tool result"
        if event.result?.isError == true {
            activity.apply(.failed(message: output, output: output))
        } else {
            activity.apply(.succeeded(output: output))
        }
        msgs[idx].toolActivity = activity
        return true
    }

    /// A remote runtime alone creates its tunnel. iOS receives only completed
    /// result content, validates it through TunnelPreviewResult, then lets its
    /// own WKWebView issue a direct public HTTPS request. No daemon browser,
    /// port-forward, or result-shaped URL is trusted outside this seam.
    func captureTunnelPreviewResult(_ event: ProtocolEvent) {
        guard let toolCallId = event.toolCallId,
              tunnelToolCallIDs.remove(toolCallId) != nil,
              let content = event.result?.content,
              TunnelPreviewResult.url(from: content) != nil
        else { return }
        tunnelPreviewResultContent = content
    }

    private func rememberTunnelToolCall(_ call: ProtocolEvent.ToolCall) {
        guard call.name == "tunnel" || call.name == "dev_tunnel_proxy" else { return }
        tunnelToolCallIDs.insert(call.id)
    }

    func failLatestRunningTool(_ message: String, matching toolCallId: String? = nil) {
        guard let idx = runningToolIndex(matching: toolCallId), var activity = msgs[idx].toolActivity else { return }
        activity.apply(.failed(message: message, output: ""))
        msgs[idx].toolActivity = activity
    }

    func runningToolIndex(matching toolCallId: String?) -> Int? {
        if let toolCallId {
            return msgs.indices.reversed().first(where: {
                msgs[$0].toolCallId == toolCallId && msgs[$0].toolActivity?.status == .running
            })
        }
        return msgs.indices.reversed().first(where: { msgs[$0].toolActivity?.status == .running })
    }

    @discardableResult
    func submitInteraction(
        id: String, kind: ApprovalInteractionKind, action: ApprovalInteractionAction
    ) -> Bool {
        var found = false
        var accepted = false
        for idx in msgs.indices where msgs[idx].toolCallId == id {
            found = true
            guard var interaction = msgs[idx].interaction, interaction.kind == kind else { continue }
            accepted = interaction.submit(action) || accepted
            msgs[idx].interaction = interaction
        }
        // Preserve endpoint behavior for callers resolving a request before its event card has
        // arrived; once a card exists, reject duplicate taps while it is submitting or resolved.
        return !found || accepted
    }

    func resolveInteraction(id: String, kind: ApprovalInteractionKind, approved: Bool) {
        for idx in msgs.indices where msgs[idx].toolCallId == id {
            guard var interaction = msgs[idx].interaction, interaction.kind == kind else { continue }
            _ = interaction.resolve(approved: approved)
            msgs[idx].interaction = interaction
        }
    }

    func failInteraction(id: String, kind: ApprovalInteractionKind, message: String) {
        for idx in msgs.indices where msgs[idx].toolCallId == id {
            guard var interaction = msgs[idx].interaction, interaction.kind == kind else { continue }
            interaction.fail(message)
            msgs[idx].interaction = interaction
        }
    }

    func applyUserMessageSubmitted(_ event: ProtocolEvent) {
        if let nonce = event.nonce, pendingNonces.contains(nonce) {
            pendingNonces.remove(nonce)
        } else {
            msgs.append(TranscriptMessage(id: "u\(event.seq)", role: .user, text: event.text ?? ""))
        }
    }

    func applyUsage(_ event: ProtocolEvent) {
        usage = UsageState(
            promptTokens: event.promptTokens ?? 0,
            completionTokens: event.completionTokens ?? 0,
            cacheReadTokens: event.cacheReadTokens ?? 0,
            cacheCreationTokens: event.cacheCreationTokens ?? 0,
            costUsd: event.costUsd ?? 0
        )
    }

}
#endif
