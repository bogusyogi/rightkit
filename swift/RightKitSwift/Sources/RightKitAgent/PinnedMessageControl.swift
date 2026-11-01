import SwiftUI

/// Pin/unpin control for a single transcript message (ENG-104). Pinning a
/// message marks it durable through the engine's `PinnedContextRegistry`
/// (`engine/crates/engine/src/pinned_context.rs`), which is reapplied onto
/// the transcript's own `pinned` flag before every compaction pass -- the
/// same flag `compactor_preservation::is_pinned` already treats as
/// "preserved verbatim". This view has no opinion on that mechanism; it is
/// only the tap target plus optimistic local state.
struct PinnedMessageControl: View {
    let messageId: String
    @Binding var isPinned: Bool
    /// Called after the local toggle, with the *new* desired state. Wire
    /// this to whatever session-control call reaches
    /// `PinnedContextRegistry::pin` / `unpin` once that FFI surface exists
    /// (no such binding exists yet in `CodeRightMobileRuntime.h` -- see the
    /// ENG-104 gap note in the C7 dispatch report). On failure the caller
    /// should flip `isPinned` back; this view does not retry on its own.
    var onToggle: (String, Bool) -> Void = { _, _ in }

    var body: some View {
        Button {
            let next = !isPinned
            isPinned = next
            onToggle(messageId, next)
        } label: {
            Image(systemName: isPinned ? "pin.fill" : "pin")
                .font(.caption)
                .foregroundStyle(isPinned ? Theme.accent : Theme.muted)
                .frame(width: 28, height: 28)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(isPinned ? "Unpin message" : "Pin message")
        .accessibilityAddTraits(isPinned ? [.isSelected] : [])
        .accessibilityHint(
            isPinned
                ? "Removes this message from pinned context; it may be summarized by compaction."
                : "Keeps this message verbatim through context compaction."
        )
    }
}

/// Compact inline badge for a message already known to be pinned, for use
/// in message bubbles that only need to *show* pin state (e.g. inside a
/// read-only transcript view) without offering the toggle affordance.
struct PinnedMessageBadge: View {
    var body: some View {
        Label("Pinned", systemImage: "pin.fill")
            .font(.caption2.weight(.medium))
            .labelStyle(.titleAndIcon)
            .foregroundStyle(Theme.accent)
            .accessibilityLabel("Pinned message")
    }
}

#Preview {
    struct PreviewHost: View {
        @State private var pinned = false
        var body: some View {
            HStack {
                PinnedMessageControl(messageId: "m1", isPinned: $pinned)
                if pinned {
                    PinnedMessageBadge()
                }
            }
            .padding()
        }
    }
    return PreviewHost()
}
