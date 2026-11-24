#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation
import os

/// Reclaims orphaned Apple Neural Engine compile bundles from the app's own Core ML cache.
///
/// **The defect this exists for** (measured on device, iPhone 16 Pro Max, iOS 26.5): the ANE compile of a
/// large encoder (about 531 MB) is started on model load and, if the app is killed or suspended before it
/// finalizes, Core ML leaves the in-progress bundle behind as `<hash>.tmp.<pid>_<inode>.bundle` containing a
/// full `H17.bundle/main/multiprocedure/ane_mil_model/weights1.bin`. Nothing ever cleans these up. A dogfood
/// device had 536 cache keys and at least 35 GB of these orphans, with almost no finalized entries, so the
/// model kept running the CPU path while consuming tens of GB.
///
/// A finalized entry is about 2 MB and must never be touched: that is the cache doing its job. Only `.tmp.*`
/// bundles are removed, and only when they are old enough that no live compile could still own them.
public enum ANECacheMaintenance {
    /// A live compile writes into its `.tmp` bundle continuously. Anything untouched for this long belongs to
    /// a process that is gone.
    public static let orphanAge: TimeInterval = 10 * 60

    private static let state = OSAllocatedUnfairLock(initialState: false)

    /// Fire-and-forget sweep, at most once per launch, off the main thread. Safe to call before any model load.
    /// - Parameters:
    ///   - logSubsystem: `os.Logger` subsystem (the host's bundle id).
    ///   - event: host event sink, called only when something was reclaimed, with `bundles` and `mb`.
    public static func sweepOrphansOnce(
        logSubsystem: String,
        event: @escaping @Sendable (String, [String: any Sendable]) -> Void = { _, _ in }
    ) {
        let first = state.withLock { didRun -> Bool in
            if didRun { return false }
            didRun = true
            return true
        }
        guard first else { return }
        Task.detached(priority: .utility) { sweepOrphans(logSubsystem: logSubsystem, event: event) }
    }

    @discardableResult
    public static func sweepOrphans(
        now: Date = Date(),
        logSubsystem: String,
        event: @Sendable (String, [String: any Sendable]) -> Void = { _, _ in }
    ) -> (removed: Int, bytes: Int64) {
        let fileManager = FileManager.default
        guard let caches = fileManager.urls(for: .cachesDirectory, in: .userDomainMask).first else {
            return (0, 0)
        }
        let root = caches.appendingPathComponent("com.apple.e5rt.e5bundlecache", isDirectory: true)
        guard fileManager.fileExists(atPath: root.path) else { return (0, 0) }

        var removed = 0
        var reclaimed: Int64 = 0
        let currentPID = String(ProcessInfo.processInfo.processIdentifier)

        // Layout: <root>/<osbuild>/<key>/<hash>.tmp.<pid>_<inode>.bundle
        guard let walker = fileManager.enumerator(
            at: root,
            includingPropertiesForKeys: [.isDirectoryKey, .contentModificationDateKey],
            options: [.skipsHiddenFiles]
        ) else { return (0, 0) }

        for case let url as URL in walker {
            guard url.lastPathComponent.hasSuffix(".bundle"),
                  url.lastPathComponent.contains(".tmp.") else { continue }
            // Never descend into a bundle we are about to evaluate or delete.
            walker.skipDescendants()

            // Skip anything this process could still be compiling.
            if url.lastPathComponent.contains(".tmp.\(currentPID)_") { continue }

            // An unreadable timestamp cannot establish that a compile is orphaned.
            guard let values = try? url.resourceValues(forKeys: [.contentModificationDateKey]),
                  let modified = values.contentModificationDate,
                  now.timeIntervalSince(modified) >= orphanAge else { continue }

            let size = directorySize(url)
            do {
                try fileManager.removeItem(at: url)
                removed += 1
                reclaimed += size
            } catch {
                // Busy or protected: leave it, next launch tries again.
                continue
            }
        }

        if removed > 0 {
            let mb = reclaimed / 1_000_000
            Logger(subsystem: logSubsystem, category: "ane-cache")
                .info("reclaimed \(removed, privacy: .public) orphaned ANE bundles, \(mb, privacy: .public) MB")
            event("ane_cache_orphans_reclaimed", [
                "bundles": removed,
                "mb": mb,
            ])
        }
        return (removed, reclaimed)
    }

    private static func directorySize(_ url: URL) -> Int64 {
        var total: Int64 = 0
        guard let walker = FileManager.default.enumerator(
            at: url,
            includingPropertiesForKeys: [.fileSizeKey],
            options: [.skipsHiddenFiles]
        ) else { return 0 }
        for case let file as URL in walker {
            let size = (try? file.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
            total += Int64(size)
        }
        return total
    }
}
#endif
