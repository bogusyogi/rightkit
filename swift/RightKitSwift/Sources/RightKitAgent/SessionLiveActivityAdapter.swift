import Combine
import Foundation

@MainActor
final class SessionLiveActivityAdapter: CodeRightLiveActivityEventAdapter {
    private let sessionID: String
    private let responder: @MainActor (String, String) async -> Void
    private let subject = CurrentValueSubject<CodeRightLiveActivityState, Never>(.init())

    init(
        sessionID: String,
        responder: @escaping @MainActor (String, String) async -> Void
    ) {
        self.sessionID = sessionID
        self.responder = responder
    }

    var liveActivityState: AnyPublisher<CodeRightLiveActivityState, Never> {
        subject.eraseToAnyPublisher()
    }

    func publish(messages: [TranscriptMessage], busy: Bool) {
        let bot = CodeRightLiveActivityBot(
            id: "session-\(sessionID)",
            threadId: sessionID,
            name: "CodeRight",
            color: "coderight",
            face: busy ? "focused" : "idle"
        )

        if let interaction = messages.reversed().compactMap(\.interaction).first(where: {
            !$0.status.isTerminal
        }) {
            let options: [String]
            switch interaction.kind {
            case .toolApproval: options = ["Deny", "Approve"]
            case .plan: options = ["Reject", "Approve"]
            case .question: options = []
            }
            subject.send(.init(updates: [.init(
                bot: bot,
                kind: .needsYou,
                line: interaction.detail.isEmpty ? interaction.title : interaction.detail,
                card: .init(
                    isPending: true,
                    requestId: interaction.id,
                    options: options,
                    isPermission: interaction.kind == .toolApproval,
                    title: interaction.title
                )
            )]))
            return
        }

        let active = busy
            || messages.contains(where: { $0.streaming || $0.toolActivity?.status == .running })
        guard active else {
            subject.send(.init())
            return
        }
        subject.send(.init(updates: [.init(
            bot: bot,
            kind: .working,
            line: messages.last?.text ?? "Working"
        )]))
    }

    func answer(
        threadId: String,
        requestId: String,
        choice: String,
        isPermission: Bool
    ) async {
        guard threadId == sessionID else { return }
        await responder(requestId, choice)
    }
}
