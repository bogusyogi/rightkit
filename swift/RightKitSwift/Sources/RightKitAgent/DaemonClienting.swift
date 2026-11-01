import Foundation

/// Errors surfaced by the daemon client. Views map these to user-facing state.
public enum DaemonError: Error, Sendable, Equatable {
    case notConfigured
    case http(status: Int, body: String)
    case transport(String)
    case decoding(String)
    case cancelled
}

/// Optional `delivery` object on a `POST /sessions/{id}/messages` response. Unrecognised state
/// strings decode to `.unknown`, so a newer daemon never fails the send.
public struct MessageDeliveryReceipt: Codable, Equatable, Sendable {
    public enum State: String, Codable, Equatable, Sendable {
        case delivered, queued, sent, held, refused, unsupported, unknown

        /// Short label shown under the sent user message in the transcript.
        public var statusText: String {
            switch self {
            case .delivered: return "Received by chat"
            case .queued, .sent: return "Waiting"
            case .held, .unknown: return "Delivery uncertain"
            case .refused, .unsupported: return "Not delivered"
            }
        }

        /// True when the daemon declined to deliver the message at all.
        public var isNotDelivered: Bool {
            self == .refused || self == .unsupported
        }
    }

    public let state: State
    public let msgId: String?

    public init(state: State, msgId: String? = nil) {
        self.state = state
        self.msgId = msgId
    }

    enum CodingKeys: String, CodingKey {
        case state, msgId
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let raw = (try? c.decode(String.self, forKey: .state)) ?? ""
        state = State(rawValue: raw) ?? .unknown
        msgId = try? c.decodeIfPresent(String.self, forKey: .msgId)
    }
}

/// The full remote-control surface the iOS client needs from a CodeRight daemon.
///
/// This protocol is the PINNED CONTRACT between the networking layer (which implements it,
/// `DaemonClient`) and the view models (which consume `any DaemonClienting`). Method
/// signatures here are frozen — do not change them without updating both sides. Endpoints map
/// 1:1 to the daemon HTTP API used by the reference PWA (apps/coderight-web).
public protocol DaemonClienting: Sendable {
    /// `GET /health` — returns true when the daemon answers 2xx with the current token.
    func health() async -> Bool

    /// `GET /sessions`
    func listSessions() async throws -> [SessionSummary]

    /// `GET /settings/orchestration` — model list + primary model.
    func orchestrationSettings() async throws -> OrchestrationSettings

    /// `GET /sessions/{id}/events/history` — backfill prior events for a freshly opened session.
    /// Returns events in ascending `seq`.
    func eventHistory(session id: String) async throws -> [ProtocolEvent]

    /// `GET /sessions/{id}/events?after={seq}` as a live SSE stream. Each element is one parsed
    /// event. The stream ends when the task is cancelled or the connection drops; callers own
    /// reconnection (resubscribe with the last seen `seq`).
    func events(session id: String, after seq: Int) -> AsyncThrowingStream<ProtocolEvent, Error>

    /// `POST /sessions/{id}/messages` — body `{text, nonce, workspaceRoot?}`. Returns the optional
    /// delivery receipt; nil for ordinary sessions, which send no `delivery` object.
    func sendMessage(
        session id: String, text: String, nonce: String, workspaceRoot: String?
    ) async throws -> MessageDeliveryReceipt?

    /// `POST /sessions/{id}/tool-approval` — body `{toolCallId, approved}`.
    func resolveToolApproval(session id: String, toolCallId: String, approved: Bool) async throws

    /// `POST /sessions/{id}/answer-question` — body `{questionId, answers}`.
    func answerQuestion(session id: String, questionId: String, answers: [String]) async throws

    /// `POST /sessions/{id}/resolve-plan` — body `{planId, approved}`.
    func resolvePlan(session id: String, planId: String, approved: Bool) async throws

    /// `POST /sessions/{id}/interrupt`
    func interrupt(session id: String) async throws

    /// `POST /sessions/{id}/steer` — body `{text}`. Injects mid-run guidance.
    func steer(session id: String, text: String) async throws

    /// `PATCH /sessions/{id}` — body `{model}`.
    func setModel(session id: String, model: String) async throws

    /// `POST /sessions` — body `{workspaceRoot, title}`. Returns the created session's id.
    func createSession(title: String, workspaceRoot: String) async throws -> String

    /// `DELETE /sessions/{id}`
    func deleteSession(id: String) async throws
}

/// Decouples the Storage/Views layers from the concrete `DaemonClient` type. The Networking
/// module installs the builder at launch (`DaemonClientFactory.make = { DaemonClient(config: $0) }`);
/// everyone else constructs clients as `any DaemonClienting` through `make(config)` and so compiles
/// against only this protocol, never the concrete implementation.
public enum DaemonClientFactory {
    nonisolated(unsafe) public static var make: (@Sendable (DaemonConfig) -> any DaemonClienting)?

    public static func client(for config: DaemonConfig) -> (any DaemonClienting)? {
        make?(config)
    }
}
