#if os(iOS)
import BackgroundTasks
import Foundation
import Observation

/// Owns one user-started local agent turn that may continue after CodeRight
/// leaves the foreground. iOS owns scheduling; CodeRight owns safe expiry.
@Observable
@MainActor
public final class LocalContinuedProcessingController {
    public static let shared = LocalContinuedProcessingController()
    public static let permittedIdentifier = "app.coderight.CodeRight.local-agent.*"

    public private(set) var isActive = false
    public private(set) var statusMessage: String?

    private var task: BGContinuedProcessingTask?
    private var expirationAction: (@MainActor () async -> Void)?
    private var completionPending: Bool?
    private var isExpiring = false
    private var heartbeatTask: Task<Void, Never>?
    private var startedAt: Date?

    public init() {}

    private func register(identifier: String) -> Bool {
        BGTaskScheduler.shared.register(
            forTaskWithIdentifier: identifier,
            using: .main
        ) { task in
            guard let continuedTask = task as? BGContinuedProcessingTask else {
                task.setTaskCompleted(success: false)
                return
            }
            // This closure inherits MainActor isolation. Deliver it on the
            // main queue; hopping inside cannot prevent an entry-time trap.
            Task { @MainActor in
                Self.shared.accept(continuedTask)
            }
        }
    }

    /// Submit while app is foregrounded, directly from user's Send action.
    /// `.fail` keeps behavior interactive: if iOS cannot begin immediately,
    /// UI tells user to keep CodeRight open instead of silently queueing work.
    @discardableResult
    public func begin(
        sessionID: String,
        model: String,
        onExpiration: @escaping @MainActor () async -> Void
    ) -> Bool {
        guard !isActive else { return true }

        expirationAction = onExpiration
        completionPending = nil
        let safeSession = sessionID.replacingOccurrences(
            of: "[^A-Za-z0-9-]",
            with: "-",
            options: .regularExpression
        )
        let identifier = "app.coderight.CodeRight.local-agent.\(safeSession).\(UUID().uuidString)"
        let request = BGContinuedProcessingTaskRequest(
            identifier: identifier,
            title: "CodeRight is working",
            subtitle: "\(model) is continuing your coding task"
        )
        request.strategy = .fail

        // Info.plist permits a wildcard, but iOS requires registration of each
        // concrete request identifier immediately before submission.
        guard register(identifier: identifier) else {
            expirationAction = nil
            statusMessage = "Background work registration failed. Keep CodeRight open until this turn finishes."
            return false
        }

        do {
            try BGTaskScheduler.shared.submit(request)
            isActive = true
            statusMessage = nil
            return true
        } catch {
            expirationAction = nil
            isActive = false
            statusMessage = "Background work is unavailable. Keep CodeRight open until this turn finishes."
            return false
        }
    }

    public func pulse(_ subtitle: String) {
        guard let task else { return }
        if task.progress.totalUnitCount == 0 {
            task.progress.totalUnitCount = 1_000
        }
        task.progress.completedUnitCount = min(950, task.progress.completedUnitCount + 1)
        task.updateTitle("CodeRight is working", subtitle: subtitle)
    }

    public func complete(success: Bool) {
        guard !isExpiring else { return }
        stopHeartbeat()
        let hadActiveRequest = isActive
        isActive = false
        expirationAction = nil
        guard let task else {
            if hadActiveRequest { completionPending = success }
            return
        }
        task.setTaskCompleted(success: success)
        self.task = nil
        completionPending = nil
    }

    public func dismissStatus() {
        statusMessage = nil
    }

    private func accept(_ task: BGContinuedProcessingTask) {
        if let completionPending {
            task.setTaskCompleted(success: completionPending)
            self.completionPending = nil
            return
        }

        self.task = task
        startedAt = Date()
        task.progress.totalUnitCount = 1_000
        task.progress.completedUnitCount = 1
        task.expirationHandler = { @Sendable [weak self] in
            Task { @MainActor [weak self] in
                await self?.expire()
            }
        }
        startHeartbeat()
    }

    private func expire() async {
        guard isActive else { return }
        isExpiring = true
        stopHeartbeat()
        isActive = false
        statusMessage = "Background time ended. Open CodeRight to continue from its saved checkpoint."
        task?.updateTitle("CodeRight paused", subtitle: "Open CodeRight to continue")
        let action = expirationAction
        expirationAction = nil
        await action?()
        task?.setTaskCompleted(success: false)
        task = nil
        completionPending = nil
        isExpiring = false
    }

    /// Some providers return one non-streaming response after several minutes.
    /// Advance a liveness unit & visible elapsed-time subtitle even when no
    /// protocol event arrives, so iOS can distinguish working from wedged.
    private func startHeartbeat() {
        heartbeatTask?.cancel()
        heartbeatTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(15))
                guard !Task.isCancelled, let self, self.isActive else { return }
                let elapsed = Int(Date().timeIntervalSince(self.startedAt ?? Date()))
                self.pulse("Still working · \(max(1, elapsed / 60)) min")
            }
        }
    }

    private func stopHeartbeat() {
        heartbeatTask?.cancel()
        heartbeatTask = nil
        startedAt = nil
    }
}

