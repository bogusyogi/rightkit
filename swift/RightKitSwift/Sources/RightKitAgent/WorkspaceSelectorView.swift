import SwiftUI

/// Native workspace chooser for desktop paths. It presents remembered roots and a manual
/// field; it deliberately has no phone filesystem browser.
@MainActor
public struct WorkspaceSelectorView: View {
    let existingRoots: [String]
    @Binding var selection: String
    let history: WorkspaceHistory
    @State private var bookmarks: [WorkspaceBookmark] = []

    public init(
        existingRoots: [String],
        selection: Binding<String>,
        history: WorkspaceHistory = .shared
    ) {
        self.existingRoots = existingRoots
        _selection = selection
        self.history = history
    }

    public var body: some View {
        Group {
            if let current = history.current() {
                Section("Current workspace") {
                    Button {
                        selection = current.root
                    } label: {
                        workspaceRow(current, isCurrent: true)
                    }
                    .buttonStyle(.plain)
                }
            }

            if !bookmarks.isEmpty {
                Section("Recent workspaces") {
                    ForEach(bookmarks) { bookmark in
                        Button {
                            selection = bookmark.root
                        } label: {
                            workspaceRow(bookmark, isCurrent: bookmark.root == selection)
                        }
                        .buttonStyle(.plain)
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button(role: .destructive) {
                                history.remove(root: bookmark.root)
                                reload()
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                    }
                }
            }

            if !availableExistingRoots.isEmpty {
                Section("Used by existing sessions") {
                    ForEach(availableExistingRoots, id: \.self) { root in
                        Button {
                            selection = root
                        } label: {
                            HStack {
                                Image(systemName: "folder")
                                    .foregroundStyle(Theme.muted)
                                Text(root)
                                    .lineLimit(1)
                                    .truncationMode(.head)
                                    .foregroundStyle(Theme.text)
                                Spacer()
                                if selection == root {
                                    Image(systemName: "checkmark")
                                        .foregroundStyle(Theme.accent)
                                }
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
            }

            Section("Workspace root (on the Mac)") {
                TextField("/path/to/repo", text: $selection)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier("new-session-root")
                Text("Enter a path on your paired Mac. CodeRight does not browse phone files.")
                    .font(.footnote)
                    .foregroundStyle(Theme.muted)
            }
        }
        .onAppear(perform: load)
    }

    private var availableExistingRoots: [String] {
        var seen = Set<String>()
        let remembered = Set(bookmarks.map(\.root))
        return existingRoots.compactMap { raw in
            let root = WorkspaceBookmark.normalizeRoot(raw)
            guard !root.isEmpty, !remembered.contains(root), seen.insert(root).inserted else { return nil }
            return root
        }
    }

    @ViewBuilder
    private func workspaceRow(_ bookmark: WorkspaceBookmark, isCurrent: Bool) -> some View {
        HStack {
            Image(systemName: isCurrent ? "checkmark.circle.fill" : "clock")
                .foregroundStyle(isCurrent ? Theme.accent2 : Theme.muted)
            VStack(alignment: .leading, spacing: 2) {
                Text(bookmark.displayName)
                    .foregroundStyle(Theme.text)
                Text(bookmark.root)
                    .font(.caption)
                    .foregroundStyle(Theme.muted)
                    .lineLimit(1)
                    .truncationMode(.head)
            }
            Spacer()
            if selection == bookmark.root {
                Image(systemName: "checkmark")
                    .foregroundStyle(Theme.accent)
            }
        }
        .contentShape(Rectangle())
    }

    private func load() {
        bookmarks = history.bookmarks()
        if selection.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            selection = history.current()?.root ?? availableExistingRoots.first ?? ""
        }
    }

    private func reload() {
        bookmarks = history.bookmarks()
    }
}
