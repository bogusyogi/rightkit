#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation
import os

/// Publishes an installed and Core ML validated model into an App Group container so sibling apps reuse it
/// instead of downloading their own copy. `copyItem` clones on APFS, so publishing costs no meaningful disk.
/// It does nothing when the entitlement, the model, the ready marker or the pinned-digest proof is absent.
///
/// The published layout is `<container>/<subpath>/<directoryName>`; sibling apps read the same path, so the
/// host passes the suite-wide `groupID` and `subpath` (see ``SharedModelGroup``).
public enum SharedModelPublisher {
    private static let attempted = OSAllocatedUnfairLock(initialState: Set<String>())

    /// Fire-and-forget publish, at most once per model and digest per launch, off the main thread.
    public static func publishIfReadyOnce(
        spec: ModelArtifactSpec,
        layout: ModelStoreLayout,
        groupID: String,
        subpath: String,
        event: @escaping @Sendable (String, [String: any Sendable]) -> Void = { _, _ in }
    ) {
        let key = [groupID, subpath, spec.directoryName, spec.expectedSHA256 ?? ""].joined(separator: "\u{0}")
        let first = attempted.withLock { $0.insert(key).inserted }
        guard first else { return }
        Task.detached(priority: .utility) {
            publishIfReady(spec: spec, layout: layout, groupID: groupID, subpath: subpath, event: event)
        }
    }

    public static func publishIfReady(
        spec: ModelArtifactSpec,
        layout: ModelStoreLayout,
        groupID: String,
        subpath: String,
        event: @Sendable (String, [String: any Sendable]) -> Void = { _, _ in }
    ) {
        let fileManager = FileManager.default
        let source = layout.modelDirectory(for: spec)
        // Only an install proven to be the pinned archive is shared (``ModelInstallProof``, new or earlier
        // manifest); `spec` must carry the build's pinned digest. An unproven or superseded install is not.
        guard spec.isStructurallyInstalled(at: source),
              CoreMLModelValidator.hasReadyMarker(at: source),
              ModelInstallProof.matchesPin(spec, modelsRoot: layout.modelsRoot),
              let container = fileManager.containerURL(forSecurityApplicationGroupIdentifier: groupID) else { return }

        let components = subpath.split(separator: "/", omittingEmptySubsequences: false)
        guard !components.isEmpty,
              components.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." && !$0.contains("\\") }),
              !spec.directoryName.isEmpty, spec.directoryName != ".", spec.directoryName != "..",
              !spec.directoryName.contains("/"), !spec.directoryName.contains("\\") else { return }
        let root = components.reduce(container) { $0.appendingPathComponent(String($1), isDirectory: true) }
        let destination = root.appendingPathComponent(spec.directoryName, isDirectory: true)
        let aside = root.appendingPathComponent(".\(spec.directoryName).previous-aside", isDirectory: true)
        // A death between the two moves below leaves `destination` missing and the previous publish one move
        // away at this fixed path: promote it back before trusting what is on disk.
        if !fileManager.fileExists(atPath: destination.path), fileManager.fileExists(atPath: aside.path) {
            try? fileManager.moveItem(at: aside, to: destination)
        }
        if spec.isStructurallyInstalled(at: destination), CoreMLModelValidator.hasReadyMarker(at: destination),
           ModelInstallProof.matchesPin(spec, modelsRoot: root) {
            return
        }

        let staging = root.appendingPathComponent(".publishing-\(UUID().uuidString)", isDirectory: true)
        // Never delete `destination` before the replacement is staged: a crash or a failed move between the two
        // steps would otherwise leave sibling apps with no shared copy at all. Move any existing publish aside,
        // commit the new one, and only then drop the aside; restore it if the commit fails.
        var movedAside: URL?
        do {
            try fileManager.createDirectory(at: root, withIntermediateDirectories: true)
            try fileManager.copyItem(at: source, to: staging)
            if fileManager.fileExists(atPath: destination.path) {
                try? fileManager.removeItem(at: aside)
                try fileManager.moveItem(at: destination, to: aside)
                movedAside = aside
            }
            do {
                try fileManager.moveItem(at: staging, to: destination)
            } catch {
                if let movedAside { try? fileManager.moveItem(at: movedAside, to: destination) }
                throw error
            }
            if let movedAside { try? fileManager.removeItem(at: movedAside) }
            event("shared_model_published", ["model": spec.backend])
        } catch {
            try? fileManager.removeItem(at: staging)
            event("shared_model_publish_failed", ["model": spec.backend])
        }
    }
}
#endif
