#if os(iOS)
#if canImport(ActivityKit)
import ActivityKit
import Foundation
import UIKit
import RightKitLiveActivity

@MainActor
final class RecordingLiveActivityController {
    static let shared = RecordingLiveActivityController()

    /// Live Activities render elapsed time themselves. The only local updates
    /// are meaningful phase/status changes; audio levels never cross this boundary.
    private static let staleGrace: TimeInterval = 30

    private let activityHost = LiveActivityHost<HeardRightRecordingActivityAttributes>()
    private var startedAt = Date()
    private var lastEmittedStatus = ""
    private var lastEmittedPhase: RecordingPhase = .ready
    private var sessionID: String?
    /// Bumped by every start and end. `LiveActivityHost.end` advances the host
    /// fence itself, so orphan cleanup fences its own awaits with this instead.
    private var lifecycleGeneration: UInt64 = 0
    private var shortcutCopiedSessionID: String?
    private var startingSessionID: String?
    private enum LifecyclePhase: Hashable, Sendable {
        case idle
        case recording
        case processing
        case copied
        case ready
        case noSpeech
        case failed
        case cancelled
    }

    private static let lifecycleRules = ActivityLifecycleRules<LifecyclePhase>(
        idle: .idle,
        start: .recording,
        failure: .failed,
        terminal: [.copied, .ready, .noSpeech, .failed, .cancelled],
        watchdog: [.recording, .processing],
        transitions: [
            .idle: [.recording],
            .recording: [.processing, .copied, .noSpeech, .failed, .cancelled],
            .processing: [.copied, .ready, .noSpeech, .failed, .cancelled],
            .copied: [.recording],
            .ready: [.recording],
            .noSpeech: [.recording],
            .failed: [.recording],
            .cancelled: [.recording],
        ]
    )
    private var stateMachine = ActivityLifecycle(rules: RecordingLiveActivityController.lifecycleRules)

    private var activity: Activity<HeardRightRecordingActivityAttributes>? {
        activityHost.activity
    }

    private init() {}

    typealias StartError = LiveActivityStartError

    func start(status: String, sessionID requestedSessionID: String? = nil) async throws {
        guard !Task.isCancelled else { throw StartError.superseded }
        lifecycleGeneration &+= 1
        let activitiesEnabled = ActivityAuthorizationInfo().areActivitiesEnabled
        DogfoodTraceStore.record("live_activity_start_requested", fields: [
            "activities_enabled": activitiesEnabled,
            "session_id": requestedSessionID ?? "",
        ])
        guard activitiesEnabled else {
            DogfoodTraceStore.record("live_activity_start_blocked_disabled", fields: [
                "session_id": requestedSessionID ?? ""
            ])
            throw StartError.disabled
        }
        let newSessionID = requestedSessionID ?? UUID().uuidString
        startingSessionID = newSessionID
        defer {
            if startingSessionID == newSessionID { startingSessionID = nil }
        }
        guard stateMachine.phase == .idle || stateMachine.isTerminal else {
            DogfoodTraceStore.record("live_activity_start_blocked_state", fields: [
                "session_id": newSessionID,
                "phase": String(describing: stateMachine.phase),
            ])
            throw StartError.stateBlocked
        }
        startedAt = Date()
        lastEmittedStatus = ""
        lastEmittedPhase = .ready
        sessionID = newSessionID
        shortcutCopiedSessionID = nil
        let attributes = HeardRightRecordingActivityAttributes(sessionID: newSessionID)
        let state = HeardRightRecordingActivityAttributes.ContentState(
            status: status,
            startedAt: startedAt,
            micLevel: 0,
            phase: .recording
        )
        do {
            let outcome = try await activityHost.start(
                attributes: attributes,
                content: ActivityContent(
                    state: state,
                    staleDate: staleDate,
                    relevanceScore: 100
                ),
                isMatchingOrphan: { orphan in
                    orphan.attributes.sessionID == requestedSessionID
                }
            )
            if case .adopted = outcome, let adopted = activityHost.activity {
                let recoveredPhase = activityPhase(for: adopted.content.state.phase)
                guard stateMachine.adopt(sessionID: newSessionID, phase: recoveredPhase) else {
                    await activityHost.end(nil, policy: .immediate)
                    throw StartError.stateBlocked
                }
                startedAt = adopted.content.state.startedAt
                lastEmittedStatus = adopted.content.state.status
                lastEmittedPhase = adopted.content.state.phase
                DogfoodTraceStore.record("live_activity_orphan_adopted", fields: [
                    "session_id": newSessionID,
                    "phase": String(describing: recoveredPhase),
                ])
                DogfoodTraceStore.record("live_activity_started", fields: [
                    "session_id": newSessionID,
                    "phase": adopted.content.state.phase.rawValue,
                    "activity_id": adopted.id,
                    "activity_state": String(describing: adopted.activityState),
                    "activities_count": Activity<HeardRightRecordingActivityAttributes>.activities.count,
                    "adopted": true,
                ])
                observeStateChanges(for: adopted, sessionID: newSessionID)
                return
            } else {
                guard stateMachine.begin(sessionID: newSessionID) else {
                    await activityHost.end(nil, policy: .immediate)
                    throw StartError.stateBlocked
                }
            }
            lastEmittedStatus = status
            lastEmittedPhase = .recording
            DogfoodTraceStore.record("live_activity_started", fields: [
                "session_id": newSessionID,
                "phase": RecordingPhase.recording.rawValue,
                "activity_id": activity?.id ?? "",
                "activity_state": String(describing: activity?.activityState),
                "activities_count": Activity<HeardRightRecordingActivityAttributes>.activities.count,
            ])
            observeStateChanges(for: activity, sessionID: newSessionID)
            return
        } catch {
            let ownsStartState = (startingSessionID ?? sessionID) == newSessionID
            if ownsStartState {
                if activityHost.activity?.attributes.sessionID == newSessionID {
                    await activityHost.end(nil, policy: .immediate)
                }
                sessionID = nil
                stateMachine.reset()
            }
            DogfoodTraceStore.record("live_activity_start_failed", fields: [
                "error": String(describing: error),
                "session_id": newSessionID,
            ])
            if let liveActivityError = error as? LiveActivityStartError {
                throw liveActivityError
            }
            if let authorizationError = error as? ActivityAuthorizationError {
                throw StartError.authorization(authorizationError)
            }
            let nsError = error as NSError
            throw StartError.requestFailed(
                domain: nsError.domain,
                code: nsError.code,
                description: error.localizedDescription
            )
        }
    }