/// ENG-56: schedules bounded, network-aware background Git sync.
///
/// Distinct from `LocalContinuedProcessingController` above: that type keeps
/// one *specific, user-started* agent turn alive past foreground exit via
/// `BGContinuedProcessingTask`, registered per-session the moment a turn
/// begins. Git sync has no user watching a progress bar and nothing to
/// resume mid-turn — it is periodic maintenance iOS runs whenever it judges
/// conditions favorable, so it uses `BGProcessingTask` with a single fixed
/// identifier declared ahead of time (see `Info.plist`), registered once at
/// launch rather than minted per submission.
///
/// # Resumability
///
/// iOS's background task window is short and can be revoked at any moment,
/// so this scheduler always requeues the *next* attempt before doing any
/// work for the current one (`scheduleNextSync()` at the top of `run`).
/// Resumability therefore lives in "there is always a next window queued",
/// not in any single run completing — a run that gets killed mid-sync simply
/// tries again at the next scheduled window, and the Rust-side
/// `BackgroundSyncCoordinator` (`engine/crates/mobile_runtime/src/background_sync.rs`)
/// treats every target fetch as an independent, restartable unit so a
/// partial previous sweep leaves nothing torn.
///
/// # Never blocks foreground work
///
/// `syncHandler` is injected rather than this type calling into the Rust
/// runtime directly, and the handler contract requires it to poll
/// `isCancelled` and return promptly once true — the same cooperative
/// cancellation contract `BackgroundSyncCoordinator`/`SyncCancelToken`
/// implement on the Rust side. Nothing here touches `LocalRuntime`'s actor
/// state, so a background sync attempt can never contend with an active
/// foreground turn for the same lock.
@MainActor
public final class LocalBackgroundSyncScheduler {
    public static let shared = LocalBackgroundSyncScheduler()

    /// Fixed identifier: unlike `LocalContinuedProcessingController`'s
    /// per-session wildcard family, `BGProcessingTask` identifiers must be
    /// declared in `Info.plist` ahead of time, so exactly one is registered
    /// for the lifetime of the app.
    public static let identifier = "app.coderight.CodeRight.background-git-sync"

    /// Injected by the app composition root once the FFI entry point for
    /// invoking the Rust `background_sync` coordinator exists (see this
    /// lane's reported gap: `ffi.rs`/`lib.rs` are outside this lane's file
    /// allowlist). The closure receives a poll-style cancellation check —
    /// call it between bounded steps, never assume you may run to
    /// completion — and returns whether the sync step made forward
    /// progress.
    public var syncHandler: (@Sendable (_ isCancelled: @Sendable () -> Bool) async -> Bool)?

    private var isRegistered = false

    public init() {}

    /// Registers the fixed background task identifier with
    /// `BGTaskScheduler`. Call once during app launch, before
    /// `application(_:didFinishLaunchingWithOptions:)` returns — the same
    /// constraint `BGTaskScheduler` places on
    /// `LocalContinuedProcessingController`'s per-request registration,
    /// except here it happens exactly once for the fixed identifier rather
    /// than once per submission.
    @discardableResult
    public func registerIfNeeded() -> Bool {
        guard !isRegistered else { return true }
        let ok = BGTaskScheduler.shared.register(
            forTaskWithIdentifier: Self.identifier,
            using: .main
        ) { [weak self] task in
            guard let processingTask = task as? BGProcessingTask else {
                task.setTaskCompleted(success: false)
                return
            }
            Task { @MainActor [weak self] in
                await self?.run(processingTask)
            }
        }
        isRegistered = ok
        return ok
    }

    /// Requests iOS run a sync sometime at or after `seconds` from now.
    /// `requiresNetworkConnectivity` respects the ENG-56 requirement that
    /// this assistance never fires on a connectivity-less device; iOS itself
    /// enforces Wi-Fi/power/background limits beyond what this request
    /// declares — this only states intent, it does not implement the
    /// constraint checking.
    public func scheduleNextSync(earliestIn seconds: TimeInterval = 15 * 60) {
        let request = BGProcessingTaskRequest(identifier: Self.identifier)
        request.earliestBeginDate = Date(timeIntervalSinceNow: seconds)
        request.requiresNetworkConnectivity = true
        request.requiresExternalPower = false
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            // Best-effort: iOS may refuse (too many pending requests, or the
            // simulator, which does not run BGTaskScheduler callbacks at
            // all). Foreground Git actions remain the fallback path — this
            // scheduler augments them but is never required for correctness.
        }
    }

    /// Withdraws any pending scheduled sync, e.g. when the user disables
    /// background sync in Settings.
    public func cancelPendingSync() {
        BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: Self.identifier)
    }

    private func run(_ task: BGProcessingTask) async {
        let cancelled = SyncCancellationFlag()
        task.expirationHandler = { @Sendable in
            // iOS may invoke this handler off the main thread; the flag is
            // its own lock rather than relying on actor isolation here.
            cancelled.set(true)
        }

        // Requeue the next attempt before doing any work for this one, so a
        // kill mid-sync never permanently drops periodic sync — see the
        // resumability note in this type's doc comment.
        scheduleNextSync()

        guard let syncHandler else {
            task.setTaskCompleted(success: false)
            return
        }

        let madeProgress = await syncHandler { cancelled.get() }
        task.setTaskCompleted(success: madeProgress && !cancelled.get())
    }
}

/// Thread-safe cancellation flag for `LocalBackgroundSyncScheduler`.
/// `BGProcessingTask.expirationHandler` is not documented as main-thread
/// only, so this deliberately does not assume `@MainActor` isolation the way
/// the rest of this file's types do.
private final class SyncCancellationFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false

    func set(_ newValue: Bool) {
        lock.lock()
        defer { lock.unlock() }
        value = newValue
    }

    func get() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}
#endif
