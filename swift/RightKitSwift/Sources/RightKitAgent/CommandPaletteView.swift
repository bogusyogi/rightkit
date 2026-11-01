import SwiftUI

/// APP-14: command palette UI. Renders the merged, query-filtered results from
/// `CommandPaletteStore`'s pluggable providers and dispatches the chosen result's kind back
/// to the caller — the view has no knowledge of what an "action" or "route" actually does.
@MainActor
public struct CommandPaletteView: View {
    @Bindable private var store: CommandPaletteStore
    @Environment(\.dismiss) private var dismiss
    private let onSelect: (CommandPaletteResult) -> Void

    public init(store: CommandPaletteStore, onSelect: @escaping (CommandPaletteResult) -> Void) {
        self.store = store
        self.onSelect = onSelect
    }

    public var body: some View {
        NavigationStack {
            List(store.results) { result in
                Button {
                    onSelect(result)
                    dismiss()
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(result.title)
                            .font(.body)
                            .foregroundStyle(Theme.text)
                        if let subtitle = result.subtitle {
                            Text(subtitle)
                                .font(.caption)
                                .foregroundStyle(Theme.muted)
                                .lineLimit(2)
                        }
                    }
                }
                .accessibilityIdentifier("command-palette-result-\(result.id)")
            }
            .searchable(text: $store.query, prompt: "Type a command or search")
            .navigationTitle("Command Palette")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .overlay {
                if store.results.isEmpty {
                    ContentUnavailableView(
                        "No matches",
                        systemImage: "magnifyingglass",
                        description: Text("Try a different search term.")
                    )
                }
            }
            .background(Theme.background)
        }
    }
}
