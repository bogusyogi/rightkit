// ENG-109: version-guarded in-app file editing.
//
// `FilePreviewFileSystem.writeFile` already refuses a save whose
// `expectedEtag` no longer matches the file on disk (see
// `LocalFilePreviewFileSystem.writeFile`, `FilePreviewError.staleRevision`).
// That check alone protects the file, but `FileViewerFeature.State.etag` —
// the value carried as `expectedEtag` — lives only in that reducer's
// in-memory state. If the view is torn down and rebuilt (backgrounding,
// a low-memory purge, navigating away and back) before the user saves, the
// observed etag is lost and the next save either has to skip the guard or
// re-fetch a fresh file, defeating the point of a guard captured at open
// time.
//
// `SaveRevisionCache` is the durable side of that guard: it remembers, per
// open file path, the etag observed the moment content was last loaded
// into the editor (`recordObserved`), independent of view lifecycle. A save
// consults it (`observedRevision`) instead of trusting only reducer state,
// and clears it once a save lands cleanly (`clearObserved`) so the next
// open starts from a fresh observation. This intentionally does not touch
// `CachedFileRevision` / `listRevisions`, which is unrelated undo-history
// content, not a version-guard token.

import Foundation

/// The version-guard token for one open file: the etag observed when its
/// content was last loaded, and when that observation was made.
public struct ObservedFileRevision: Codable, Equatable, Sendable {
    public let path: String
    public let etag: String
    public let observedAtMs: Int64

    public init(path: String, etag: String, observedAtMs: Int64) {
        self.path = path
        self.etag = etag
        self.observedAtMs = observedAtMs
    }
}

/// Per-file-path store of the etag a viewer last observed for a file,
/// independent of any single `FileViewerFeature` instance's lifetime.
///
/// Serialized through an actor because `FileViewerFeature`'s effects run
/// off the main actor; persistence is best-effort (a cache miss just means
/// the next save re-derives its guard from a fresh `readFile`, which is
/// always safe — it can never widen what a save is allowed to clobber).
public actor SaveRevisionCache {
    public static let shared = SaveRevisionCache()

    private var observed: [String: ObservedFileRevision] = [:]
    private let persistenceURL: URL
    private var didLoadPersistence = false

    public init(storageURL: URL? = nil) {
        persistenceURL = storageURL ?? Self.defaultPersistenceURL()
    }

    /// Record the etag observed for `path` at load/open time. Overwrites
    /// any prior observation for the same path — only the most recent open
    /// is a valid guard baseline.
    public func recordObserved(path: String, etag: String, observedAtMs: Int64? = nil) {
        let observedAtMs = observedAtMs ?? SaveRevisionCache.nowMs()
        loadPersistenceIfNeeded()
        guard !etag.isEmpty else { return }
        observed[path] = ObservedFileRevision(path: path, etag: etag, observedAtMs: observedAtMs)
        persist()
    }

    /// The etag last observed for `path`, if any is on file.
    public func observedRevision(path: String) -> ObservedFileRevision? {
        loadPersistenceIfNeeded()
        return observed[path]
    }

    /// Clear the guard token for `path` — call after a save lands (success
    /// or an explicit user-confirmed overwrite) so the next open captures a
    /// fresh baseline rather than reusing a now-superseded one.
    public func clearObserved(path: String) {
        loadPersistenceIfNeeded()
        guard observed.removeValue(forKey: path) != nil else { return }
        persist()
    }

    private func loadPersistenceIfNeeded() {
        guard !didLoadPersistence else { return }
        didLoadPersistence = true
        guard let data = try? Data(contentsOf: persistenceURL),
              let snapshot = try? JSONDecoder().decode([String: ObservedFileRevision].self, from: data)
        else { return }
        observed = snapshot
    }

    private func persist() {
        guard let data = try? JSONEncoder().encode(observed) else { return }
        let parent = persistenceURL.deletingLastPathComponent()
        do {
            try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
            // the app-owned: atomic write to an arbitrary URL with a protection class (no shared consumer; RightKit issue withdrawn 2026-10-18)
            try data.write(
                to: persistenceURL,
                options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication]
            )
        } catch {
            // Best-effort: losing the persisted guard baseline only means
            // the next save falls back to a fresh read-derived guard, which
            // is always safe (see type doc above).
        }
    }

    private static func defaultPersistenceURL() -> URL {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return support
            .appendingPathComponent("CodeRight", isDirectory: true)
            .appendingPathComponent("FilePreview", isDirectory: true)
            .appendingPathComponent("save-revision-cache.json", isDirectory: false)
    }

    static func nowMs() -> Int64 {
        Int64(Date().timeIntervalSince1970 * 1000)
    }
}
