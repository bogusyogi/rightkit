#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation
import RightKitArtifacts

/// What an install on disk was made from, read from its manifest. Every readiness decision goes through
/// ``matchesPin(_:modelsRoot:)``, so a model is ready only when the archive it came from is the one the
/// build pins, whichever installer wrote it.
enum ModelInstallProof {
    /// The archive SHA-256 (lowercase) the install at `<modelsRoot>/<directoryName>` was made from: the
    /// `.rightkit-manifest.json` of ``ArtifactInstaller`` when there is one, otherwise the `manifest.json`
    /// the earlier in-app installer wrote (``LegacyModelInstall``). Nil when neither records a digest.
    static func installedDigest(_ spec: ModelArtifactSpec, modelsRoot: URL) -> String? {
        if let manifest = try? ArtifactInstaller.readManifest(destinationRoot: modelsRoot, topLevelDirectory: spec.directoryName) {
            return manifest.archiveSHA256?.lowercased()
        }
        return LegacyModelInstall.installedDigest(spec, modelsRoot: modelsRoot)
    }

    /// True only when the spec pins a digest and the install records the same one.
    static func matchesPin(_ spec: ModelArtifactSpec, modelsRoot: URL) -> Bool {
        guard let pinned = spec.expectedSHA256?.lowercased(),
              let installed = installedDigest(spec, modelsRoot: modelsRoot) else { return false }
        return installed == pinned
    }
}

/// The on-disk record of the app's earlier in-app installer, which predates this owner:
///
/// - `<models>/<dir>/manifest.json`: `{"backend":..., "platform":..., "route":..., "sha256":"<archive digest>", "status":"installed"}`;
/// - `<models>/.<dir>.previous-aside`: the previous install, moved aside while a replacement is activated;
/// - `<models>/.<dir>.replacement-pending`: a marker file, present from the activation until Core ML
///   validation of the replacement settled.
///
/// Nothing writes these any more, but a device that was updated mid-replacement, or holds a model installed
/// by that build, still has them.
enum LegacyModelInstall {
    static let manifestFileName = "manifest.json"

    static func asideURL(_ spec: ModelArtifactSpec, modelsRoot: URL) -> URL {
        modelsRoot.appendingPathComponent(".\(spec.directoryName).previous-aside", isDirectory: true)
    }

    static func pendingURL(_ spec: ModelArtifactSpec, modelsRoot: URL) -> URL {
        modelsRoot.appendingPathComponent(".\(spec.directoryName).replacement-pending")
    }

    /// The archive digest recorded by the earlier installer's manifest, lowercase; nil when absent or unreadable.
    static func installedDigest(_ spec: ModelArtifactSpec, modelsRoot: URL) -> String? {
        let url = modelsRoot.appendingPathComponent(spec.directoryName, isDirectory: true).appendingPathComponent(manifestFileName)
        guard let data = try? Data(contentsOf: url),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let digest = object["sha256"] as? String else { return nil }
        let value = digest.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return value.isEmpty ? nil : value
    }

    /// Settles an activation the earlier installer left unresolved. Safe to call on every launch and a
    /// no-op when nothing is left.
    ///
    /// - no aside: a stray marker is removed;
    /// - aside, no install: the process died between the two moves, so the previous install is put back;
    /// - aside and install, no marker: the replacement settled, the aside is stale and is removed;
    /// - aside, install and marker: the replacement was activated but its validation never settled. When it
    ///   is complete and carries the ready marker it was validated, and the aside is dropped. Otherwise it
    ///   is discarded and the previous install is put back.
    static func recover(_ spec: ModelArtifactSpec, modelsRoot: URL) throws {
        let manager = FileManager.default
        let aside = asideURL(spec, modelsRoot: modelsRoot)
        let pending = pendingURL(spec, modelsRoot: modelsRoot)
        let destination = modelsRoot.appendingPathComponent(spec.directoryName, isDirectory: true)
        guard manager.fileExists(atPath: aside.path) else {
            if manager.fileExists(atPath: pending.path) { try? manager.removeItem(at: pending) }
            return
        }
        if !manager.fileExists(atPath: destination.path) {
            try manager.moveItem(at: aside, to: destination)
            try? manager.removeItem(at: pending)
            return
        }
        if !manager.fileExists(atPath: pending.path) {
            try manager.removeItem(at: aside)
            return
        }
        if spec.isStructurallyInstalled(at: destination), CoreMLModelValidator.hasReadyMarker(at: destination) {
            try manager.removeItem(at: aside)
            try? manager.removeItem(at: pending)
        } else {
            try manager.removeItem(at: destination)
            try manager.moveItem(at: aside, to: destination)
            try? manager.removeItem(at: pending)
        }
    }
}
#endif
