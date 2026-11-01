#if os(iOS)
import Foundation
import Observation
import UIKit

/// Owns one session's transcript state: backfill, live SSE subscription with reconnect,
/// and the outbound actions (send/approve/answer/resolve/interrupt/steer/setModel).
/// Port of `../coderight-web/src/contexts/SessionContext.tsx`.
@MainActor
@Observable
public final class SessionViewModel {
    public var msgs: [TranscriptMessage] = []
    public var online = false
    public private(set) var connectionState: ConnectionLifecycleState = .idle
    public private(set) var eventCursor = 0
    /// Mirrors the lifecycle coordinator: the external chat refused the live stream (process not running).
    public private(set) var externalSessionNotRunning = false
    public var busy = false
    public var usage: UsageState = .empty
    public var model: String = ""
    public var models: [String] = []
    /// Latest validated connected/cloud tunnel result. TranscriptView owns
    /// presentation; this model only correlates SSE tool IDs to raw output.
    public internal(set) var tunnelPreviewResultContent: String?
    public let draftScope: ComposerDraftScope

    @ObservationIgnored let sessionId: String
    @ObservationIgnored let client: any DaemonClienting
    @ObservationIgnored var pendingNonces: Set<String> = []
    @ObservationIgnored var tunnelToolCallIDs: Set<String> = []
    @ObservationIgnored var streamTask: Task<Void, Never>?
    @ObservationIgnored var streamGeneration = 0
    @ObservationIgnored var backgroundTask = UIBackgroundTaskIdentifier.invalid
    @ObservationIgnored var backgroundExpiryWorkItem: DispatchWorkItem?
    @ObservationIgnored nonisolated(unsafe) var lifecycleObservers: [NSObjectProtocol] = []
    @ObservationIgnored let backgroundLingerDuration: TimeInterval
    @ObservationIgnored lazy var lifecycleCoordinator = SessionLifecycleCoordinator(
        sessionID: sessionId,
        transport: DaemonSessionLifecycleTransport(client: client),
        backgroundLingerDuration: backgroundLingerDuration,
        onEvents: { [weak self] events in
            self?.apply(events)
        },
        onStateChange: { [weak self] snapshot in
            self?.applyLifecycleSnapshot(snapshot)
        }
    )
    @ObservationIgnored lazy var liveActivityAdapter = SessionLiveActivityAdapter(
        sessionID: sessionId
    ) { [weak self] requestID, choice in
        guard let self,
              let interaction = self.msgs.reversed().compactMap(\.interaction).first(where: {
                  $0.id == requestID
              }) else { return }
        let approved = choice.caseInsensitiveCompare("Approve") == .orderedSame
            || choice.caseInsensitiveCompare("Allow") == .orderedSame
        switch interaction.kind {
        case .toolApproval:
            await self.resolveToolApproval(toolCallId: requestID, approved: approved)
        case .plan:
            await self.resolvePlan(planId: requestID, approved: approved)
        case .question:
            break
        }
    }
    @ObservationIgnored lazy var liveActivityCoordinator: CodeRightLiveActivityCoordinator = {
        let coordinator = CodeRightLiveActivityCoordinator()
        coordinator.attach(to: liveActivityAdapter)
        return coordinator
    }()

    public init(
        sessionId: String,
        daemonID: String,
        client: any DaemonClienting,
        backgroundLingerDuration: TimeInterval = ConnectionBackgroundLinger.maximumDuration
    ) {
        self.sessionId = sessionId
        draftScope = ComposerDraftScope(daemonID: daemonID, sessionID: sessionId)
        self.client = client
        self.backgroundLingerDuration = ConnectionBackgroundLinger.boundedDuration(backgroundLingerDuration)
        lifecycleObservers = [
            NotificationCenter.default.addObserver(
                forName: .codeRightDidBecomeActive, object: nil, queue: .main
            ) { [weak self] _ in
                Task { @MainActor [weak self] in self?.appDidBecomeActive() }
            },
            NotificationCenter.default.addObserver(
                forName: .codeRightDidEnterBackground, object: nil, queue: .main
            ) { [weak self] _ in
                Task { @MainActor [weak self] in self?.appDidEnterBackground() }
            }
        ]
    }

