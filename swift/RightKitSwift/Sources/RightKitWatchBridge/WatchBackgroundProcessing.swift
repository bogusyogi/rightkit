#if os(iOS) && canImport(BackgroundTasks)
import BackgroundTasks
import Foundation

/// Registration identity & work come from host composition. Uses APIs supported on iOS 17.
public enum WatchBackgroundProcessing {
    @discardableResult
    public static func register(identifier: String,
        process: @escaping @MainActor @Sendable () async throws -> WatchRecordingDrainResult) -> Bool {
        BGTaskScheduler.shared.register(forTaskWithIdentifier: identifier, using: nil) { task in
            guard let task = task as? BGProcessingTask else { task.setTaskCompleted(success: false); return }
            let completion = BackgroundCompletion(task: task)
            let work = Task { @MainActor in
                do {
                    let result = try await process()
                    completion.finish(result.succeeded && !Task.isCancelled)
                } catch { completion.finish(false) }
            }
            completion.attach(work)
            task.expirationHandler = { completion.expire() }
        }
    }
    public static func schedule(identifier: String, earliestBeginDate: Date = Date(timeIntervalSinceNow: 1)) throws {
        let request = BGProcessingTaskRequest(identifier: identifier)
        request.requiresNetworkConnectivity = false
        request.requiresExternalPower = false
        request.earliestBeginDate = earliestBeginDate
        try BGTaskScheduler.shared.submit(request)
    }
}

/// Expiration & normal completion race; only one completes BGTask. Late attachment cancels work.
private final class BackgroundCompletion: @unchecked Sendable {
    private let lock = NSLock()
    private let task: BGProcessingTask
    private var completed = false
    private var work: Task<Void, Never>?
    init(task: BGProcessingTask) { self.task = task }
    func attach(_ work: Task<Void, Never>) {
        let cancel = lock.withLock {
            if completed { return true }
            self.work = work; return false
        }
        if cancel { work.cancel() }
    }
    func finish(_ success: Bool) {
        let shouldComplete = lock.withLock {
            guard !completed else { return false }
            completed = true; work = nil; return true
        }
        if shouldComplete { task.setTaskCompleted(success: success) }
    }
    func expire() {
        let state: (Bool, Task<Void, Never>?) = lock.withLock {
            guard !completed else { return (false, nil) }
            completed = true
            let previous = work; work = nil
            return (true, previous)
        }
        state.1?.cancel()
        if state.0 { task.setTaskCompleted(success: false) }
    }
}
#endif
