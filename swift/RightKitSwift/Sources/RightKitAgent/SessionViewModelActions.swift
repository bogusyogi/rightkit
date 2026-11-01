#if os(iOS)
import Foundation

extension SessionViewModel {
    // MARK: - Actions

    public func send(text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !busy else { return }
        busy = true
        syncLiveActivity()
        defer {
            busy = false
            syncLiveActivity()
        }
        let nonce = UUID().uuidString
        pendingNonces.insert(nonce)
        msgs.append(TranscriptMessage(
            id: "u\(Int(Date().timeIntervalSince1970 * 1000))", role: .user, text: trimmed, nonce: nonce
        ))
        do {
            let receipt = try await client.sendMessage(
                session: sessionId, text: trimmed, nonce: nonce, workspaceRoot: nil
            )
            // No receipt means an ordinary session: the optimistic row stays exactly as before.
            guard let receipt else { return }
            recordDelivery(receipt, nonce: nonce)
            if receipt.state.isNotDelivered {
                pendingNonces.remove(nonce)
                reportDeliveryRefused(receipt.state)
            }
        } catch {
            // The optimistic row would otherwise sit in the transcript looking sent while the
            // daemon never saw it (silent data loss) — and the nonce would leak, so a later
            // retype of the same text could be dropped as a duplicate echo.
            pendingNonces.remove(nonce)
            reportFailure("Message failed to send", error)
        }
    }

    public func resolveToolApproval(toolCallId: String, approved: Bool) async {
        guard submitInteraction(
            id: toolCallId,
            kind: .toolApproval,
            action: approved ? .approve : .decline
        ) else { return }
        do {
            try await client.resolveToolApproval(
                session: sessionId, toolCallId: toolCallId, approved: approved
            )
            resolveInteraction(id: toolCallId, kind: .toolApproval, approved: approved)
        } catch {
            failInteraction(id: toolCallId, kind: .toolApproval, message: failureDetail(error))
            reportFailure("Couldn't \(approved ? "approve" : "skip") the tool call", error)
        }
    }

    public func answerQuestion(questionId: String, answers: [String]) async {
        guard submitInteraction(
            id: questionId,
            kind: .question,
            action: .answer(answers.joined(separator: "\n"))
        ) else { return }
        do {
            try await client.answerQuestion(session: sessionId, questionId: questionId, answers: answers)
            resolveInteraction(id: questionId, kind: .question, approved: true)
        } catch {
            failInteraction(id: questionId, kind: .question, message: failureDetail(error))
            reportFailure("Couldn't send the answer", error)
        }
    }

    public func resolvePlan(planId: String, approved: Bool) async {
        guard submitInteraction(id: planId, kind: .plan, action: approved ? .approve : .decline) else { return }
        do {
            try await client.resolvePlan(session: sessionId, planId: planId, approved: approved)
            resolveInteraction(id: planId, kind: .plan, approved: approved)
        } catch {
            failInteraction(id: planId, kind: .plan, message: failureDetail(error))
            reportFailure("Couldn't \(approved ? "approve" : "reject") the plan", error)
        }
    }

    public func interrupt() async {
        await run("Couldn't interrupt the run") { try await $0.interrupt(session: self.sessionId) }
    }

    public func steer(text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        await run("Couldn't send steering guidance") {
            try await $0.steer(session: self.sessionId, text: trimmed)
        }
    }

    public func setModel(_ newModel: String) async {
        let previous = model
        model = newModel
        do {
            try await client.setModel(session: sessionId, model: newModel)
        } catch {
            model = previous
            reportFailure("Couldn't switch the model", error)
        }
    }

    /// Actions used to `try?` their calls — tapping Approve with a dead connection silently did
    /// nothing and the card just sat there. Failures now land in the transcript as error rows.
    func run(_ failureTitle: String, _ action: (any DaemonClienting) async throws -> Void) async {
        do {
            try await action(client)
        } catch {
            reportFailure(failureTitle, error)
        }
    }

    func reportFailure(_ title: String, _ error: Error) {
        let detail = failureDetail(error)
        msgs.append(TranscriptMessage(
            id: "le\(Int(Date().timeIntervalSince1970 * 1000))",
            role: .error,
            text: "\(title) — \(detail). Check the connection and try again."
        ))
    }

    /// Attach a daemon delivery receipt to the optimistic user row that carries `nonce`.
    func recordDelivery(_ receipt: MessageDeliveryReceipt, nonce: String) {
        guard let index = msgs.lastIndex(where: { $0.nonce == nonce }) else { return }
        msgs[index].delivery = receipt
    }

    /// Error row for a message the daemon declined, worded like the existing send-failure row.
    func reportDeliveryRefused(_ state: MessageDeliveryReceipt.State) {
        let reason = state == .unsupported
            ? "this chat cannot take messages from CodeRight"
            : "the chat refused it"
        msgs.append(TranscriptMessage(
            id: "le\(Int(Date().timeIntervalSince1970 * 1000))", role: .error,
            text: "Message failed to send — \(reason)."
        ))
    }

    func failureDetail(_ error: Error) -> String {
        switch error {
        case DaemonError.http(let status, _): return "daemon returned HTTP \(status)"
        case DaemonError.transport(let message): return message
        case DaemonError.decoding: return "unexpected daemon response"
        default: return "connection failed"
        }
    }}
#endif
