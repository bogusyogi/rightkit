#if os(iOS)
import Foundation
import UIKit

/*
 Provenance (exact): openmausbot commit
 `6951c40156ba0916a10b6e49e5051b7785198b93`,
 `ios/App/Session.swift:294-365` (`connect`, `refresh`, and stream restart),
 `ios/App/Session.swift:367-412` (`disconnect`, bounded background linger),
 and `ios/App/Session.swift:414-480` (stream loop, intentional cancellation,
 status transitions, and 1/2/4/8…15-second retry).
 `ios/Sources/CompanionCore/SSE.swift:82-181` supplies the related stream
 ownership rule: one task retains URLSession bytes until completion, and
 cancellation belongs to the lifecycle owner rather than the parser.

 CodeRight's `GET /sessions/:id/events?after=N` has no donor `hello` frame, so
 the first successful history request is the donor's cold hydrate boundary and
 each accepted ProtocolEvent advances the integer cursor. The state machine
 remains isolated here until an integration owner wires it to a view model.
*/

/// Timing values retained from openmausbot's iOS lifecycle implementation.
public enum SessionLifecycleTiming {
    public static let retryInitial: TimeInterval = 1
    public static let retryMaximum: TimeInterval = 15
    public static let backgroundLingerMaximum: TimeInterval = 25
    public static let refreshTimeout: TimeInterval = 10

    public static func boundedBackgroundLinger(_ requested: TimeInterval) -> TimeInterval {
        min(max(0, requested), backgroundLingerMaximum)
    }

    public static func nextRetryDelay(after current: TimeInterval) -> TimeInterval {
        guard current > 0 else { return retryInitial }
        return min(current * 2, retryMaximum)
    }
}

/// User-facing mapping for transport failures. Unauthorized is handled as a
/// terminal state by the coordinator before this mapper is used.
public enum SessionLifecycleFailure {
    public static func message(for error: Error) -> String {
        switch error {
        case DaemonError.http(let status, _):
            return "The daemon returned HTTP \(status)."
        case DaemonError.transport(let message):
            return message.isEmpty ? "The network is unavailable." : message
        case DaemonError.decoding:
            return "The daemon sent an invalid response."
        case DaemonError.notConfigured:
            return "No daemon is configured."
        case DaemonError.cancelled:
            return "The connection was cancelled."
        case let urlError as URLError:
            return urlError.localizedDescription.isEmpty
                ? "The network is unavailable."
                : urlError.localizedDescription
        default:
            return error.localizedDescription.isEmpty
                ? "The network is unavailable."
                : error.localizedDescription
        }
    }

    public static func isUnauthorized(_ error: Error) -> Bool {
        guard case DaemonError.http(let status, _) = error else { return false }
        return status == 401 || status == 403
    }

    public static func isCancellation(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        if case DaemonError.cancelled = error { return true }
        return false
    }

    /// Shown when an external Claude chat's process is not running. History stays readable.
    public static let externalSessionNotRunningMessage = "Open this chat on the computer to continue."

    /// The SSE route answers HTTP 409 `{"error":"external_session_not_running"}` when the external
    /// chat's process is gone. Retrying cannot help, so the coordinator stops instead of backing off.
    public static func isExternalSessionNotRunning(_ error: Error) -> Bool {
        guard case DaemonError.http(let status, let body) = error, status == 409 else { return false }
        return body.contains("external_session_not_running")
    }
}

/// Main-actor lifecycle coordinator ready to be attached to a CodeRight
/// `SessionViewModel` through the event/state handlers in `SessionLifecyclePort`.
/// It owns stream effects and cursor/status state, but never owns transcript
/// projection, pairing, or navigation.
@MainActor
public final class SessionLifecycleCoordinator: SessionLifecycleSceneHandling {
    public let sessionID: String

    public private(set) var status: SessionLifecycleStatus = .unpaired
    public private(set) var eventCursor = 0
    /// True after the stream was refused with `external_session_not_running`. Cleared by any new connection.
    public private(set) var externalSessionNotRunning = false

    private let transport: any SessionLifecycleTransport
    private let onEvents: SessionLifecycleEventHandler
    private let onStateChange: SessionLifecycleStateHandler
    private let backgroundLingerDuration: TimeInterval

    /// The generation prevents a cancelled stream's late cleanup from clearing
    /// the replacement stream, matching openmausbot's `streamGeneration` guard.
    private var streamGeneration = 0
    private var streamTask: Task<Void, Never>?
    private var retryDelay: TimeInterval = 0
    private var historyLoaded = false

    /// The iOS assertion and its sleeper are kept as a pair. Ending either
    /// side ends both, so an expired assertion cannot cancel a later linger.
    private var backgroundTask = UIBackgroundTaskIdentifier.invalid
    private var lingerSleep: Task<Void, Never>?

