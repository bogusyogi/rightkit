#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation
import RightKitArtifacts

/// Transactional activation of a downloaded model archive, built on ``ArtifactInstaller``.
///
/// The archive digest is verified before anything is touched; the archive is unpacked into staging and
/// activated with the previous install moved aside; the activated directory is then validated by loading
/// every Core ML stage (``CoreMLModelValidator``). Only a passing validation discards the previous
/// install. A failed or interrupted activation puts the newest working model back, and an aside left by a
/// crash is resolved by ``recoverInterruptedInstall(_:layout:)`` or by the next install. No `.previous`
/// copy outlives a settled install.
public struct ModelInstaller: Sendable {
    typealias Validate = @Sendable (ModelArtifactSpec, URL) async throws -> Void

    private let validate: Validate

    /// Validates with `validator`, off the calling task's executor (stage loading is blocking and slow).
    public init(validator: CoreMLModelValidator = CoreMLModelValidator()) {
        validate = { spec, directory in
            try await Task.detached(priority: .userInitiated) {
                _ = try validator.validateInstalledArtifact(
                    backend: spec.backend, requiredFiles: spec.requiredFiles,
                    requiredStageNames: spec.requiredStageNames, at: directory)
            }.value
        }
    }

    /// Test seam: replaces the Core ML load step.
    init(validate: @escaping Validate) {
        self.validate = validate
    }

    /// Verifies, activates and validates `archive` as the installed model for `spec`, returning the model
    /// directory. `onValidating` is called when activation succeeded and validation begins.
    ///
    /// Throws ``ModelDownloadError/missingExpectedDigest(_:)`` before any work when the spec has no digest,
    /// ``ModelDownloadError/checksumMismatch(expected:actual:)`` for a wrong archive, and an
    /// ``ArtifactError`` for structural or validation failures; in every case the previous install is intact.
    @discardableResult
    public func install(
        archive: URL,
        spec: ModelArtifactSpec,
        layout: ModelStoreLayout,
        onValidating: @escaping @Sendable () -> Void = {}
    ) async throws -> URL {
        guard let digest = spec.expectedSHA256?.lowercased() else {
            throw ModelDownloadError.missingExpectedDigest(spec.directoryName)
        }
        try layout.prepare()
        let validate = self.validate
        let options = InstallOptions(
            requiredFiles: spec.requiredFiles,
            postActivationValidation: { directory in
                onValidating()
                try await validate(spec, directory)
            })
        let descriptor = ArtifactDescriptor(topLevelDirectory: spec.directoryName, expectedSHA256: digest, version: spec.version)
        do {
            let directory = try await ArtifactInstaller.install(
                archive: archive, descriptor: descriptor, destinationRoot: layout.modelsRoot, options: options)
            ModelStoreLayout.excludeFromBackup(directory)
            return directory
        } catch let ArtifactError.checksumMismatch(expected, actual) {
            throw ModelDownloadError.checksumMismatch(expected: expected, actual: actual)
        }
    }

    /// Resolves an activation that a crash or kill interrupted. A half-activated model is validated and
    /// kept if it loads, otherwise rolled back to the previous install. Call it on launch.
    @discardableResult
    public func recoverInterruptedInstall(_ spec: ModelArtifactSpec, layout: ModelStoreLayout) async throws -> ArtifactRecoveryOutcome {
        let validate = self.validate
        return try await ArtifactInstaller.recoverInterruptedInstall(
            destinationRoot: layout.modelsRoot, topLevelDirectory: spec.directoryName,
            postActivationValidation: { directory in try await validate(spec, directory) })
    }

    /// True when the model is installed, validated (ready marker present), structurally complete, and is
    /// the archive `spec` pins. Resolves an interrupted install first.
    ///
    /// An install that recorded a different archive digest than the spec's is not ready: a build that
    /// pins a newer model must download it instead of keeping the old one. An install with no manifest
    /// (made by an older app, before this owner) is trusted as it stands.
    public func isReady(_ spec: ModelArtifactSpec, layout: ModelStoreLayout) async -> Bool {
        _ = try? await recoverInterruptedInstall(spec, layout: layout)
        let directory = layout.modelDirectory(for: spec)
        guard spec.isStructurallyInstalled(at: directory), CoreMLModelValidator.hasReadyMarker(at: directory) else { return false }
        if let installed = (try? ArtifactInstaller.readManifest(destinationRoot: layout.modelsRoot, topLevelDirectory: spec.directoryName))?.archiveSHA256,
           let pinned = spec.expectedSHA256, installed.caseInsensitiveCompare(pinned) != .orderedSame {
            return false
        }
        return true
    }

    /// Deletes the installed model and any retained archive or aside for `spec`.
    public func remove(_ spec: ModelArtifactSpec, layout: ModelStoreLayout) throws {
        let manager = FileManager.default
        let modelsEntries = (try? manager.contentsOfDirectory(atPath: layout.modelsRoot.path)) ?? []
        // The installer's scratch and aside names all contain the top-level name; remove them with it.
        let related = modelsEntries.filter { $0 == spec.directoryName || ($0.hasPrefix(".") && $0.contains(spec.directoryName)) }
        for name in related { try manager.removeItem(at: layout.modelsRoot.appendingPathComponent(name)) }
        if manager.fileExists(atPath: layout.archiveURL(for: spec).path) {
            try manager.removeItem(at: layout.archiveURL(for: spec))
        }
    }
}
#endif
