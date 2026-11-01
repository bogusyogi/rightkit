import SwiftUI

/// Renders a unified diff with per-line coloring — the phone-sized sibling of the PWA's
/// `DiffView`. Pure line classification over the raw diff text: additions on a green tint,
/// removals on red, hunk headers muted, context plain. Lines never wrap (a wrapped diff is
/// unreadable); the block scrolls horizontally like a code block.
struct DiffText: View {
    let diff: String

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    Text(line.text.isEmpty ? " " : line.text)
                        .font(.system(.footnote, design: .monospaced))
                        .foregroundStyle(line.kind.foreground)
                        .fixedSize(horizontal: true, vertical: false)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(line.kind.background)
                }
            }
            .padding(.vertical, 6)
        }
        .background(Theme.background.opacity(0.6), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .stroke(Theme.border, lineWidth: 1)
        )
        .accessibilityLabel("Proposed change diff")
    }

    private var lines: [DiffLine] {
        diff.components(separatedBy: "\n").map { DiffLine(text: $0, kind: DiffLineKind(of: $0)) }
    }
}

struct DiffLine: Equatable {
    let text: String
    let kind: DiffLineKind
}

enum DiffLineKind: Equatable {
    case addition, removal, hunk, meta, context

    /// Classification must check file headers BEFORE +/-: "+++ b/x" and "--- a/x" open with
    /// the same characters as added/removed lines but are metadata.
    init(of line: String) {
        if line.hasPrefix("+++") || line.hasPrefix("---") {
            self = .meta
        } else if line.hasPrefix("@@") {
            self = .hunk
        } else if line.hasPrefix("+") {
            self = .addition
        } else if line.hasPrefix("-") {
            self = .removal
        } else {
            self = .context
        }
    }

    var foreground: Color {
        switch self {
        case .addition: return Theme.accent2
        case .removal: return Theme.danger
        case .hunk, .meta: return Theme.muted
        case .context: return Theme.text
        }
    }

    var background: Color {
        switch self {
        case .addition: return Theme.accent2.opacity(0.10)
        case .removal: return Theme.danger.opacity(0.10)
        case .hunk, .meta, .context: return .clear
        }
    }
}
