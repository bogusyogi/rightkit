import SwiftUI

/// product atom 20: picker sheet for inserting a saved prompt template into the composer.
/// This is the focused "just the templates" surface; `CommandPaletteView` (APP-14) embeds
/// the same store as one of several pluggable result providers for the broader palette.
@MainActor
public struct LocalPromptPaletteView: View {
    @Bindable private var store: LocalPromptTemplateStore
    @State private var query = ""
    @State private var editing: LocalPromptTemplate?
    @State private var adding = false
    @State private var command = ""
    @State private var title = ""
    @State private var prompt = ""
    @State private var validationError: String?
    @Environment(\.dismiss) private var dismiss
    private let onInsert: (String) -> Void

    public init(store: LocalPromptTemplateStore, onInsert: @escaping (String) -> Void) {
        self.store = store
        self.onInsert = onInsert
    }

    public var body: some View {
        NavigationStack {
            List {
                ForEach(store.matching(query)) { template in
                    Button {
                        onInsert(template.body)
                        dismiss()
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("/\(template.command)")
                                .font(.caption.monospaced())
                                .foregroundStyle(Theme.accent)
                            Text(template.title)
                                .font(.body)
                                .foregroundStyle(Theme.text)
                            Text(template.body)
                                .font(.caption)
                                .foregroundStyle(Theme.muted)
                                .lineLimit(2)
                        }
                    }
                    .accessibilityIdentifier("prompt-template-\(template.command)")
                    .contextMenu {
                        Button("Edit", systemImage: "pencil") {
                            editing = template; command = template.command; title = template.title
                            prompt = template.body; validationError = nil; adding = true
                        }
                        Button("Delete", systemImage: "trash", role: .destructive) { store.remove(template.id) }
                    }
                }
                .onDelete { offsets in
                    let visible = store.matching(query)
                    for index in offsets {
                        store.remove(visible[index].id)
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .searchable(text: $query, prompt: "Search prompt templates")
            .navigationTitle("Prompt library")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button("Add prompt", systemImage: "plus") {
                        editing = nil; command = ""; title = ""; prompt = ""; validationError = nil; adding = true
                    }
                }
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .background(Theme.background)
            .sheet(isPresented: $adding) {
                NavigationStack {
                    Form {
                        Section("Prompt") {
                            TextField("Title", text: $title).accessibilityIdentifier("prompt-title")
                            TextField("Command", text: $command).textInputAutocapitalization(.never).autocorrectionDisabled()
                                .accessibilityIdentifier("prompt-command")
                            TextField("Instructions", text: $prompt, axis: .vertical).lineLimit(5...12)
                                .accessibilityIdentifier("prompt-body")
                        }
                        if let validationError { Text(validationError).foregroundStyle(Theme.danger) }
                    }
                    .scrollContentBackground(.hidden).background(Theme.background)
                    .navigationTitle(editing == nil ? "New prompt" : "Edit prompt")
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) { Button("Cancel") { adding = false } }
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Save") {
                                let normalized = LocalPromptTemplate.normalizeCommand(command)
                                guard !store.templates.contains(where: { $0.command == normalized && $0.id != editing?.id }) else {
                                    validationError = "Choose a unique command."; return
                                }
                                if var existing = editing {
                                    existing.command = normalized; existing.title = title; existing.body = prompt; store.update(existing)
                                } else { _ = store.add(command: normalized, title: title, body: prompt) }
                                adding = false
                            }.disabled(command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        }
                    }
                }
            }
        }
    }
}
