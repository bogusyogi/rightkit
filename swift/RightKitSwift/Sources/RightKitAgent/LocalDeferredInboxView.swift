import SwiftUI

/// Explicit on-device surface for deferred work & asynchronous agent inbox.
/// It is intentionally user-triggered: local completion demos prove no shell
/// or daemon is needed to produce, read, acknowledge, or retire messages.
struct LocalDeferredInboxView: View {
    let bridge: LocalDeferredInboxBridge

    @State private var snapshot = LocalDeferredInboxSnapshot(
        messages: [], droppedTotal: 0, pendingCount: 0)
    @State private var pendingID: String?
    @State private var error: String?
    @State private var isWorking = false

    var body: some View {
        List {
            Section("Deferred work") {
                Button("Start local deferred work") { deferWork() }
                    .disabled(isWorking || pendingID != nil)
                Button("Resolve local deferred work") { resolveWork() }
                    .disabled(isWorking || pendingID == nil)
                if let pendingID {
                    Text("Waiting: \(pendingID)")
                        .font(.caption)
                        .foregroundStyle(Theme.muted)
                }
            }

            Section("Inbox") {
                Button("Post local completion") { postLocalCompletion() }
                    .disabled(isWorking)
                Button("Refresh") { refresh() }
                    .disabled(isWorking)
                Text("\(snapshot.pendingCount) pending · \(snapshot.droppedTotal) dropped")
                    .font(.caption)
                    .foregroundStyle(Theme.muted)

                ForEach(snapshot.messages) { message in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(message.channel).font(.caption.weight(.semibold))
                        Text(message.body)
                        HStack {
                            Text("#\(message.id)").font(.caption2)
                            Spacer()
                            Button("Acknowledge") { acknowledge(message.id) }
                                .font(.caption)
                        }
                    }
                }
                if snapshot.messages.isEmpty {
                    Text("Inbox empty")
                        .foregroundStyle(Theme.muted)
                }
            }
        }
        .navigationTitle("Async Inbox")
        .task { refresh() }
        .alert("Inbox error", isPresented: Binding(
            get: { error != nil }, set: { if !$0 { error = nil } }
        )) {
            Button("OK", role: .cancel) { error = nil }
        } message: {
            Text(error ?? "Unknown error")
        }
    }

    private func deferWork() {
        let id = "ios-deferred-\(UUID().uuidString.lowercased())"
        perform {
            try bridge.deferWork(id: id)
            pendingID = id
        }
    }

    private func resolveWork() {
        guard let pendingID else { return }
        perform {
            try bridge.resolveWork(id: pendingID, content: "On-device deferred work completed.")
            self.pendingID = nil
            refresh()
        }
    }

    private func postLocalCompletion() {
        perform {
            try bridge.postLocalMessage(
                channel: "ios_local_completion",
                body: "On-device completion arrived outside the active turn.")
            refresh()
        }
    }

    private func acknowledge(_ id: UInt64) {
        perform {
            try bridge.acknowledge(through: id)
            refresh()
        }
    }

    private func refresh() {
        perform { snapshot = try bridge.read() }
    }

    private func perform(_ action: () throws -> Void) {
        isWorking = true
        defer { isWorking = false }
        do {
            try action()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
