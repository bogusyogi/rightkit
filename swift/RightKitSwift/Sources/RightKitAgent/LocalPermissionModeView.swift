import SwiftUI

/// Explicit permission-mode selector for local sessions.
///
/// Mirrors `PermissionMode` in `engine/crates/policy/src/types.rs` exactly —
/// five modes, not the four the inline picker in `LocalSessionLauncher`
/// exposed. `Bypass` is included because the engine accepts it, but it is
/// visually flagged: it is "only for an explicit sandbox / disposable
/// worktree" per the Rust doc comment, and a phone workspace is neither.
///
/// Wire values match `parse_permission_mode_strict` (case-insensitive, so the
/// exact casing here is cosmetic but kept consistent with what
/// `LocalSessionLauncher` already sends).
enum LocalPermissionMode: String, CaseIterable, Identifiable {
    case plan
    case ask
    case acceptEdits
    case autoSafe
    case bypass

    var id: String { rawValue }

    var title: String {
        switch self {
        case .plan: return "Plan only"
        case .ask: return "Ask for changes"
        case .acceptEdits: return "Accept edits"
        case .autoSafe: return "Auto-safe"
        case .bypass: return "Bypass"
        }
    }

    /// Kept in sync with the doc comments on `PermissionMode` in
    /// `engine/crates/policy/src/types.rs`.
    var detail: String {
        switch self {
        case .plan:
            return "Read/plan only — no mutation, ever, even with approval."
        case .ask:
            return "Mutating tools pause for your approval. The default."
        case .acceptEdits:
            return "File edits auto-apply; shell and destructive actions still ask."
        case .autoSafe:
            return "Non-destructive mutations auto-apply; destructive or process-control actions still ask."
        case .bypass:
            return "Everything is allowed automatically, including destructive actions. Only for a disposable worktree you can afford to lose."
        }
    }

    var isDangerous: Bool { self == .bypass }

    var systemImage: String {
        switch self {
        case .plan: return "eye"
        case .ask: return "hand.raised"
        case .acceptEdits: return "pencil"
        case .autoSafe: return "checkmark.shield"
        case .bypass: return "exclamationmark.triangle.fill"
        }
    }
}

/// Full-screen picker, in the style of `LocalModelPickerView`: a list with
/// descriptions rather than a compact segmented control, because the
/// difference between `autoSafe` and `acceptEdits` is not obvious from the
/// label alone and getting it wrong changes what an agent can do to the
/// user's files unattended.
struct LocalPermissionModeView: View {
    @Binding var selection: String

    @Environment(\.dismiss) private var dismiss

    private var current: LocalPermissionMode {
        LocalPermissionMode(rawValue: selection) ?? .ask
    }

    var body: some View {
        NavigationStack {
            List {
                ForEach(LocalPermissionMode.allCases) { mode in
                    Button {
                        selection = mode.rawValue
                        dismiss()
                    } label: {
                        HStack(alignment: .top, spacing: 12) {
                            Image(systemName: mode.systemImage)
                                .foregroundStyle(mode.isDangerous ? Theme.danger : Theme.accent)
                                .frame(width: 22)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(mode.title)
                                    .foregroundStyle(Theme.text)
                                Text(mode.detail)
                                    .font(.caption)
                                    .foregroundStyle(Theme.muted)
                            }
                            Spacer()
                            if mode == current {
                                Image(systemName: "checkmark")
                                    .foregroundStyle(Theme.accent)
                            }
                        }
                    }
                    .accessibilityIdentifier("local-permission-mode-\(mode.rawValue)")
                }
            }
            .navigationTitle("Permission Mode")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}

/// Compact row for embedding inline in a `Form` — used by
/// `LocalSessionLauncher` in place of the old 4-way inline `Picker`, which
/// silently excluded `Bypass`.
struct LocalPermissionModeRow: View {
    @Binding var selection: String
    @State private var showPicker = false

    private var current: LocalPermissionMode {
        LocalPermissionMode(rawValue: selection) ?? .ask
    }

    var body: some View {
        Button {
            showPicker = true
        } label: {
            HStack {
                Label("Mode", systemImage: current.systemImage)
                    .foregroundStyle(Theme.text)
                Spacer()
                Text(current.title)
                    .foregroundStyle(current.isDangerous ? Theme.danger : Theme.muted)
            }
        }
        .accessibilityIdentifier("local-permission-mode-row")
        .sheet(isPresented: $showPicker) {
            LocalPermissionModeView(selection: $selection)
        }
    }
}
