import Foundation

public enum TransferDeliveryState: String, Codable, Sendable {
    case queued
    case awaitingAcknowledgement
}

/// One immutable file awaiting phone-side acknowledgement. `fileName` (not an absolute URL) is
/// stored because sandbox paths relocate between launches; resolve with `RelocatableFile`.
public struct TransferLedgerEntry<Payload: Codable & Sendable>: Codable, Sendable {
    public let id: String
    public var payload: Payload
    public var fileName: String
    public var state: TransferDeliveryState?
    public var awaitingSince: Date?

    public init(id: String, payload: Payload, fileName: String, state: TransferDeliveryState? = .queued, awaitingSince: Date? = nil) {
        self.id = id
        self.payload = payload
        self.fileName = fileName
        self.state = state
        self.awaitingSince = awaitingSince
    }
}

/// Pure delivery state machine for WatchConnectivity file transfers. The OS reports bytes moved;
/// only an application-level acknowledgement closes delivery. Reconciliation, not a timer, makes it
/// correct: an item awaiting acknowledgement that the OS no longer lists as outstanding is stranded
/// and is re-sent. `registrationGrace` is only a debounce for the window between `transferFile`
/// returning and the OS listing it. Callers serialise access with their own lock.
public struct TransferLedger<Payload: Codable & Sendable>: Sendable {
    public struct Dispatch: Sendable, Equatable {
        public let id: String
        public let isRetry: Bool
    }

    public private(set) var entries: [TransferLedgerEntry<Payload>]
    public var registrationGrace: TimeInterval

    public init(entries: [TransferLedgerEntry<Payload>] = [], registrationGrace: TimeInterval = 3) {
        self.entries = entries
        self.registrationGrace = registrationGrace
    }

    public init(decoding data: Data?, registrationGrace: TimeInterval = 3) {
        let decoded = data.flatMap { try? JSONDecoder().decode([TransferLedgerEntry<Payload>].self, from: $0) }
        self.init(entries: decoded ?? [], registrationGrace: registrationGrace)
    }

    public func encoded() -> Data? { try? JSONEncoder().encode(entries) }
    public var pendingCount: Int { entries.count }
    public func contains(_ id: String) -> Bool { entries.contains { $0.id == id } }

    @discardableResult
    public mutating func enqueue(_ entry: TransferLedgerEntry<Payload>) -> Bool {
        guard !contains(entry.id) else { return false }
        entries.append(entry)
        return true
    }

    /// Chooses what to send now and marks it awaiting acknowledgement. `activeIDs` are transfers the
    /// OS lists (or the caller still holds handles for) and are never re-sent. Items whose file is
    /// gone are reported in `missingFiles` and left in place.
    public mutating func claimDispatches(
        now: Date = Date(),
        activeIDs: Set<String>,
        fileExists: (TransferLedgerEntry<Payload>) -> Bool
    ) -> (dispatches: [Dispatch], missingFiles: [String]) {
        var dispatches: [Dispatch] = []
        var missing: [String] = []
        for index in entries.indices {
            let item = entries[index]
            guard !activeIDs.contains(item.id) else { continue }
            guard fileExists(item) else { missing.append(item.id); continue }
            let isQueued = item.state == .queued || item.state == nil
            let stranded = item.state == .awaitingAcknowledgement
                && now.timeIntervalSince(item.awaitingSince ?? .distantPast) >= registrationGrace
            guard isQueued || stranded else { continue }
            entries[index].state = .awaitingAcknowledgement
            entries[index].awaitingSince = now
            dispatches.append(Dispatch(id: item.id, isRetry: stranded))
        }
        return (dispatches, missing)
    }

    /// OS-level completion. Failure requeues; success starts the acknowledgement wait now (the retry
    /// timeout measures the round trip, not how long the OS took to move bytes).
    public mutating func transportFinished(id: String, failed: Bool, now: Date = Date()) {
        guard let index = entries.firstIndex(where: { $0.id == id }) else { return }
        if failed {
            entries[index].state = .queued
            entries[index].awaitingSince = nil
        } else {
            entries[index].state = .awaitingAcknowledgement
            entries[index].awaitingSince = now
        }
    }

    /// Receiver acknowledgement is the sole authority for clearing an item.
    @discardableResult
    public mutating func acknowledge(id: String) -> TransferLedgerEntry<Payload>? {
        guard let index = entries.firstIndex(where: { $0.id == id }) else { return nil }
        return entries.remove(at: index)
    }

    /// The receiver delivered nothing durable; requeue the same immutable file.
    @discardableResult
    public mutating func rollback(id: String) -> Bool {
        guard let index = entries.firstIndex(where: { $0.id == id }) else { return false }
        entries[index].state = .queued
        entries[index].awaitingSince = nil
        return true
    }

    /// After a relaunch: items the OS still lists wait for acknowledgement, all others requeue (the
    /// receiver is idempotent and the sender retains the file).
    public mutating func restoreOutstanding(_ outstandingIDs: Set<String>, now: Date = Date()) {
        for index in entries.indices {
            if outstandingIDs.contains(entries[index].id) {
                entries[index].state = .awaitingAcknowledgement
                entries[index].awaitingSince = now
            } else {
                entries[index].state = .queued
                entries[index].awaitingSince = nil
            }
        }
    }

    public mutating func remove(id: String) -> [TransferLedgerEntry<Payload>] {
        let removed = entries.filter { $0.id == id }
        entries.removeAll { $0.id == id }
        return removed
    }

    /// Undo a `remove` whose file deletion failed, unless the id was re-added meanwhile.
    public mutating func restore(_ removed: [TransferLedgerEntry<Payload>]) {
        for var item in removed where !contains(item.id) {
            item.state = .queued
            item.awaitingSince = nil
            entries.append(item)
        }
    }
}
