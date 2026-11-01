import Foundation
import SwiftUI
import WatchConnectivity
import RightKitWatchBridge

/// iPhone mirror of the Watch's unsent recordings (#170). The Watch owns the
/// audio; this only lists it and sends Retry (resend) / Delete (purge) commands.
@MainActor
final class WatchPendingStore: ObservableObject {
    static let shared = WatchPendingStore()
    private static let defaultsKey = "watch.pending-mirror.v1"

    @Published private(set) var items: [WatchPendingItem] = []

    private init() {
        if let data = UserDefaults.standard.data(forKey: Self.defaultsKey),
           let decoded = try? JSONDecoder().decode([WatchPendingItem].self, from: data) {
            items = decoded.sorted { $0.startedAt < $1.startedAt }
        }
    }

    func apply(context: [String: Any]) {
        guard let raw = context[WatchPendingPolicy.listKey] as? [[String: Any]] else { return }
        replace(raw.compactMap(WatchPendingItem.init(wire:)))
    }

    func applyPurged(_ ids: [String]) {
        let gone = Set(ids)
        replace(items.filter { !gone.contains($0.id) })
    }

    func requestRefresh() { send([WatchPendingPolicy.listRequestKey: true]) }
    func retry(_ item: WatchPendingItem) { send([WatchPendingPolicy.retryKey: [item.id]]) }

    func delete(_ item: WatchPendingItem) {
        // Optimistic removal waits for the Watch ack; the list stays until then.
        send([WatchPendingPolicy.purgeKey: [item.id]])
        DogfoodTraceStore.record("watch_pending_purge_requested", fields: ["recording_id": item.id])
    }

    private func replace(_ next: [WatchPendingItem]) {
        items = next.sorted { $0.startedAt < $1.startedAt }
        if let data = try? JSONEncoder().encode(items) {
            UserDefaults.standard.set(data, forKey: Self.defaultsKey)
        }
    }

    private func send(_ payload: [String: Any]) {
        guard WCSession.isSupported() else { return }
        _ = DualPathSender(transport: WCSessionTransport(session: WCSession.default)).send(payload)
    }
}

struct WatchPendingRecordingsView: View {
    @ObservedObject private var store = WatchPendingStore.shared
    let muted: Color

    var body: some View {
        if !store.items.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("Pending Watch recordings (\(store.items.count))")
                    .font(.caption.weight(.semibold))
                ForEach(store.items) { item in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(Self.duration(item.duration)) · \(Self.age(item.startedAt))")
                                .font(.caption)
                            Text(item.startedAt, format: .dateTime.month(.abbreviated).day().hour().minute())
                                .font(.caption2)
                                .foregroundStyle(muted)
                        }
                        Spacer()
                        Button("Retry") { store.retry(item) }
                            .accessibilityIdentifier("watch-pending-retry")
                        Button("Delete", role: .destructive) { store.delete(item) }
                            .accessibilityIdentifier("watch-pending-delete")
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                }
            }
            .onAppear { store.requestRefresh() }
        }
    }

    static func duration(_ seconds: TimeInterval) -> String {
        let total = max(0, Int(seconds.rounded(.down)))
        return String(format: "%d:%02d", total / 60, total % 60)
    }

    static func age(_ date: Date, now: Date = Date()) -> String {
        let s = max(0, now.timeIntervalSince(date))
        if s < 3600 { return "\(Int(s / 60)) min ago" }
        if s < 86_400 { return "\(Int(s / 3600)) h ago" }
        return "\(Int(s / 86_400)) d ago"
    }
}
