import SwiftUI

/// Create a session on the daemon. The phone can't browse the Mac's filesystem, so the
/// workspace-root picker offers the distinct roots already in use by existing sessions,
/// with manual entry as the fallback for a fresh path.
@MainActor
struct NewSessionSheet: View {
    let existingRoots: [String]
    let onCreate: (_ title: String, _ workspaceRoot: String) async -> String?
    let history: WorkspaceHistory

    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State private var root = ""
    @State private var creating = false
    @State private var errorMessage: String?

    init(
        existingRoots: [String],
        history: WorkspaceHistory = .shared,
        onCreate: @escaping (_ title: String, _ workspaceRoot: String) async -> String?
    ) {
        self.existingRoots = existingRoots
        self.history = history
        self.onCreate = onCreate
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("Title") {
                    TextField("What is this session for?", text: $title)
                        .accessibilityIdentifier("new-session-title")
                }
                WorkspaceSelectorView(existingRoots: existingRoots, selection: $root, history: history)
                if let errorMessage {
                    Section {
                        Text(errorMessage)
                            .font(.footnote)
                            .foregroundStyle(Theme.accent)
                    }
                }
                // Primary action is in the navigation bar — a form-bottom button is
                // unreachable under the keyboard accessory while typing.
            }
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .scrollDismissesKeyboard(.interactively)
            .toolbar {
                ToolbarItemGroup(placement: .keyboard) {
                    Spacer()
                    Button("Done") { KeyboardDismiss.now() }
                        .tint(Theme.accent)
                }
            }
            .navigationTitle("New Session")
            .navigationBarTitleDisplayMode(.inline)
            .onAppear {
                if root.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    root = history.current()?.root ?? ""
                }
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if creating {
                        ProgressView()
                    } else {
                        Button("Create") { Task { await create() } }
                            .accessibilityIdentifier("new-session-create")
                            .disabled(trimmedRoot.isEmpty)
                            .fontWeight(.semibold)
                    }
                }
            }
        }
    }

    private var trimmedRoot: String {
        root.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func create() async {
        creating = true
        defer { creating = false }
        guard let draft = WorkspaceSessionDraft(title: title, workspaceRoot: trimmedRoot) else {
            errorMessage = "Choose a workspace root on the Mac."
            return
        }
        if await onCreate(draft.title, draft.workspaceRoot) != nil {
            if let bookmark = history.record(root: draft.workspaceRoot, name: draft.title) {
                history.setCurrent(bookmark)
            }
            dismiss()
        } else {
            errorMessage = "The daemon rejected the session. Check the workspace root path."
        }
    }
}
