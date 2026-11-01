import Foundation
import Observation
import SwiftUI

/// Swift mirror of `ArtifactRow` (`engine/crates/protocol/src/artifact.rs`)
/// plus the CRUD bookkeeping `engine/crates/tools/src/artifact_crud.rs` packs
/// into `ArtifactRow.meta` as `ArtifactCrudMeta` JSON, decoded inline here
/// via `CodingKeys`/`init(from:)` rather than as a nested type, since the
/// wire shape keeps `version`/`updatedAt`/`sessionId` flattened onto the row
/// (see `artifact_crud::row_to_json`) rather than nested under a `meta` key.
public struct ArtifactRecord: Identifiable, Equatable, Codable, Sendable {
    public var id: String
    public var path: String
    public var kind: String
    public var contentHash: String
    public var shippable: Bool
    public var producer: String
    public var provenance: String
    public var trustLabel: String
    public var createdAt: String
    public var version: Int
    public var updatedAt: String
    public var sessionId: String
    /// Present only on a single-artifact read (`artifact_read`), absent on
    /// list rows (`artifact_list`) to keep listing payloads small.
    public var content: String?

    public init(
        id: String,
        path: String,
        kind: String,
        contentHash: String,
        shippable: Bool,
        producer: String,
        provenance: String,
        trustLabel: String,
        createdAt: String,
        version: Int,
        updatedAt: String,
        sessionId: String,
        content: String? = nil
    ) {
        self.id = id
        self.path = path
        self.kind = kind
        self.contentHash = contentHash
        self.shippable = shippable
        self.producer = producer
        self.provenance = provenance
        self.trustLabel = trustLabel
        self.createdAt = createdAt
        self.version = version
        self.updatedAt = updatedAt
        self.sessionId = sessionId
        self.content = content
    }
}

/// Where an `ArtifactRecord` lives on disk, mirroring
/// `engine/crates/tools/src/artifact_crud.rs`'s layout exactly:
/// `<workspace>/.coderight/artifacts/<id>/{meta.json,content}`. Kept as a
/// small value type so both the store below and the File Provider
/// extension's enumerator/item (`FileProviderExtension/**`, same lane) agree
/// on one path scheme without either importing the other's module — the
/// extension runs in a separate process and cannot depend on app-target
/// Swift files.
///
/// NOTE (integration gap): this store talks to the artifact directory
/// directly with `FileManager`, the same way `LocalWorkspaceStore` already
/// manages workspace folders, rather than through a Rust FFI call into
/// `artifact_crud`'s tools. `engine/crates/mobile_runtime/src/ffi.rs` has no
/// `coderight_mobile_artifact_*` entry points and is outside this lane's
/// file allowlist. That means a create/update performed here does **not**
/// go through `FileVersionGuard`'s capture-check-write the way the Rust
/// tool does — see the file-level doc below for the exact risk and the
/// follow-up this leaves for the FFI owner.
public enum ArtifactStorageLayout {
    public static func artifactsRoot(in workspace: URL) -> URL {
        workspace.appendingPathComponent(".coderight/artifacts", isDirectory: true)
    }

    public static func artifactDirectory(in workspace: URL, id: String) -> URL {
        artifactsRoot(in: workspace).appendingPathComponent(id, isDirectory: true)
    }

    public static func metaURL(_ artifactDirectory: URL) -> URL {
        artifactDirectory.appendingPathComponent("meta.json", isDirectory: false)
    }

    public static func contentURL(_ artifactDirectory: URL) -> URL {
        artifactDirectory.appendingPathComponent("content", isDirectory: false)
    }
}

/// Lists and previews artifacts for one on-device workspace (ENG-115).
///
/// This reads the same on-disk `meta.json`/`content` files
/// `engine/crates/tools/src/artifact_crud.rs` writes during an agent turn —
/// it is a read/list surface first, with create/update kept intentionally
/// minimal (see the class doc) until the FFI gap above is closed. That gap
/// is the reason `create`/`update` here are best-effort, UI-triggered
/// conveniences (e.g. "duplicate as new artifact"), not the primary way an
/// agent produces artifacts — that path is, and stays, the `artifact_create`
/// / `artifact_update` tools running inside a turn.
@MainActor
@Observable
public final class ArtifactSurfaceStore {
    public private(set) var artifacts: [ArtifactRecord] = []
    public var lastError: String?

    private let workspace: URL
    private let fileManager: FileManager

    public init(workspace: URL, fileManager: FileManager = .default) {
        self.workspace = workspace
        self.fileManager = fileManager
    }

