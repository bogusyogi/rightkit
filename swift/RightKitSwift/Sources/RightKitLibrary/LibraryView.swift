#if os(iOS)
import SwiftUI

/// History of every finished job. Unlimited on every tier — desktop parity.
struct LibraryView: View {
    @EnvironmentObject private var artifacts: ArtifactStore
    @State private var query = ""

    var body: some View {
        NavigationStack {
            List {
                ForEach(filtered) { artifact in
                    NavigationLink(value: artifact) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(artifact.title)
                                .font(.srBody(15, weight: .medium))
                                .foregroundStyle(Color.srText)
                                .lineLimit(2)
                            HStack(spacing: 6) {
                                Text(artifact.createdAt.formatted(date: .abbreviated, time: .shortened))
                                    .font(.srMono(11))
                                    .foregroundStyle(Color.srMuted)
                                if let engine = artifact.asrEngine {
                                    EngineBadge(engine: engine)
                                }
                            }
                        }
                    }
                    .listRowBackground(Color.srSurface)
                    .swipeActions {
                        Button(role: .destructive) {
                            artifacts.delete(artifact)
                        } label: {
                            Label("Delete", systemImage: "trash")
                        }
                    }
                }
            }
            .scrollContentBackground(.hidden)
            // Search has the system Cancel button; scrolling to read results
            // should get the keyboard out of the way too.
            .scrollDismissesKeyboard(.immediately)
            .background(Color.srBackground)
            .navigationTitle("Library")
            .searchable(text: $query)
            .navigationDestination(for: Artifact.self) { artifact in
                ArtifactDetailView(artifact: artifact)
            }
            .overlay {
                if artifacts.artifacts.isEmpty {
                    ContentUnavailableView(
                        "Nothing extracted yet",
                        systemImage: "books.vertical",
                        description: Text("Finished jobs land here and stay on this device.")
                    )
                }
            }
        }
    }

    private var filtered: [Artifact] {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return artifacts.artifacts }
        return artifacts.artifacts.filter {
            $0.title.localizedCaseInsensitiveContains(trimmed)
                || $0.sourceInput.localizedCaseInsensitiveContains(trimmed)
        }
    }
}

/// Which engine produced a transcript, at a glance — the point of the
/// bake-off is that you never have to remember what the toggle said.
struct EngineBadge: View {
    let engine: ASREngine

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: engine.badgeSymbol)
                .font(.system(size: 9))
            Text(engine.displayName)
                .font(.srMono(10))
        }
        .padding(.horizontal, 5)
        .padding(.vertical, 2)
        .background(Color.srAccent.opacity(0.14), in: Capsule())
        .foregroundStyle(Color.srAccent)
        .accessibilityLabel("Transcribed with \(engine.displayName)")
    }
}

struct ArtifactDetailView: View {
    @EnvironmentObject private var artifacts: ArtifactStore
    let artifact: Artifact

    private var text: String { artifacts.text(for: artifact) }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                sectionLabel("Source")
                // The library keeps text, not the imported file, so a URL job
                // shows a live link and a file job shows what it came from.
                OriginalPreview(fileURL: nil, sourceInput: artifact.sourceInput)

                HStack(spacing: 8) {
                    CopyTextButton(text: text)
                        .font(.srBody(14, weight: .medium))
                    Spacer(minLength: 0)
                    if let engine = artifact.asrEngine {
                        EngineBadge(engine: engine)
                    }
                }
                .padding(.top, 4)

                sectionLabel("Extracted")
                Text(text)
                    .font(.srMono(14))
                    .foregroundStyle(Color.srText)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding()
        }
        .background(Color.srBackground)
        .navigationTitle(artifact.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    CopyTextButton(text: text)
                    ShareLink(item: artifacts.textFileURL(for: artifact)) {
                        Label("Save text (.txt)", systemImage: "doc.text")
                    }
                    ShareLink(
                        item: MarkdownExport(artifact: artifact, text: text),
                        preview: SharePreview(artifact.title)
                    ) {
                        Label("Save Markdown (.md)", systemImage: "doc.richtext")
                    }
                } label: {
                    Image(systemName: "square.and.arrow.up")
                }
                .accessibilityLabel("Share or save")
            }
        }
    }

    private func sectionLabel(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.srMono(11))
            .foregroundStyle(Color.srMuted)
    }
}

extension Artifact: Hashable {
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}
#endif