    /// Compatibility surface for capture metering callers. `micLevel` is
    /// deliberately ignored: external Live Activity state is phase-only.
    func update(
        status: String,
        micLevel _: Double,
        phase: RecordingPhase = .recording,
        alertConfiguration: AlertConfiguration? = nil
    ) async {
        guard let activity else { return }
        guard stateMachine.transition(to: activityPhase(for: phase), sessionID: sessionID) else {
            DogfoodTraceStore.record("live_activity_update_rejected", fields: [
                "session_id": sessionID ?? "",
                "phase": phase.rawValue,
            ])
            return
        }
        guard status != lastEmittedStatus || phase != lastEmittedPhase else { return }
        lastEmittedStatus = status
        lastEmittedPhase = phase
        let state = HeardRightRecordingActivityAttributes.ContentState(
            status: status,
            startedAt: startedAt,
            micLevel: 0,
            phase: phase
        )
        await activityHost.update(ActivityContent(
            state: state,
            staleDate: staleDate,
            relevanceScore: 100
        ), alert: alertConfiguration)
        DogfoodTraceStore.record("live_activity_updated", fields: [
            "session_id": sessionID ?? "",
            "phase": phase.rawValue,
            "recording_ms": Int(Date().timeIntervalSince(startedAt) * 1000),
        ])
    }