    /// Re-scan `.coderight/artifacts` under the workspace. Corrupt or
    /// half-written rows (e.g. a create observed mid-write) are skipped
    /// rather than surfaced as an error — a listing view should degrade
    /// gracefully, not blank out because one artifact is transiently odd.
    public func reload() {
        let root = ArtifactStorageLayout.artifactsRoot(in: workspace)
        guard let entries = try? fileManager.contentsOfDirectory(
            at: root,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        ) else {
            artifacts = []
            return
        }

        var loaded: [ArtifactRecord] = []
        for directory in entries {
            let metaURL = ArtifactStorageLayout.metaURL(directory)
            guard let data = try? Data(contentsOf: metaURL),
                  let record = try? JSONDecoder().decode(ArtifactRecord.self, from: data)
            else { continue }
            loaded.append(record)
        }
        artifacts = loaded.sorted { $0.updatedAt > $1.updatedAt }
    }

    /// Load full content for one artifact (list rows omit it to stay small).
    public func loadContent(for record: ArtifactRecord) -> String? {
        let directory = ArtifactStorageLayout.artifactDirectory(in: workspace, id: record.id)
        return try? String(contentsOf: ArtifactStorageLayout.contentURL(directory), encoding: .utf8)
    }

    /// Move an artifact directory to `.coderight/artifacts_trash`, mirroring
    /// `artifact_delete`'s soft-delete convention exactly so either side
    /// (agent tool call or this UI action) leaves the same recoverable trace.
    public func delete(_ record: ArtifactRecord) {
        let directory = ArtifactStorageLayout.artifactDirectory(in: workspace, id: record.id)
        let trashRoot = workspace.appendingPathComponent(".coderight/artifacts_trash", isDirectory: true)
        do {
            try fileManager.createDirectory(at: trashRoot, withIntermediateDirectories: true)
            let target = trashRoot.appendingPathComponent("\(record.id)-\(Int(Date().timeIntervalSince1970 * 1_000_000_000))")
            try fileManager.moveItem(at: directory, to: target)
            artifacts.removeAll { $0.id == record.id }
            lastError = nil
        } catch {
            lastError = "Could not delete artifact: \(error.localizedDescription)"
        }
    }
}

/// Lists every artifact in the current workspace with an inline preview on
/// selection (ENG-115).
public struct ArtifactSurfaceView: View {
    @State private var store: ArtifactSurfaceStore
    @State private var selected: ArtifactRecord?

    public init(workspace: URL) {
        _store = State(initialValue: ArtifactSurfaceStore(workspace: workspace))
    }

    public var body: some View {
        List {
            if let error = store.lastError {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(Theme.danger)
                }
            }
            if store.artifacts.isEmpty {
                Section {
                    Text("No artifacts yet. An agent creates one with artifact_create; it appears here.")
                        .foregroundStyle(Theme.muted)
                        .font(.subheadline)
                }
            } else {
                Section {
                    ForEach(store.artifacts) { artifact in
                        Button {
                            selected = artifact
                        } label: {
                            ArtifactRow(artifact: artifact)
                        }
                        .buttonStyle(.plain)
                        .swipeActions(edge: .trailing) {
                            Button(role: .destructive) {
                                store.delete(artifact)
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                    }
                }
            }
        }
        .listStyle(.plain)
        .navigationTitle("Artifacts")
        .task { store.reload() }
        .refreshable { store.reload() }
        .sheet(item: $selected) { artifact in
            NavigationStack {
                ArtifactInlinePreviewView(
                    artifact: artifact,
                    content: store.loadContent(for: artifact) ?? ""
                )
                .navigationTitle(artifact.path)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Close") { selected = nil }
                    }
                }
            }
        }
    }
}

private struct ArtifactRow: View {
    let artifact: ArtifactRecord

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: ArtifactKindIcon.systemName(for: artifact.kind))
                .foregroundStyle(Theme.accent)
                .frame(width: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(artifact.path)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Text("\(artifact.kind) · v\(artifact.version)")
                    .font(.caption)
                    .foregroundStyle(Theme.muted)
            }
            Spacer()
            if artifact.shippable {
                Image(systemName: "checkmark.seal.fill")
                    .foregroundStyle(Theme.accent2)
                    .accessibilityLabel("Shippable")
            }
        }
        .padding(.vertical, 4)
    }
}

/// Maps an artifact `kind` string to an SF Symbol, shared by the list row
/// and the inline preview header so the two surfaces never drift.
enum ArtifactKindIcon {
    static func systemName(for kind: String) -> String {
        switch kind {
        case "markdown": return "doc.richtext"
        case "diff": return "arrow.triangle.branch"
        case "html-preview": return "safari"
        case "test_report": return "checklist"
        case "screenshot": return "photo"
        case "cost_report": return "dollarsign.circle"
        case "plan": return "list.bullet.rectangle"
        default: return "doc"
        }
    }
}
