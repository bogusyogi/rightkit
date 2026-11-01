import Foundation

/*
 Provenance (exact): openmausbot commit
 `6951c40156ba0916a10b6e49e5051b7785198b93`:
 - `ios/App/Session.swift:24-31` — explicit unpaired/connecting/live/
   unauthorized/offline state machine.
 - `ios/App/Session.swift:414-480` — stream ownership, intentional
   cancellation, retry, and failure transitions.
 - `ios/Sources/CompanionCore/Store.swift:317-345` — replay-safe cursor
   advancement and duplicate-frame tolerance.

 This file only stages the seams. It does not alter CodeRight's pinned daemon
 contract or connect a session view to the coordinator.
*/

/// The lifecycle transport needed by the openmausbot session state machine.
///
/// CodeRight's daemon uses an integer `seq` cursor rather than openmausbot's
/// opaque `<streamId>:<seq>` cursor. Keeping this adapter narrow lets the
/// reconnect loop preserve the same resume semantics without importing the
/// donor's connection, fleet, or message model.
public protocol SessionLifecycleTransport: Sendable {
    func eventHistory(session id: String) async throws -> [ProtocolEvent]
    func events(session id: String, after sequence: Int) -> AsyncThrowingStream<ProtocolEvent, Error>
}

/// Adapter from CodeRight's frozen daemon client contract to the isolated
/// lifecycle port. The coordinator depends on this protocol, so a local
/// runtime or a test transport can be supplied without changing the state
/// machine.
public struct DaemonSessionLifecycleTransport: SessionLifecycleTransport, Sendable {
    private let client: any DaemonClienting

    public init(client: any DaemonClienting) {
        self.client = client
    }

    public func eventHistory(session id: String) async throws -> [ProtocolEvent] {
        try await client.eventHistory(session: id)
    }

    public func events(session id: String, after sequence: Int) -> AsyncThrowingStream<ProtocolEvent, Error> {
        client.events(session: id, after: sequence)
    }
}

/// Explicit lifecycle states keep an unavailable daemon distinct from an
/// invalid credential, an unpaired phone, and an intentional stop.
public enum SessionLifecycleStatus: Equatable, Sendable {
    case unpaired
    case connecting
    case live
    case unauthorized
    case offline(String)
    case cancelled

    public var isLive: Bool {
        self == .live
    }

    public var isTerminalCredentialFailure: Bool {
        self == .unauthorized
    }

    public var message: String? {
        switch self {
        case let .offline(message): return message
        case .unauthorized: return "The daemon rejected this token. Pair again in Settings."
        case .unpaired, .connecting, .live, .cancelled: return nil
        }
    }
}

/// Observable-at-the-seam state. A host view model can copy this into its own
/// Observation model without giving the port ownership of transcript state.
public struct SessionLifecycleSnapshot: Equatable, Sendable {
    public let sessionID: String
    public let status: SessionLifecycleStatus
    public let eventCursor: Int
    public let retryDelay: TimeInterval
    public let streamActive: Bool
    public let backgroundLingerActive: Bool
    public let externalSessionNotRunning: Bool

    public init(
        sessionID: String,
        status: SessionLifecycleStatus,
        eventCursor: Int,
        retryDelay: TimeInterval,
        streamActive: Bool,
        backgroundLingerActive: Bool,
        externalSessionNotRunning: Bool = false
    ) {
        self.sessionID = sessionID
        self.status = status
        self.eventCursor = eventCursor
        self.retryDelay = retryDelay
        self.streamActive = streamActive
        self.backgroundLingerActive = backgroundLingerActive
        self.externalSessionNotRunning = externalSessionNotRunning
    }
}

/// Keep callback ownership at the integration boundary. The coordinator only
/// emits accepted, cursor-advancing events; the host decides how to fold them.
public typealias SessionLifecycleEventHandler = @MainActor ([ProtocolEvent]) -> Void
public typealias SessionLifecycleStateHandler = @MainActor (SessionLifecycleSnapshot) -> Void

/// The scene transitions the app root must forward to a lifecycle coordinator.
/// `.inactive` deliberately carries no action: openmausbot only changes stream
/// ownership on active/background edges.
public enum SessionLifecycleScenePhase: Sendable {
    case active
    case inactive
    case background
}

/// Adapter seam for `CodeRightApp`'s `scenePhase` callback. Keeping this
/// protocol local avoids wiring the port to SwiftUI or to app-root globals.
@MainActor
public protocol SessionLifecycleSceneHandling: AnyObject {
    func sessionLifecycleDidBecomeActive()
    func sessionLifecycleDidEnterBackground()
}