    func end(status: String, phase: RecordingPhase = .copied, dismissalDelay: TimeInterval = 4.0, expectedSessionID: String? = nil, transcriptPreview: String? = nil) async {
        if let expectedSessionID {
            // An old completion must not invalidate a newer start suspended
            // in orphan cleanup. A matching cancel still fences that start
            // before ActivityKit has returned an activity handle.
            guard (startingSessionID ?? sessionID) == expectedSessionID else { return }
        }
        let endGeneration = activityHost.startToken()
        lifecycleGeneration &+= 1
        guard let activity else { return }
        guard expectedSessionID == nil || sessionID == expectedSessionID else { return }
        let endedSessionID = sessionID
        let endedActivityID = activity.id
        let endedStartedAt = startedAt
        if !stateMachine.transition(to: activityPhase(for: phase), sessionID: sessionID) {
            stateMachine.reset()
        }
        let state = HeardRightRecordingActivityAttributes.ContentState(
            status: status,
            startedAt: startedAt,
            micLevel: 0,
            phase: phase,
            transcriptPreview: RecordingResultPreview.text(transcriptPreview)
        )
        let content = ActivityContent(state: state, staleDate: nil, relevanceScore: 100)
        if dismissalDelay > 0 {
            // Present the result once, at once. A user Shortcut that stops,
            // copies and confirms (`confirmShortcutCopy`) upgrades the content
            // to Copied without a second alert. Distinct content
            // (`resultPresentedAt`) avoids alert deduplication.
            BackgroundTranscriptionGuard.shared.startIndependentBackgroundTask(name: "recording-result-presentation") { [self] in
                guard isCurrentEnd(generation: endGeneration, sessionID: endedSessionID, activityID: endedActivityID) else { return }
                var alertedState = state
                alertedState.resultPresentedAt = Date()
                var alertedContent = ActivityContent(state: alertedState, staleDate: nil, relevanceScore: 100)
                UtteranceTimeline.mark("alert_requested", sessionID: endedSessionID)
                await presentResult(alertedContent, status: alertedState.status)
                UtteranceTimeline.mark("alert_sent", sessionID: endedSessionID)
                guard isCurrentEnd(generation: endGeneration, sessionID: endedSessionID, activityID: endedActivityID) else { return }
                DogfoodTraceStore.record("live_activity_result_presented", fields: [
                    "session_id": endedSessionID ?? "",
                    "phase": alertedState.phase.rawValue,
                    "preview_chars": state.transcriptPreview?.count ?? 0,
                ])
                // The result stays in the Dynamic Island about two seconds (Adrian, 2026-10-24: 4-6 s
                // was too long, one second "appears and disappears"). A Shortcut copy
                // confirmation inside that window upgrades it to Copied; a later one changes
                // nothing (the confirm intent still succeeds).
                let deadline = Date().addingTimeInterval(min(dismissalDelay, Self.resultDwell))
                while Date() < deadline, !isShortcutCopied(endedSessionID) {
                    try? await Task.sleep(for: .milliseconds(250))
                    guard isCurrentEnd(generation: endGeneration, sessionID: endedSessionID, activityID: endedActivityID) else { return }
                }
                if phase == .ready, isShortcutCopied(endedSessionID) {
                    // One alert per result: a later Shortcut copy confirmation only updates the
                    // content to Copied, with no second alert.
                    alertedState.phase = .copied
                    alertedState.status = "Copied to clipboard."
                    alertedContent = ActivityContent(state: alertedState, staleDate: nil, relevanceScore: 100)
                    await activityHost.update(alertedContent)
                    guard isCurrentEnd(generation: endGeneration, sessionID: endedSessionID, activityID: endedActivityID) else { return }
                }
                await finishEnd(activity: activity, content: alertedContent, generation: endGeneration, sessionID: endedSessionID, startedAt: endedStartedAt, phase: alertedState.phase, retainLockScreen: true)
            }
            return
        }
        await finishEnd(activity: activity, content: content, generation: endGeneration, sessionID: endedSessionID, startedAt: endedStartedAt, phase: phase, retainLockScreen: false)
    }

    /// Shortcuts owns its native Copy action. A following, session-bound
    /// confirmation upgrades Ready only after that action has succeeded.
    func confirmShortcutCopy(sessionID expectedSessionID: String, transcript: String) async -> Bool {
        guard sessionID == expectedSessionID,
              let activity,
              activity.attributes.sessionID == expectedSessionID,
              activity.content.state.phase == .ready,
              !transcript.isEmpty,
              activity.content.state.transcriptPreview == RecordingResultPreview.text(transcript) else { return false }
        shortcutCopiedSessionID = expectedSessionID
        var state = activity.content.state
        state.phase = .copied
        state.status = "Copied to clipboard."
        await activityHost.update(ActivityContent(state: state, staleDate: nil, relevanceScore: 100))
        DogfoodTraceStore.record("shortcut_clipboard_confirmed", fields: [
            "session_id": expectedSessionID, "chars": transcript.count,
        ])
        return true
    }

    /// Result presentation without a beep. With HeardRight in the foreground the result is
    /// already on screen, so the Live Activity only updates. Otherwise the alert still expands the
    /// Dynamic Island and lights the Lock Screen, with a silent bundled sound (a named sound the
    /// system cannot find would fall back to the default beep).
    private func presentResult(
        _ content: ActivityContent<HeardRightRecordingActivityAttributes.ContentState>,
        status: String
    ) async {
        if UIApplication.shared.applicationState == .active {
            await activityHost.update(content)
            return
        }
        await activityHost.update(content, alert: AlertConfiguration(
            title: "HeardRight",
            body: "\(status)",
            sound: .named(Self.silentAlertSound)
        ))
    }

    /// Silent 0.2 s clip in the app bundle (HeardRight/HeardRightSilence.wav).
    static let silentAlertSound = "HeardRightSilence.wav"

