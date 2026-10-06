import Foundation

/// A cross-process command. `requestID` correlates acknowledgement; `issuedAt` drives expiry.
public protocol AppGroupCommand: Codable, Equatable, Sendable {
    var requestID: UUID { get }
    var issuedAt: Date { get }
}

/// Bounded, crash-safe FIFO command queue persisted in the App Group.
///
/// `claimAll()` reserves commands without removing them. Consumers call `acknowledge(_:)` only after
/// attempting execution; a crash leaves a command claimable again once its lease expires
/// (at-least-once delivery).
///
/// Lock contract: when the lock cannot be taken, no mutation runs unsynchronised. `submit` returns
/// `nil` (never a fabricated success), `pending`/`claimAll` return empty with the queue untouched,
/// and `acknowledge` skips removal so the command recovers through its claim lease.
public struct AppGroupCommandQueue<Command: AppGroupCommand>: @unchecked Sendable {
    public struct Limits: Sendable {
        public var maxCommands = 8
        public var commandTTL: TimeInterval = 30
        public var claimLeaseTTL: TimeInterval = 5 * 60
        public var maxAcknowledged = 32
        public init() {}
    }

    private struct Entry: Codable {
        let command: Command
        var claimedAt: Date?
    }

    private let configuration: AppGroupConfiguration
    private let queueKey: String
    private let acknowledgedKey: String
    private let limits: Limits

    public init(
        configuration: AppGroupConfiguration,
        queueKey: String = "rightkit.commandQueue",
        acknowledgedKey: String = "rightkit.commandAcknowledgedIDs",
        limits: Limits = Limits()
    ) {
        self.configuration = configuration
        self.queueKey = queueKey
        self.acknowledgedKey = acknowledgedKey
        self.limits = limits
    }

    private var defaults: UserDefaults? { configuration.defaults }

    /// Persists `command` under the lock. `nil` means it was never queued, which keeps a failed
    /// submit honest: callers watching for an acknowledgement would otherwise wait forever.
    @discardableResult
    public func submit(_ command: Command, now: Date = Date()) -> Command? {
        let queued: Void? = AppGroupFileLock.withExclusiveLock(configuration) {
            var entries = liveEntries(now: now)
            entries.append(Entry(command: command, claimedAt: nil))
            if entries.count > limits.maxCommands {
                entries.removeFirst(entries.count - limits.maxCommands)
            }
            save(entries)
        }
        guard queued != nil else {
            configuration.report(.commandSubmitFailed(requestID: command.requestID))
            return nil
        }
        return command
    }

    public func pending(now: Date = Date()) -> [Command] {
        AppGroupFileLock.withExclusiveLock(configuration) {
            liveEntries(now: now).filter { $0.claimedAt == nil }.map(\.command)
        } ?? []
    }

    public func claimAll(now: Date = Date()) -> [Command] {
        AppGroupFileLock.withExclusiveLock(configuration) {
            var entries = liveEntries(now: now)
            var claimed: [Command] = []
            for index in entries.indices where entries[index].claimedAt == nil {
                entries[index].claimedAt = now
                claimed.append(entries[index].command)
            }
            save(entries)
            return claimed
        } ?? []
    }

    public func acknowledge(_ requestIDs: [UUID], now: Date = Date()) {
        guard !requestIDs.isEmpty else { return }
        let removed: Void? = AppGroupFileLock.withExclusiveLock(configuration) {
            let ids = Set(requestIDs)
            save(liveEntries(now: now).filter { !ids.contains($0.command.requestID) })
            var acknowledged = acknowledgedRequestIDs()
            acknowledged.append(contentsOf: requestIDs)
            if acknowledged.count > limits.maxAcknowledged {
                acknowledged.removeFirst(acknowledged.count - limits.maxAcknowledged)
            }
            defaults?.set(acknowledged.map(\.uuidString), forKey: acknowledgedKey)
        }
        if removed == nil { configuration.report(.commandAcknowledgeFailed(requestIDs: requestIDs)) }
    }

    /// `nil` means either "none acknowledged yet" or "lock unavailable"; callers treat both as unknown.
    public func lastAcknowledgedRequestID() -> UUID? {
        AppGroupFileLock.withExclusiveLock(configuration) { acknowledgedRequestIDs().last } ?? nil
    }

    private func acknowledgedRequestIDs() -> [UUID] {
        (defaults?.stringArray(forKey: acknowledgedKey) ?? []).compactMap(UUID.init(uuidString:))
    }

    private func liveEntries(now: Date) -> [Entry] {
        let acknowledged = Set(acknowledgedRequestIDs())
        var entries = decodeEntries(defaults?.data(forKey: queueKey)).filter { entry in
            guard now.timeIntervalSince(entry.command.issuedAt) <= limits.commandTTL || entry.claimedAt != nil else {
                return false
            }
            return !acknowledged.contains(entry.command.requestID)
        }
        for index in entries.indices {
            if let claimedAt = entries[index].claimedAt, now.timeIntervalSince(claimedAt) > limits.claimLeaseTTL {
                entries[index].claimedAt = nil
            }
        }
        save(entries)
        return entries
    }

    private func decodeEntries(_ data: Data?) -> [Entry] {
        guard let data else { return [] }
        if let entries = try? JSONDecoder().decode([Entry].self, from: data) { return entries }
        // Queues written before claim leases existed stored bare commands.
        guard let commands = try? JSONDecoder().decode([Command].self, from: data) else { return [] }
        return commands.map { Entry(command: $0, claimedAt: nil) }
    }

    private func save(_ entries: [Entry]) {
        guard let data = try? JSONEncoder().encode(entries) else { return }
        defaults?.set(data, forKey: queueKey)
    }
}
