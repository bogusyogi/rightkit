#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation

/// A one-time reset for a release that retires installed models: when a retired install is still on disk,
/// delete it and the models named with it, drop any staged archive, and tell the host so it can send the
/// user back through setup (the host decides what that means; this owner does not know about onboarding).
///
/// The reset runs at most once. It needs readable protected data: a launch before the first unlock after a
/// reboot reads `UserDefaults` as empty, so the completion flag alone would run the reset again and delete a
/// model the user already installed.
public enum ModelRetirementReset {
    /// - Parameters:
    ///   - trigger: the retired install whose presence on disk starts the reset. Without it the reset
    ///     records itself as done and does nothing.
    ///   - remove: every model (retired and current) to delete once the reset starts, with its archive.
    ///   - discardStagedModelIDs: ids whose parked background download is dropped.
    ///   - completedKey: the `UserDefaults` flag that records a finished reset.
    ///   - protectedDataAvailable: false before the first unlock; the reset then waits for a later launch.
    ///   - onRemoved: called once when at least one model was deleted (the host resets its own setup state).
    ///   - event: host event sink; fields are comma-joined directory names.
    public static func runOnce(
        trigger: ModelArtifactSpec,
        remove: [ModelArtifactSpec],
        discardStagedModelIDs: [String],
        layout: ModelStoreLayout,
        staging: ModelDownloadStaging,
        completedKey: String,
        defaults: UserDefaults = .standard,
        fileManager: FileManager = .default,
        protectedDataAvailable: Bool,
        installer: ModelInstaller = ModelInstaller(),
        onRemoved: () -> Void = {},
        eventName: String = "model_retirement_reset",
        event: (String, [String: any Sendable]) -> Void = { _, _ in }
    ) {
        guard protectedDataAvailable, !defaults.bool(forKey: completedKey) else { return }
        guard fileManager.fileExists(atPath: layout.modelDirectory(for: trigger).path) else {
            defaults.set(true, forKey: completedKey)
            return
        }
        var removed: [String] = []
        var failed: [String] = []
        // Keep the trigger until every other deletion succeeds, so a partial failure remains retryable
        // without introducing a new persisted migration key.
        let ordered = remove.filter { $0.directoryName != trigger.directoryName }
            + remove.filter { $0.directoryName == trigger.directoryName }
        for spec in ordered {
            if spec.directoryName == trigger.directoryName, !failed.isEmpty { continue }
            guard fileManager.fileExists(atPath: layout.modelDirectory(for: spec).path)
                || fileManager.fileExists(atPath: layout.archiveURL(for: spec).path) else { continue }
            do {
                try installer.remove(spec, layout: layout)
                removed.append(spec.directoryName)
            } catch {
                failed.append(spec.directoryName)
            }
        }
        for id in discardStagedModelIDs { staging.discard(model: id) }
        if !removed.isEmpty { onRemoved() }
        // A failed delete retries on the next launch.
        if failed.isEmpty { defaults.set(true, forKey: completedKey) }
        event(eventName, [
            "removed": removed.joined(separator: ","),
            "failed": failed.joined(separator: ","),
        ])
    }
}
#endif