    public init(
        sessionID: String,
        transport: any SessionLifecycleTransport,
        backgroundLingerDuration: TimeInterval = SessionLifecycleTiming.backgroundLingerMaximum,
        onEvents: @escaping SessionLifecycleEventHandler = { _ in },
        onStateChange: @escaping SessionLifecycleStateHandler = { _ in }
    ) {
        self.sessionID = sessionID
        self.transport = transport
        self.backgroundLingerDuration = SessionLifecycleTiming.boundedBackgroundLinger(
            backgroundLingerDuration
        )
        self.onEvents = onEvents
        self.onStateChange = onStateChange
        emitState()
    }

    deinit {
        lingerSleep?.cancel()
    }

    public var snapshot: SessionLifecycleSnapshot {
        SessionLifecycleSnapshot(
            sessionID: sessionID,
            status: status,
            eventCursor: eventCursor,
            retryDelay: retryDelay,
            streamActive: streamTask != nil,
            backgroundLingerActive: backgroundTask != .invalid || lingerSleep != nil,
            externalSessionNotRunning: externalSessionNotRunning
        )
    }

    // MARK: - Lifecycle

    /// Open one stream. Calling this while a stream is live is idempotent; an
    /// explicit `reconnect()` is required to replace an existing stream.
    public func connect() {
        openConnection(allowUnauthorized: false)
    }

    /// Explicit user recovery. This is the one path allowed to retry after a
    /// terminal authorization failure; passive foreground recovery stays
    /// terminal until the user asks for it.
    public func reconnect() {
        cancelStream(incrementGeneration: true)
        endLinger()
        openConnection(allowUnauthorized: true)
    }

    /// Reopen the stream and wait until it leaves `.connecting`, bounded like
    /// openmausbot's pull-to-refresh path so a dead network cannot trap a UI
    /// refresh control indefinitely.
    public func refresh() async {
        reconnect()
        let deadline = Date().addingTimeInterval(SessionLifecycleTiming.refreshTimeout)
        while status == .connecting, !Task.isCancelled, Date() < deadline {
            try? await Task.sleep(for: .milliseconds(120))
        }
    }

    /// Intentional teardown. It does not manufacture an offline error: the
    /// caller chose to stop, and the next `connect()` can reuse the cursor.
    public func disconnect() {
        cancelStream(incrementGeneration: true)
        endLinger()
        emitState()
    }

    /// Explicit stop state for a host that is being disposed. This is kept
    /// separate from `disconnect()` so background teardown can preserve the
    /// donor's visible status until it either reconnects or expires.
    public func stop() {
        cancelStream(incrementGeneration: true)
        endLinger()
        setStatus(.cancelled)
    }

    /// Called by the app root on a foreground transition. A pending linger is
    /// ended first; an existing stream is reused, while an expired stream is
    /// reopened from the last accepted sequence.
    public func sessionLifecycleDidBecomeActive() {
        endLinger()
        guard streamTask == nil, status != .unauthorized, status != .cancelled, !externalSessionNotRunning else {
            emitState()
            return
        }
        connect()
    }

    /// Called by the app root on a background transition. The stream remains
    /// alive for iOS's bounded grace period so late events still arrive; the
    /// expiry path cancels it cleanly and leaves a truthful offline state.
    public func sessionLifecycleDidEnterBackground() {
        guard streamTask != nil else { return }
        linger()
    }

    /// Convenience for integrations that already have a scene-phase enum.
    public func scenePhaseDidChange(_ phase: SessionLifecycleScenePhase) {
        switch phase {
        case .active:
            sessionLifecycleDidBecomeActive()
        case .background:
            sessionLifecycleDidEnterBackground()
        case .inactive:
            break
        }
    }

    /// Begin an iOS background assertion and the donor's 25-second bounded
    /// sleeper. Calling this twice never creates a second assertion.
    public func linger() {
        guard streamTask != nil, backgroundTask == .invalid else {
            disconnect()
            return
        }

        // A stale sleeper must never outlive the assertion it belongs to.
        lingerSleep?.cancel()
        lingerSleep = nil

        let task = UIApplication.shared.beginBackgroundTask(withName: "coderight.session.linger") {
            [weak self] in
            Task { @MainActor [weak self] in
                self?.expireBackgroundLinger()
            }
        }
        guard task != .invalid else {
            disconnect()
            return
        }

        backgroundTask = task
        let duration = backgroundLingerDuration
        lingerSleep = Task { [weak self] in
            do {
                try await Task.sleep(for: .seconds(duration))
            } catch {
                return
            }
            guard !Task.isCancelled else { return }
            await self?.expireBackgroundLinger()
        }
        emitState()
    }

    /// End the stream when iOS expires the assertion or the bounded sleeper.
    /// `force` lets an integration forward an expiration callback even if the
    /// platform already invalidated its identifier.
    public func expireBackgroundLinger(force: Bool = false) {
        guard force || backgroundTask != .invalid || lingerSleep != nil else { return }
        endLinger()
        cancelStream(incrementGeneration: true)
        setStatus(.offline("Background time expired. Return to CodeRight to reconnect."))
    }

    /// Idempotently end both halves of the background grace window.
    public func endLinger() {
        lingerSleep?.cancel()
        lingerSleep = nil
        guard backgroundTask != .invalid else {
            emitState()
            return
        }
        UIApplication.shared.endBackgroundTask(backgroundTask)
        backgroundTask = .invalid
        emitState()
    }

