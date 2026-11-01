import SwiftUI

/// A single durable cross-session memory row, mirroring the shape of
/// `coderight-memory::store::MemoryRecord` (engine/crates/memory/src/store.rs).
/// This UI-side type intentionally carries no screening logic of its own --
/// screening is a write-time engine gate (`coderight_memory::screening`);
/// anything reaching this panel already passed it.
struct MemoryItem: Identifiable, Equatable {
    let id: String
    var scope: String
    var content: String
    var createdAt: String
    var accessCount: Int
    var pinned: Bool
}

/// Project memory is loaded & mutated through the shared runtime's screened store.
@MainActor
@Observable
final class MemoryPanelStore {
    private(set) var items: [MemoryItem] = []
    private(set) var isLoading = false
    var errorMessage: String?

    private let loader: () async throws -> [MemoryItem]
    private let deleter: (String) async throws -> Void
    private let pinner: (String, Bool) async throws -> Void
    private let writer: ((String) async throws -> Void)?
    var canWrite: Bool { writer != nil }

    init(
        loader: @escaping () async throws -> [MemoryItem],
        deleter: @escaping (String) async throws -> Void = { _ in throw MemoryStorageFailure() },
        pinner: @escaping (String, Bool) async throws -> Void = { _, _ in throw MemoryStorageFailure() },
        writer: ((String) async throws -> Void)? = nil
    ) {
        self.loader = loader
        self.deleter = deleter
        self.pinner = pinner
        self.writer = writer
    }

    static func local(root: URL) -> MemoryPanelStore {
        let storage = root.appendingPathComponent(".coderight/runtime").path
        return MemoryPanelStore(loader: {
            let rows = try await LocalRuntimeBridge.listMemories(storageDir: storage, scope: "project")
            return try rows.map { value in
                guard let row = value as? [String: Any], let id = row["id"] as? String,
                      let content = row["content"] as? String else { throw MemoryStorageFailure() }
                return MemoryItem(id: id, scope: "Project", content: content,
                    createdAt: row["created_at"] as? String ?? "", accessCount: row["access_count"] as? Int ?? 0,
                    pinned: row["pinned"] as? Bool ?? false)
            }
        }, deleter: { id in
            _ = try await LocalRuntimeBridge.deleteMemory(storageDir: storage, id: id)
        }, pinner: { id, pinned in
            try await LocalRuntimeBridge.pinMemory(storageDir: storage, id: id, pinned: pinned)
        }, writer: { content in
            try await LocalRuntimeBridge.writeMemory(storageDir: storage, id: UUID().uuidString,
                content: content, createdAt: ISO8601DateFormatter().string(from: Date()))
        })
    }

    func add(_ content: String) async -> Bool {
        guard let writer else { return false }
        do { try await writer(content); await load(); return errorMessage == nil }
        catch { errorMessage = error.localizedDescription; return false }
    }

    func load() async {
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }
        do {
            items = try await loader()
        } catch {
            errorMessage = "Couldn't load memories: \(error.localizedDescription)"
        }
    }

    func delete(_ item: MemoryItem) async {
        let previous = items
        items.removeAll { $0.id == item.id }
        do {
            try await deleter(item.id)
        } catch {
            items = previous
            errorMessage = "Couldn't delete memory: \(error.localizedDescription)"
        }
    }

    func setPinned(_ item: MemoryItem, pinned: Bool) async {
        guard let index = items.firstIndex(where: { $0.id == item.id }) else { return }
        let previous = items[index].pinned
        items[index].pinned = pinned
        do {
            try await pinner(item.id, pinned)
        } catch {
            items[index].pinned = previous
            errorMessage = "Couldn't update pin: \(error.localizedDescription)"
        }
    }
}

struct MemoryStorageFailure: LocalizedError {
    var errorDescription: String? { "Memory storage is unavailable." }
}

struct MemoryPanelView: View {
    @State private var store: MemoryPanelStore
    @Environment(\.dismiss) private var dismiss
    @Environment(LocalWorkspaceStore.self) private var workspaces
    @State private var pendingDelete: MemoryItem?
    @State private var selectedProject: LocalWorkspace?
    @State private var adding = false
    @State private var draft = ""
    @State private var saving = false
    private let usesProjectStore: Bool

    init(store: MemoryPanelStore? = nil) {
        usesProjectStore = store == nil
        _store = State(initialValue: store ?? MemoryPanelStore(loader: { throw MemoryStorageFailure() }))
    }

