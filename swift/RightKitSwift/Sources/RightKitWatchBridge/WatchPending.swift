import Foundation
#if canImport(Combine)
import Combine
#endif

public struct WatchPendingItem: Codable, Equatable, Identifiable, Sendable {
    public let id: String
    public let startedAt: Date
    public let duration: TimeInterval
    public let byteCount: Int64

    public init(id: String, startedAt: Date, duration: TimeInterval, byteCount: Int64) {
        self.id = id; self.startedAt = startedAt; self.duration = duration; self.byteCount = byteCount
    }

    public var wire: [String: Any] {
        ["id": id, "startedAt": startedAt.timeIntervalSince1970, "duration": duration, "byteCount": byteCount]
    }

    public init?(wire: [String: Any]) {
        guard let id = wire["id"] as? String, !id.isEmpty,
              let started = wire["startedAt"] as? TimeInterval, started.isFinite else { return nil }
        self.init(id: id, startedAt: Date(timeIntervalSince1970: started),
                  duration: wire["duration"] as? TimeInterval ?? 0,
                  byteCount: (wire["byteCount"] as? NSNumber)?.int64Value ?? 0)
    }
}

public struct WatchPendingKeys: Sendable {
    public var list = "watchPendingList"
    public var listUpdated = "watchPendingListAt"
    public var retry = "watchPendingRetry"
    public var purge = "watchPendingPurge"
    public var purged = "watchPendingPurged"
    public var listRequest = "watchPendingListRequest"
    public init() {}
}

public struct WatchPendingSummary: Equatable, Sendable {
    public let count: Int
    public let bytes: Int64
    public let oldestAge: TimeInterval
}

/// Never expires unsent audio. Destructive actions require explicit host intent.
public struct WatchPendingPolicy: Sendable {
    public var capBytes: Int64
    public var recoveryPromptAge: TimeInterval
    public var escapeHatchAge: TimeInterval
    public init(capBytes: Int64 = 1_073_741_824, recoveryPromptAge: TimeInterval = 86_400,
                escapeHatchAge: TimeInterval = 604_800) {
        self.capBytes = capBytes; self.recoveryPromptAge = recoveryPromptAge; self.escapeHatchAge = escapeHatchAge
    }
    public func admitsNewRecording(unsentBytes: Int64) -> Bool { unsentBytes < capBytes }
    public func summary(_ items: [WatchPendingItem], now: Date) -> WatchPendingSummary {
        let bytes = items.reduce(Int64(0)) { total, item in
            let (sum, overflow) = total.addingReportingOverflow(max(0, item.byteCount))
            return overflow ? Int64.max : sum
        }
        return WatchPendingSummary(count: items.count, bytes: bytes,
            oldestAge: items.map(\.startedAt).min().map { max(0, now.timeIntervalSince($0)) } ?? 0)
    }
    public func needsRecoveryPrompt(_ items: [WatchPendingItem], now: Date) -> Bool {
        !items.isEmpty && summary(items, now: now).oldestAge >= recoveryPromptAge
    }
    public func escapeHatchAvailable(_ items: [WatchPendingItem], now: Date) -> Bool {
        !items.isEmpty && summary(items, now: now).oldestAge >= escapeHatchAge
    }
    public func purgeTargets(requested: [String], pending: [WatchPendingItem]) -> [String] {
        let have = Set(pending.map(\.id))
        var seen = Set<String>()
        return requested.filter { have.contains($0) && seen.insert($0).inserted }
    }
}

/// Phone mirror only; deletion waits for an acknowledgement from the audio owner.
@MainActor
public final class WatchPendingStore {
    #if canImport(Combine)
    @Published public private(set) var items: [WatchPendingItem]
    #else
    public private(set) var items: [WatchPendingItem]
    #endif
    private let defaults: UserDefaults
    private let defaultsKey: String
    private let keys: WatchPendingKeys
    private let send: ([String: Any]) -> Void
    public var didChange: (([WatchPendingItem]) -> Void)?

    public init(defaults: UserDefaults = .standard, defaultsKey: String, keys: WatchPendingKeys = .init(),
                send: @escaping ([String: Any]) -> Void) {
        self.defaults = defaults; self.defaultsKey = defaultsKey; self.keys = keys; self.send = send
        items = (defaults.data(forKey: defaultsKey).flatMap {
            try? JSONDecoder().decode([WatchPendingItem].self, from: $0)
        } ?? []).sorted { $0.startedAt < $1.startedAt }
    }
    public func apply(context: [String: Any]) {
        guard let raw = context[keys.list] as? [[String: Any]] else { return }
        replace(raw.compactMap(WatchPendingItem.init(wire:)))
    }
    public func applyPurged(_ ids: [String]) {
        let gone = Set(ids); replace(items.filter { !gone.contains($0.id) })
    }
    public func requestRefresh() { send([keys.listRequest: true]) }
    public func retry(_ item: WatchPendingItem) { send([keys.retry: [item.id]]) }
    public func delete(_ item: WatchPendingItem) { send([keys.purge: [item.id]]) }
    private func replace(_ next: [WatchPendingItem]) {
        items = next.sorted { $0.startedAt < $1.startedAt }
        if let data = try? JSONEncoder().encode(items) { defaults.set(data, forKey: defaultsKey) }
        didChange?(items)
    }
}

#if canImport(Combine)
extension WatchPendingStore: ObservableObject {}
#endif