    deinit {
        for observer in lifecycleObservers {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    public func start() {
        _ = liveActivityCoordinator
        syncLiveActivity()
        Task { [weak self] in await self?.loadModels() }
        lifecycleCoordinator.connect()
    }

    public func stop() {
        lifecycleCoordinator.stop()
    }

    /// Reopen exactly one stream from the current cursor. Cancelling the old task first avoids
    /// duplicate subscriptions when foreground notifications arrive while teardown is in flight.
    public func reconnect() {
        lifecycleCoordinator.reconnect()
    }

    public func appDidBecomeActive() {
        lifecycleCoordinator.sessionLifecycleDidBecomeActive()
    }

    public func appDidEnterBackground() {
        lifecycleCoordinator.sessionLifecycleDidEnterBackground()
    }

    func applyLifecycleSnapshot(_ snapshot: SessionLifecycleSnapshot) {
        eventCursor = max(eventCursor, snapshot.eventCursor)
        externalSessionNotRunning = snapshot.externalSessionNotRunning
        switch snapshot.status {
        case .unpaired: setConnectionState(.idle)
        case .connecting: setConnectionState(.connecting)
        case .live: setConnectionState(.online)
        case .unauthorized: setConnectionState(.unauthorized)
        case let .offline(message): setConnectionState(.offline(message))
        case .cancelled: setConnectionState(.cancelled)
        }
    }

    func syncLiveActivity() {
        liveActivityAdapter.publish(messages: msgs, busy: busy)
    }

    func loadModels() async {
        guard let settings = try? await client.orchestrationSettings() else { return }
        models = settings.models
        if model.isEmpty, let primary = settings.primaryModel, !primary.isEmpty {
            model = primary
        }
    }

    func advanceEventCursor(to sequence: Int) {
        eventCursor = max(eventCursor, sequence)
    }

    func backfillAndStream(generation: Int) async {
        guard await backfill(generation: generation) else { return }
        await streamUntilStopped(generation: generation)
    }

    func backfill(generation: Int) async -> Bool {
        do {
            apply(try await client.eventHistory(session: sessionId))
        } catch let error as DaemonError {
            guard isStreamActive(generation), !handleTerminal(error) else { return false }
            setConnectionState(.offline(failureMessage(for: error)))
        } catch {
            guard isStreamActive(generation) else { return false }
            setConnectionState(.offline(failureMessage(for: error)))
        }
        return true
    }

    func streamUntilStopped(generation: Int) async {
        var attempt = 0
        while isStreamActive(generation) {
            guard let receivedEvent = await streamOnce(generation: generation) else { return }
            attempt = receivedEvent ? 1 : attempt + 1
            guard await waitForRetry(attempt: attempt, generation: generation) else { return }
        }
    }

    func streamOnce(generation: Int) async -> Bool? {
        setConnectionState(.connecting)
        var receivedEvent = false
        do {
            for try await event in client.events(session: sessionId, after: eventCursor) {
                guard isStreamActive(generation) else { return nil }
                receivedEvent = true
                setConnectionState(.online)
                apply([event])
            }
            guard isStreamActive(generation) else { return nil }
            setConnectionState(.offline("The daemon closed the event stream. Retrying…"))
        } catch let error as DaemonError {
            guard !handleTerminal(error), isStreamActive(generation) else { return nil }
            setConnectionState(.offline(failureMessage(for: error)))
        } catch {
            guard isStreamActive(generation) else { return nil }
            setConnectionState(.offline(failureMessage(for: error)))
        }
        return receivedEvent
    }

    func waitForRetry(attempt: Int, generation: Int) async -> Bool {
        let delay = ConnectionRetryBackoff.delay(forAttempt: attempt)
        do {
            try await Task.sleep(for: .seconds(delay))
            return isStreamActive(generation)
        } catch {
            return false
        }
    }

    func isStreamActive(_ generation: Int) -> Bool {
        !Task.isCancelled && generation == streamGeneration
    }

    func clearStreamTask(generation: Int) {
        guard streamGeneration == generation else { return }
        streamTask = nil
        if connectionState == .connecting {
            setConnectionState(.offline("Connection stopped."))
        }
    }

    func setConnectionState(_ state: ConnectionLifecycleState) {
        connectionState = state
        online = state.isOnline
    }

    /// Returns true when no reconnect should be attempted.
    func handleTerminal(_ error: DaemonError) -> Bool {
        if case .http(let status, _) = error, status == 401 || status == 403 {
            endBackgroundLinger()
            setConnectionState(.unauthorized)
            return true
        }
        if case .cancelled = error { return true }
        return false
    }

    func beginBackgroundLinger() {
        guard backgroundTask == .invalid else { return }
        let task = UIApplication.shared.beginBackgroundTask(withName: "coderight.session.linger") { [weak self] in
            Task { @MainActor [weak self] in self?.expireBackgroundLinger() }
        }
        guard task != .invalid else {
            expireBackgroundLinger(force: true)
            return
        }
        backgroundTask = task
        backgroundExpiryWorkItem?.cancel()
        let workItem = DispatchWorkItem { [weak self] in
            Task { @MainActor [weak self] in self?.expireBackgroundLinger() }
        }
        backgroundExpiryWorkItem = workItem
        DispatchQueue.main.asyncAfter(deadline: .now() + backgroundLingerDuration, execute: workItem)
    }

    func expireBackgroundLinger(force: Bool = false) {
        guard force || backgroundTask != .invalid || backgroundExpiryWorkItem != nil else { return }
        endBackgroundLinger()
        streamTask?.cancel()
        streamTask = nil
        streamGeneration += 1
        setConnectionState(.offline("Background time expired. Return to CodeRight to reconnect."))
    }

    func endBackgroundLinger() {
        backgroundExpiryWorkItem?.cancel()
        backgroundExpiryWorkItem = nil
        guard backgroundTask != .invalid else { return }
        UIApplication.shared.endBackgroundTask(backgroundTask)
        backgroundTask = .invalid
    }

    func failureMessage(for error: Error) -> String {
        switch error {
        case DaemonError.http(let status, _): return "The daemon returned HTTP \(status)."
        case DaemonError.transport(let message): return message.isEmpty ? "The network is unavailable." : message
        case DaemonError.decoding: return "The daemon sent an invalid response."
        case DaemonError.notConfigured: return "No daemon is configured."
        case DaemonError.cancelled: return "The connection was cancelled."
        default: return error.localizedDescription
        }
    }

}
#endif