    var body: some View {
        NavigationStack {
            Group {
                if store.isLoading && store.items.isEmpty {
                    ProgressView("Loading memories…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if store.items.isEmpty {
                    ContentUnavailableView(
                        "No memories yet",
                        systemImage: "brain",
                        description: Text("Save useful project facts to include in new chats.")
                    )
                } else {
                    List {
                        if !pinnedItems.isEmpty {
                            Section("Pinned") {
                                ForEach(pinnedItems) { item in
                                    MemoryRow(item: item, store: store, pendingDelete: $pendingDelete)
                                }
                            }
                        }
                        Section(pinnedItems.isEmpty ? "Memories" : "Other memories") {
                            ForEach(unpinnedItems) { item in
                                MemoryRow(item: item, store: store, pendingDelete: $pendingDelete)
                            }
                        }
                    }
                    .listStyle(.insetGrouped)
                    .scrollContentBackground(.hidden)
                }
            }
            .background(Theme.background)
            .navigationTitle("Memory")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    if usesProjectStore {
                        Menu(selectedProject?.name ?? "Choose project") {
                            ForEach(workspaces.workspaces) { workspace in
                                Button(workspace.name) { Task { await select(workspace) } }
                            }
                        }
                    }
                }
                ToolbarItem(placement: .bottomBar) {
                    if store.canWrite { Button("Add memory", systemImage: "plus") { adding = true } }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }.accessibilityIdentifier("memory-done")
                }
            }
            .task {
                if usesProjectStore, let workspace = workspaces.workspaces.first { await select(workspace) }
                else if !usesProjectStore { await store.load() }
            }
            .sheet(isPresented: $adding) {
                NavigationStack {
                    Form {
                        Section(selectedProject?.name ?? "Project") {
                            TextField("Something useful to remember…", text: $draft, axis: .vertical)
                                .lineLimit(5...12).accessibilityIdentifier("memory-content")
                            Text("\(draft.count)/4,000 characters").font(.caption).foregroundStyle(Theme.muted)
                        }
                        Text("Included in new chats in this project. Up to 8,000 characters of saved context are used, with pinned facts first. Existing conversations retain their earlier context.")
                            .font(.footnote).foregroundStyle(Theme.muted)
                        if let error = store.errorMessage { Text(error).foregroundStyle(Theme.danger) }
                    }.navigationTitle("Add memory")
                        .toolbar {
                            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { adding = false; draft = "" } }
                            ToolbarItem(placement: .confirmationAction) {
                                Button("Save") {
                                    saving = true
                                    Task { if await store.add(draft) { draft = ""; adding = false }; saving = false }
                                }.disabled(saving || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || draft.count > 4_000)
                            }
                        }
                }
            }
            .alert(
                "Delete this memory?",
                isPresented: Binding(
                    get: { pendingDelete != nil },
                    set: { if !$0 { pendingDelete = nil } }
                ),
                presenting: pendingDelete
            ) { item in
                Button("Delete", role: .destructive) {
                    Task { await store.delete(item) }
                }
                Button("Cancel", role: .cancel) {}
            } message: { item in
                Text(item.content)
            }
            .alert(
                "Something went wrong",
                isPresented: Binding(
                    get: { store.errorMessage != nil && !adding },
                    set: { if !$0 { store.errorMessage = nil } }
                )
            ) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(store.errorMessage ?? "")
            }
        }
    }

    private func select(_ workspace: LocalWorkspace) async {
        selectedProject = workspace
        store = .local(root: workspaces.url(for: workspace))
        await store.load()
    }

    private var pinnedItems: [MemoryItem] { store.items.filter(\.pinned) }
    private var unpinnedItems: [MemoryItem] { store.items.filter { !$0.pinned } }
}

private struct MemoryRow: View {
    let item: MemoryItem
    let store: MemoryPanelStore
    @Binding var pendingDelete: MemoryItem?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                if item.pinned {
                    Image(systemName: "pin.fill")
                        .font(.caption)
                        .foregroundStyle(Theme.accent)
                        .accessibilityHidden(true)
                }
                Text(item.scope)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Theme.muted)
                Spacer()
                Text(ISO8601DateFormatter().date(from: item.createdAt)?.formatted(date: .abbreviated, time: .omitted) ?? item.createdAt)
                    .font(.caption2)
                    .foregroundStyle(Theme.muted)
            }
            Text(item.content)
                .font(.subheadline)
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
            Text(item.pinned ? "Pinned context" : "Saved project context")
                .font(.caption2)
                .foregroundStyle(Theme.muted)
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(item.pinned ? "Pinned memory" : "Memory"): \(item.content)")
        .swipeActions(edge: .trailing) {
            Button("Delete", role: .destructive) {
                pendingDelete = item
            }
            Button(item.pinned ? "Unpin" : "Pin") {
                Task { await store.setPinned(item, pinned: !item.pinned) }
            }
            .tint(Theme.accent2)
        }
    }
}

#Preview {
    MemoryPanelView(store: MemoryPanelStore(loader: {
        [
            MemoryItem(id: "1", scope: "global", content: "Adrian prefers pnpm for this workspace.", createdAt: "2026-08-20", accessCount: 4, pinned: true),
            MemoryItem(id: "2", scope: "project", content: "The desktop UI lives in apps/coderight-tauri.", createdAt: "2026-08-25", accessCount: 1, pinned: false),
        ]
    }))
}
