import Foundation

/// Platform hooks the coordinator calls; implemented by `CloudSyncTriggerSources` or by the app.
public protocol CloudSyncLifecycleObserver: Sendable {
    /// Sync was switched on or off: start/stop the network monitor, register for remote pushes.
    func syncEnabledDidChange(_ enabled: Bool)
    /// A trigger is about to run (e.g. re-register for remote notifications on foreground).
    func syncWillRun(_ trigger: SyncTrigger)
}

/// Owns every sync trigger. Single-flight; arrivals during a run coalesce into one follow-up pass and
/// every caller receives the result; OFF is a hard fence via a generation counter; failed work retries
/// with bounded exponential backoff that a success cancels (a healthy idle app never polls).
public actor CloudSyncCoordinator {
    private let runner: any CloudSyncRunning
    private let enabledStore: any SyncEnabledStoring
    private let backoff: SyncBackoffPolicy
    private let sleep: @Sendable (TimeInterval) async throws -> Void
    private let retryMessage: String
    private var lifecycle: (any CloudSyncLifecycleObserver)?

    private var generation: UInt64 = 0
    private var running = false
    private var pending = false
    private var waiters: [CheckedContinuation<SyncResult, Never>] = []
    private var retryTask: Task<Void, Never>?
    private var retryAttempts = 0

    public init(
        runner: any CloudSyncRunning,
        enabledStore: any SyncEnabledStoring,
        backoff: SyncBackoffPolicy = .default,
        messages: SyncMessages = SyncMessages(),
        sleep: @escaping @Sendable (TimeInterval) async throws -> Void = { seconds in
            try await Task.sleep(nanoseconds: UInt64(min(max(seconds, 0), 1e9) * 1_000_000_000))
        }
    ) {
        self.runner = runner
        self.enabledStore = enabledStore
        self.backoff = backoff
        self.sleep = sleep
        self.retryMessage = messages.retryWhenConnected
    }

    public func setLifecycleObserver(_ observer: (any CloudSyncLifecycleObserver)?) {
        lifecycle = observer
        if enabledStore.isEnabled { observer?.syncEnabledDidChange(true) }
    }

    public func setEnabled(_ value: Bool) async {
        guard value != enabledStore.isEnabled else { return }
        generation &+= 1
        let mine = generation
        pending = false
        retryTask?.cancel()
        retryTask = nil
        retryAttempts = 0
        await runner.setEnabled(value)
        guard generation == mine else { return }
        lifecycle?.syncEnabledDidChange(value)
        if value { Task { _ = await self.request(.enabled) } }
    }

    @discardableResult
    public func request(_ trigger: SyncTrigger) async -> SyncResult {
        guard enabledStore.isEnabled else { return .disabled }
        if trigger != .retry { retryAttempts = 0 }
        lifecycle?.syncWillRun(trigger)
        if trigger == .accountChanged {
            generation &+= 1
            retryTask?.cancel()
            retryTask = nil
            await runner.accountDidChange()
            guard enabledStore.isEnabled else { return .disabled }
        }
        pending = true
        if running {
            return await withCheckedContinuation { waiters.append($0) }
        }
        running = true
        var result = SyncResult.disabled
        while enabledStore.isEnabled, pending {
            let runGeneration = generation
            pending = false
            if let delay = await runner.retryDelayRemaining(), delay > 0 {
                result.status = .unavailable(retryMessage)
                break
            }
            let pass = await runner.sync()
            result = SyncResult(
                uploaded: result.uploaded + pass.uploaded,
                downloaded: result.downloaded + pass.downloaded,
                deleted: result.deleted + pass.deleted,
                status: pass.status
            )
            if case .ready = result.status {
                retryAttempts = 0
                retryTask?.cancel()
                retryTask = nil
            }
            // OFF is a hard fence: a returned CloudKit operation may finish but cannot schedule more.
            guard enabledStore.isEnabled, generation == runGeneration else {
                result.status = .disabled
                continue
            }
        }
        running = false
        let completed = waiters
        waiters.removeAll()
        for waiter in completed { waiter.resume(returning: result) }
        let completedGeneration = generation
        if case .unavailable = result.status, enabledStore.isEnabled,
           let delay = await runner.retryDelayRemaining(), delay > 0, generation == completedGeneration {
            scheduleRetry(serverHint: delay, generation: completedGeneration)
        }
        return result
    }

    private func scheduleRetry(serverHint: TimeInterval, generation: UInt64) {
        guard retryTask == nil else { return }
        retryAttempts = min(retryAttempts + 1, backoff.maxAttempts)
        let delay = backoff.delay(attempt: retryAttempts, serverHint: serverHint)
        let sleep = self.sleep
        retryTask = Task { [weak self] in
            do { try await sleep(delay) } catch { return }
            await self?.fireRetry(generation: generation)
        }
    }

    private func fireRetry(generation: UInt64) async {
        guard enabledStore.isEnabled, self.generation == generation else { return }
        retryTask = nil
        _ = await request(.retry)
    }
}