    // MARK: - Cursor and stream state

    /// Exposed for a host that restores a durable cursor before connecting.
    /// Cursors only move forward, matching the donor's replay boundary.
    public func advanceEventCursor(to sequence: Int) {
        eventCursor = max(eventCursor, sequence)
        emitState()
    }

    /// Mark history as already restored when the host supplied its own cache.
    /// The next stream then resumes directly from `eventCursor`.
    public func markHistoryLoaded() {
        historyLoaded = true
        emitState()
    }

    /// Whether a task generation is still allowed to mutate this coordinator.
    public func isStreamActive(_ generation: Int) -> Bool {
        !Task.isCancelled && generation == streamGeneration
    }

    private func openConnection(allowUnauthorized: Bool) {
        guard allowUnauthorized || status != .unauthorized else { return }
        endLinger()
        guard streamTask == nil else {
            emitState()
            return
        }

        retryDelay = 0
        externalSessionNotRunning = false
        streamGeneration += 1
        let generation = streamGeneration
        setStatus(.connecting)
        streamTask = Task { [weak self] in
            guard let self else { return }
            await self.run(generation: generation)
            await self.finishStream(generation: generation)
        }
        emitState()
    }

    private func cancelStream(incrementGeneration: Bool) {
        streamTask?.cancel()
        streamTask = nil
        if incrementGeneration { streamGeneration += 1 }
        emitState()
    }

    private func finishStream(generation: Int) {
        guard streamGeneration == generation else { return }
        streamTask = nil
        emitState()
    }

    private func run(generation: Int) async {
        while isStreamActive(generation) {
            if !historyLoaded {
                do {
                    let history = try await transport.eventHistory(session: sessionID)
                    guard isStreamActive(generation) else { return }
                    accept(history)
                    historyLoaded = true
                    emitState()
                } catch {
                    guard isStreamActive(generation) else { return }
                    if SessionLifecycleFailure.isCancellation(error) { return }
                    if SessionLifecycleFailure.isUnauthorized(error) {
                        setStatus(.unauthorized)
                        return
                    }
                    setStatus(.offline(SessionLifecycleFailure.message(for: error)))
                    guard await waitForRetry(generation: generation) else { return }
                    continue
                }
            }

            guard isStreamActive(generation) else { return }
            setStatus(.connecting)
            do {
                // The query is fixed when the stream opens. Reconnect always
                // supplies the latest cursor, so replay starts at the last
                // event accepted by `accept(_:)`.
                let stream = transport.events(session: sessionID, after: eventCursor)
                for try await event in stream {
                    guard isStreamActive(generation) else { return }
                    // This terminal signal names a replay gap, not a durable
                    // event. Keep our cursor so history fills that gap first.
                    if event.kind == "streamResyncRequired" {
                        historyLoaded = false
                        setStatus(.offline("Restoring session history…"))
                        break
                    }
                    retryDelay = 0

                    // Receiving even a replay duplicate proves the transport
                    // is live, while the sequence guard prevents duplicate
                    // transcript projection after a dropped socket.
                    setStatus(.live)
                    guard event.seq > eventCursor else {
                        emitState()
                        continue
                    }
                    onEvents([event])
                    eventCursor = max(eventCursor, event.seq)
                    emitState()
                }
                guard isStreamActive(generation) else { return }
                setStatus(.offline("Lost the connection."))
            } catch {
                guard isStreamActive(generation) else { return }
                if SessionLifecycleFailure.isCancellation(error) { return }
                if SessionLifecycleFailure.isUnauthorized(error) {
                    setStatus(.unauthorized)
                    return
                }
                if SessionLifecycleFailure.isExternalSessionNotRunning(error) {
                    // The external chat's process is gone: keep hydrated history, stop retrying.
                    externalSessionNotRunning = true
                    setStatus(.offline(SessionLifecycleFailure.externalSessionNotRunningMessage))
                    return
                }
                setStatus(.offline(SessionLifecycleFailure.message(for: error)))
            }

            guard await waitForRetry(generation: generation) else { return }
        }
    }

    /// History is the cold-hydrate boundary. The daemon guarantees ascending
    /// sequence order; filtering against the current cursor keeps an injected
    /// cache or repeated history response replay-safe.
    private func accept(_ history: [ProtocolEvent]) {
        let accepted = history.filter { $0.seq > eventCursor }
        guard !accepted.isEmpty else { return }
        onEvents(accepted)
        for event in accepted {
            eventCursor = max(eventCursor, event.seq)
        }
        emitState()
    }

    private func waitForRetry(generation: Int) async -> Bool {
        retryDelay = SessionLifecycleTiming.nextRetryDelay(after: retryDelay)
        emitState()
        do {
            try await Task.sleep(for: .seconds(retryDelay))
        } catch {
            return false
        }
        return isStreamActive(generation)
    }

    private func setStatus(_ next: SessionLifecycleStatus) {
        status = next
        emitState()
    }

    private func emitState() {
        onStateChange(snapshot)
    }
}
#endif
