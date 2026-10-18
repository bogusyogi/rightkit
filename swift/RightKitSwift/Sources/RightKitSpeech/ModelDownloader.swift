#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation

/// Owns one background `URLSession` for model archives.
///
/// - A download survives the app being suspended or killed. If it finishes while a caller is waiting,
///   ``download(_:modelID:onProgress:)`` returns the retained file. If it finishes while nobody is waiting
///   (iOS relaunched the app for it), the file is moved synchronously into ``Configuration/staging`` and
///   ``onStaged`` is told, so the next launch installs it without downloading again.
/// - A running download for the same model id is adopted instead of duplicated, so a relaunched app
///   rejoins its transfer.
/// - The session retains its delegate, so keep one downloader for the life of the app.
/// - Hook ``handleEventsForBackgroundURLSession(identifier:completionHandler:)`` into
///   `application(_:handleEventsForBackgroundURLSession:completionHandler:)`, and call ``reattach()`` early
///   in launch so the system can deliver events for transfers that finished while the app was away.
public final class ModelDownloader: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
    public struct Configuration: Sendable {
        /// The background session identifier. Keep it stable across launches and unique per app.
        public var sessionIdentifier: String
        /// Where a download that finished with no waiter is parked.
        public var staging: ModelDownloadStaging
        /// Where a finished download is retained for the waiter until it is installed.
        public var workDirectory: URL
        /// Accepts only model ids the app knows, so a stray task cannot stage arbitrary files.
        public var isKnownModel: @Sendable (String) -> Bool

        public init(
            sessionIdentifier: String,
            staging: ModelDownloadStaging,
            workDirectory: URL,
            isKnownModel: @escaping @Sendable (String) -> Bool
        ) {
            self.sessionIdentifier = sessionIdentifier
            self.staging = staging
            self.workDirectory = workDirectory
            self.isKnownModel = isKnownModel
        }
    }

    private struct Waiter {
        let onProgress: @Sendable (Int64, Int64) -> Void
        let continuation: CheckedContinuation<URL, Error>
    }

    public let configuration: Configuration

    private let lock = NSLock()
    private var _session: URLSession?
    private var waiters: [Int: Waiter] = [:]
    private var systemCompletion: (@Sendable () -> Void)?
    private var _onStaged: (@Sendable (String) -> Void)?

    /// Called, on the session's delegate queue, with the model id of a download staged while nobody waited.
    public var onStaged: (@Sendable (String) -> Void)? {
        get { lock.withLock { _onStaged } }
        set { lock.withLock { _onStaged = newValue } }
    }

    public init(configuration: Configuration) {
        self.configuration = configuration
        super.init()
    }

    private var session: URLSession {
        lock.withLock {
            if let existing = _session { return existing }
            let background = URLSessionConfiguration.background(withIdentifier: configuration.sessionIdentifier)
            background.sessionSendsLaunchEvents = true
            background.isDiscretionary = false
            let created = URLSession(configuration: background, delegate: self, delegateQueue: nil)
            _session = created
            return created
        }
    }

    /// Recreates the session so the system can deliver events for downloads that finished while the app
    /// was not running.
    public func reattach() {
        _ = session
    }

    // MARK: Download

    /// Downloads `request` and returns the retained archive file. Joins a running download for `modelID`
    /// when one exists. Cancelling the calling task cancels the transfer. A non-2xx answer throws
    /// ``ModelDownloadError/httpStatus(_:)`` and keeps nothing.
    ///
    /// The caller owns the returned file and must move or delete it.
    public func download(
        _ request: URLRequest,
        modelID: String,
        onProgress: @escaping @Sendable (Int64, Int64) -> Void = { _, _ in }
    ) async throws -> URL {
        let session = self.session
        let running = await session.allTasks.compactMap { $0 as? URLSessionDownloadTask }.first {
            $0.taskDescription == modelID && ($0.state == .running || $0.state == .suspended)
        }
        let task: URLSessionDownloadTask
        if let running {
            task = running
        } else {
            task = session.downloadTask(with: request)
            task.taskDescription = modelID
        }
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
                guard register(taskIdentifier: task.taskIdentifier, onProgress: onProgress, continuation: continuation) else {
                    continuation.resume(throwing: ModelDownloadError.alreadyInProgress)
                    return
                }
                task.resume()
                // A cancel that raced registration cannot be missed: fail the waiter if the task is going away.
                if task.state == .canceling { fail(taskIdentifier: task.taskIdentifier, error: URLError(.cancelled)) }
            }
        } onCancel: {
            task.cancel()
        }
    }

    /// Cancels running downloads for `modelID`, or every download when nil.
    public func cancelDownloads(modelID: String? = nil) async {
        let tasks = await session.allTasks
        for task in tasks where modelID == nil || task.taskDescription == modelID { task.cancel() }
    }

    // MARK: System events

    /// Call from `application(_:handleEventsForBackgroundURLSession:completionHandler:)`. The completion
    /// handler is called on the main queue once the session has delivered all pending events.
    public func handleEventsForBackgroundURLSession(identifier: String, completionHandler: @escaping @Sendable () -> Void) {
        guard identifier == configuration.sessionIdentifier else {
            completionHandler()
            return
        }
        let previous = lock.withLock { () -> (@Sendable () -> Void)? in
            let previous = systemCompletion
            systemCompletion = completionHandler
            return previous
        }
        previous?()
        reattach()
    }

    // MARK: Waiter bookkeeping (internal for tests)

    @discardableResult
    func register(
        taskIdentifier: Int,
        onProgress: @escaping @Sendable (Int64, Int64) -> Void,
        continuation: CheckedContinuation<URL, Error>
    ) -> Bool {
        lock.withLock {
            guard waiters[taskIdentifier] == nil else { return false }
            waiters[taskIdentifier] = Waiter(onProgress: onProgress, continuation: continuation)
            return true
        }
    }

    private func takeWaiter(_ taskIdentifier: Int) -> Waiter? {
        lock.withLock { waiters.removeValue(forKey: taskIdentifier) }
    }

    func fail(taskIdentifier: Int, error: Error) {
        takeWaiter(taskIdentifier)?.continuation.resume(throwing: error)
    }

    func reportProgress(taskIdentifier: Int, written: Int64, expected: Int64) {
        let callback = lock.withLock { waiters[taskIdentifier]?.onProgress }
        callback?(written, expected)
    }

    /// The finished-download path. Must run synchronously inside `didFinishDownloadingTo`, because the
    /// system deletes `location` when that delegate call returns.
    func handleFinished(taskIdentifier: Int, modelID: String?, response: URLResponse?, location: URL) {
        guard let waiter = takeWaiter(taskIdentifier) else {
            // Nobody is waiting: the app was relaunched for this download. Park it durably.
            guard let modelID, configuration.isKnownModel(modelID) else { return }
            if configuration.staging.stageCompleted(location: location, model: modelID, response: response) {
                let callback = lock.withLock { _onStaged }
                callback?(modelID)
            }
            return
        }
        guard let http = response as? HTTPURLResponse else {
            waiter.continuation.resume(throwing: URLError(.badServerResponse))
            return
        }
        guard (200..<300).contains(http.statusCode) else {
            waiter.continuation.resume(throwing: ModelDownloadError.httpStatus(http.statusCode))
            return
        }
        do {
            try FileManager.default.createDirectory(at: configuration.workDirectory, withIntermediateDirectories: true)
            let retained = configuration.workDirectory
                .appendingPathComponent("\(modelID ?? "model")-\(UUID().uuidString).download")
            try FileManager.default.moveItem(at: location, to: retained)
            waiter.continuation.resume(returning: retained)
        } catch {
            waiter.continuation.resume(throwing: error)
        }
    }

    func completeSystemEvents() {
        let completion = lock.withLock { () -> (@Sendable () -> Void)? in
            let completion = systemCompletion
            systemCompletion = nil
            return completion
        }
        guard let completion else { return }
        DispatchQueue.main.async { completion() }
    }

    // MARK: URLSessionDownloadDelegate

    public func urlSession(
        _ session: URLSession, downloadTask: URLSessionDownloadTask,
        didWriteData bytesWritten: Int64, totalBytesWritten: Int64, totalBytesExpectedToWrite: Int64
    ) {
        reportProgress(taskIdentifier: downloadTask.taskIdentifier, written: totalBytesWritten, expected: totalBytesExpectedToWrite)
    }

    public func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        handleFinished(
            taskIdentifier: downloadTask.taskIdentifier, modelID: downloadTask.taskDescription,
            response: downloadTask.response, location: location)
    }

    public func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard let error else { return }
        fail(taskIdentifier: task.taskIdentifier, error: error)
    }

    public func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
        completeSystemEvents()
    }
}
#endif