    /// How long a result stays in the Dynamic Island before the activity ends.
    private static let resultDwell: TimeInterval = 2.0

    private func isShortcutCopied(_ endedSessionID: String?) -> Bool {
        guard let endedSessionID else { return false }
        return shortcutCopiedSessionID == endedSessionID
    }

    private func isCurrentEnd(generation: UInt64, sessionID endedSessionID: String?, activityID: String) -> Bool {
        activityHost.isCurrent(generation) && sessionID == endedSessionID && activity?.id == activityID
    }

    private func finishEnd(activity: Activity<HeardRightRecordingActivityAttributes>, content: ActivityContent<HeardRightRecordingActivityAttributes.ContentState>, generation endGeneration: UInt64, sessionID endedSessionID: String?, startedAt endedStartedAt: Date, phase: RecordingPhase, retainLockScreen: Bool) async {
        let endedActivityID = activity.id
        // A newer start may replace this activity during result presentation.
        // Never dismiss or clear its state from an older completion.
        guard activityHost.isCurrent(endGeneration),
              sessionID == endedSessionID,
              self.activity?.id == endedActivityID else { return }
        await activityHost.end(content, policy: retainLockScreen ? .after(Date().addingTimeInterval(30)) : .immediate)
        guard sessionID == endedSessionID,
              self.activity == nil || self.activity?.id == endedActivityID else {
            DogfoodTraceStore.record("live_activity_end_skipped_stale", fields: [
                "ended_session_id": endedSessionID ?? "",
                "current_session_id": sessionID ?? "",
                "ended_activity_id": endedActivityID,
                "current_activity_id": self.activity?.id ?? "",
            ])
            return
        }
        sessionID = nil
        stateMachine.reset()
        lastEmittedStatus = ""
        lastEmittedPhase = .ready
        DogfoodTraceStore.record("live_activity_ended", fields: [
            "session_id": endedSessionID ?? "",
            "phase": phase.rawValue,
            "recording_ms": Int(Date().timeIntervalSince(endedStartedAt) * 1000),
        ])
    }

    private func observeStateChanges(
        for activity: Activity<HeardRightRecordingActivityAttributes>?,
        sessionID: String
    ) {
        guard let activity else { return }
        Task {
            for await state in activity.activityStateUpdates {
                DogfoodTraceStore.record("live_activity_state_changed", fields: [
                    "session_id": sessionID,
                    "activity_id": activity.id,
                    "activity_state": String(describing: state),
                ])
            }
        }
    }

    /// Reconcile an ActivityKit survivor with the shared command session.
    /// Mismatched activities are ended; a matching active session is preserved.
    func endAnyOrphan() async {
        let cleanupGeneration = lifecycleGeneration
        let cleanupSessionID = sessionID
        let cleanupActivityID = activity?.id
        let snapshot = SharedDictationStateStore().snapshot
        let activeSessionID: String? = if let snapshot,
            snapshot.isActiveWork,
            Date().timeIntervalSince(snapshot.updatedAt) < 10 {
            snapshot.sessionID
        } else {
            nil
        }
        for activity in Activity<HeardRightRecordingActivityAttributes>.activities {
            if let activeSessionID, activity.attributes.sessionID == activeSessionID {
                continue
            }
            if activity.id == self.activity?.id {
                await activityHost.end(nil, policy: .immediate)
            } else {
                await activity.end(
                    ActivityContent(state: activity.content.state, staleDate: nil),
                    dismissalPolicy: .immediate
                )
            }
        }
        if activeSessionID != nil {
            DogfoodTraceStore.record("live_activity_orphan_cleanup_preserved_active", fields: [
                "session_id": activeSessionID ?? "",
            ])
            return
        }
        // A start or end that ran during the awaits above owns the state now.
        // The host clears `activity` when this cleanup ends our own activity,
        // so nil is ours; a different activity can only come from a newer start.
        guard lifecycleGeneration == cleanupGeneration,
              sessionID == cleanupSessionID,
              activity == nil || activity?.id == cleanupActivityID else { return }
        sessionID = nil
        stateMachine.reset()
    }

    private func activityPhase(for phase: RecordingPhase) -> LifecyclePhase {
        switch phase {
        case .recording: .recording
        case .processing: .processing
        case .copied: .copied
        case .ready: .ready
        case .noSpeech: .noSpeech
        case .failed: .failed
        case .cancelled: .cancelled
        }
    }

    private var staleDate: Date {
        startedAt
            .addingTimeInterval(HeardRightShared.maxRecordingDuration)
            .addingTimeInterval(Self.staleGrace)
    }
}
#endif
#endif
