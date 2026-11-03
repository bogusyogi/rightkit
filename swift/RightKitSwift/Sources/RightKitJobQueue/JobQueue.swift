import Foundation

/// One job as stored and returned by ``JobQueue``.
public struct JobRecord<Payload: Codable & Sendable>: Codable, Sendable {
    public enum State: Codable, Equatable, Sendable {
        case queued
        /// Claimed by a worker until `leaseUntil`. An expired lease is claimable again.
        case claimed(leaseUntil: Date)
        case done
        /// Failed for good, with the last reason recorded.
        case failed(reason: String)
    }

    public let id: String
    public let payload: Payload
    public let enqueuedAt: Date
    /// How many times a worker has claimed this job.
    public private(set) var attempts: Int
    public private(set) var state: State

    public init(id: String, payload: Payload, enqueuedAt: Date, attempts: Int, state: State) {
        self.id = id
        self.payload = payload
        self.enqueuedAt = enqueuedAt
        self.attempts = attempts
        self.state = state
    }

    fileprivate mutating func setState(_ state: State) { self.state = state }
    fileprivate mutating func incrementAttempts() { attempts += 1 }
}

public enum JobQueueError: Error, Equatable, Sendable {
    case unknownJob(String)
    case notClaimed(String)
    /// The queue file exists but cannot be decoded. Nothing is rewritten; move the file aside to recover.
    case corruptQueue(String)
}

/// A durable, first-in first-out job queue persisted as one atomically written JSON file.
///
/// Workers call ``claim(now:leaseSeconds:)``, then ``complete(_:)`` or ``fail(_:reason:retry:)``. A worker
/// that dies leaves its lease to expire, and the job is claimed again. Every mutation is saved before it returns,
/// so a crash never loses a completed or failed state. Isolation is the actor's, so one process owns one queue file.
public actor JobQueue<Payload: Codable & Sendable> {
    private let fileURL: URL
    private var jobs: [JobRecord<Payload>]
    /// Attempts after which a failing job is no longer retried.
    public let maxAttempts: Int

    public init(fileURL: URL, maxAttempts: Int = 5) throws {
        self.fileURL = fileURL
        self.maxAttempts = max(1, maxAttempts)
        if FileManager.default.fileExists(atPath: fileURL.path) {
            do {
                jobs = try JSONDecoder().decode([JobRecord<Payload>].self, from: Data(contentsOf: fileURL))
            } catch {
                throw JobQueueError.corruptQueue(fileURL.lastPathComponent)
            }
        } else {
            jobs = []
        }
    }

    /// All jobs in enqueue order, for display and diagnostics.
    public var all: [JobRecord<Payload>] { jobs }

    /// Appends a job and returns its id.
    @discardableResult
    public func enqueue(_ payload: Payload, now: Date = Date()) throws -> String {
        let id = UUID().uuidString
        jobs.append(JobRecord(id: id, payload: payload, enqueuedAt: now, attempts: 0, state: .queued))
        try save()
        return id
    }

    /// Claims the oldest queued job, or a job whose lease has expired. Returns `nil` when nothing is claimable.
    public func claim(now: Date = Date(), leaseSeconds: TimeInterval = 60) throws -> JobRecord<Payload>? {
        guard let index = jobs.firstIndex(where: { job in
            switch job.state {
            case .queued: true
            case let .claimed(leaseUntil): leaseUntil <= now
            case .done, .failed: false
            }
        }) else { return nil }
        jobs[index].setState(.claimed(leaseUntil: now.addingTimeInterval(leaseSeconds)))
        jobs[index].incrementAttempts()
        try save()
        return jobs[index]
    }

    /// Marks a claimed job done.
    public func complete(_ id: String) throws {
        let index = try claimedIndex(id)
        jobs[index].setState(.done)
        try save()
    }

    /// Ends a claimed job's attempt. With `retry` and attempts left, the job is queued again; otherwise it fails for good.
    public func fail(_ id: String, reason: String, retry: Bool) throws {
        let index = try claimedIndex(id)
        if retry && jobs[index].attempts < maxAttempts {
            jobs[index].setState(.queued)
        } else {
            jobs[index].setState(.failed(reason: reason))
        }
        try save()
    }

    /// Removes finished jobs, done or failed, and returns how many were removed.
    @discardableResult
    public func purgeFinished() throws -> Int {
        let before = jobs.count
        jobs.removeAll { job in
            switch job.state {
            case .done, .failed: true
            case .queued, .claimed: false
            }
        }
        try save()
        return before - jobs.count
    }

    private func claimedIndex(_ id: String) throws -> Int {
        guard let index = jobs.firstIndex(where: { $0.id == id }) else { throw JobQueueError.unknownJob(id) }
        guard case .claimed = jobs[index].state else { throw JobQueueError.notClaimed(id) }
        return index
    }

    private func save() throws {
        try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(jobs).write(to: fileURL, options: .atomic)
    }
}
