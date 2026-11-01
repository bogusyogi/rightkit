import SwiftUI

/// One diff hunk and the optional narration supplied by the local runtime.
/// Keeping `nil` distinct from an empty string lets the view label coverage
/// honestly when the shared small-model budget or provider was unavailable.
public struct LocalDiffWalkthroughHunk: Identifiable, Equatable, Sendable {
    /// Stable identity for SwiftUI diffing when a walkthrough is refreshed.
    public let id: String
    /// The unified-diff range header shown above this hunk.
    public let header: String
    /// The original hunk text rendered by the shared diff view.
    public let diff: String
    /// Plain-language narration, or `nil` when the hunk was not narrated.
    public let narration: String?

    /// Creates the renderable projection of one typed runtime hunk result.
    public init(id: String, header: String, diff: String, narration: String?) {
        self.id = id
        self.header = header
        self.diff = diff
        self.narration = narration
    }
}

/// Presents each unified-diff hunk beside its plain-language narration.
///
/// This is a rendering surface only: no provider or network call is made here.
/// A caller must supply the typed walkthrough result; missing narration stays
/// visibly marked instead of being replaced with a guessed explanation.
@MainActor
public struct LocalDiffWalkthroughView: View {
    private let hunks: [LocalDiffWalkthroughHunk]

    public init(hunks: [LocalDiffWalkthroughHunk]) {
        self.hunks = hunks
    }

    public var body: some View {
        Group {
            if hunks.isEmpty {
                ContentUnavailableView(
                    "No diff hunks",
                    systemImage: "doc.text.magnifyingglass",
                    description: Text("This diff did not contain any narratable hunks.")
                )
            } else {
                List {
                    ForEach(hunks) { hunk in
                        Section {
                            DiffText(diff: hunk.diff)

                            if let narration = hunk.narration,
                               !narration.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                                VStack(alignment: .leading, spacing: 6) {
                                    Label("What changed", systemImage: "text.bubble")
                                        .font(.caption.weight(.semibold))
                                        .foregroundStyle(Theme.accent)
                                    Text(narration)
                                        .font(.subheadline)
                                        .foregroundStyle(Theme.text)
                                        .textSelection(.enabled)
                                }
                                .padding(.vertical, 4)
                                .accessibilityElement(children: .combine)
                                .accessibilityLabel("Narration: \(narration)")
                            } else {
                                Label("Not narrated", systemImage: "exclamationmark.bubble")
                                    .font(.subheadline.weight(.semibold))
                                    .foregroundStyle(Theme.muted)
                                    .accessibilityLabel("This diff hunk was not narrated")
                            }
                        } header: {
                            Text(hunk.header)
                                .font(.system(.caption, design: .monospaced))
                                .foregroundStyle(Theme.muted)
                                .textSelection(.enabled)
                        }
                    }
                }
                .listStyle(.insetGrouped)
            }
        }
        .navigationTitle("Diff walkthrough")
        .navigationBarTitleDisplayMode(.inline)
    }
}
