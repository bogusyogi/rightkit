import Foundation
import RightKitFS

struct Artifact: Identifiable, Codable, Sendable, Equatable {
    let id: UUID
    var title: String
    var kind: JobKind
    var sourceInput: String
    var createdAt: Date
    /// Which ASR engine produced this transcript, when one did. Nil for OCR /
    /// PDF / article artifacts. Surfaced as a badge so engine comparisons read
    /// off the library instead of memory.
    var asrEngine: ASREngine?
}

/// The library: every finished job persists as a folder
/// `artifacts/<id>/` holding `text.txt` + `artifact.json`, plus a flat
/// `index.json` for listing. Files stay on disk until the user deletes them
/// (desktop parity). History is unlimited on every tier.
@MainActor
final class ArtifactStore: ObservableObject {
    @Published private(set) var artifacts: [Artifact] = []

    private let root: RightKitAppRoot
    private let folder: String
    private var indexPath: String { "\(folder)/index.json" }

    init(root: RightKitAppRoot, folder: String = "artifacts") {
        self.root = root
        self.folder = folder
        _ = try? root.dataDir(folder)
        if let indexURL = try? root.path("\(folder)/index.json"),
           let data = try? Data(contentsOf: indexURL),
           let decoded = try? JSONDecoder.srStable.decode([Artifact].self, from: data) {
            artifacts = decoded
        }
    }

    @discardableResult
    func save(outcome: JobOutcome, job: Job) throws -> Artifact {
        let artifact = Artifact(
            id: UUID(),
            title: outcome.title,
            kind: outcome.kind,
            sourceInput: outcome.sourceInput,
            createdAt: Date(),
            asrEngine: outcome.asrEngine
        )
        try root.atomicWrite(Data(outcome.text.utf8), to: relativePath(for: artifact.id, "text.txt"))
        try root.atomicWrite(
            try JSONEncoder.srStable.encode(artifact),
            to: relativePath(for: artifact.id, "artifact.json")
        )
        artifacts.insert(artifact, at: 0)
        persistIndex()
        return artifact
    }

    func artifact(id: UUID) -> Artifact? {
        artifacts.first { $0.id == id }
    }

    func text(for artifact: Artifact) -> String {
        (try? String(contentsOf: textFileURL(for: artifact), encoding: .utf8)) ?? ""
    }

    func textFileURL(for artifact: Artifact) -> URL {
        let rel = relativePath(for: artifact.id, "text.txt")
        return (try? root.path(rel)) ?? root.root.appendingPathComponent(rel)
    }

    func delete(_ artifact: Artifact) {
        if let url = try? root.path("\(folder)/\(artifact.id.uuidString)") {
            try? FileManager.default.removeItem(at: url)
        }
        artifacts.removeAll { $0.id == artifact.id }
        persistIndex()
    }

    // MARK: - Internals

    private func relativePath(for id: UUID, _ file: String) -> String {
        "\(folder)/\(id.uuidString)/\(file)"
    }

    private func persistIndex() {
        if let data = try? JSONEncoder.srStable.encode(artifacts) {
            try? root.atomicWrite(data, to: indexPath)
        }
    }
}
