import Foundation

extension Notification.Name {
    static let heardRightTranscriptLibraryDidMutate = Notification.Name(
        "heardright.transcript-library.did-mutate"
    )
}

/// Cloud merge writes are deliberately silent to prevent a downloaded row from
/// immediately becoming a new outbound upload trigger.
enum TranscriptLibraryMutationOrigin: Equatable {
    case local
    case hydration
    case cloudMerge
}

/// A mutation cannot safely proceed when the on-disk library cannot be read.
/// Keep this distinction visible to callers so an import or recording path can
/// report a blocked write instead of treating it as a successful save.
enum TranscriptLibraryError: LocalizedError, Equatable {
    case unreadable
    case writeFailed

    var errorDescription: String? {
        switch self {
        case .unreadable:
            return "HeardRight could not read transcript history."
        case .writeFailed:
            return "HeardRight could not save transcript history."
        }
    }
}

/// Durable, queryable transcript library.
///
/// HeardRight's App Group UserDefaults store remains the fast cross-extension
/// hand-off. This store is the long-lived library: records are written
/// atomically, retain source/audio metadata, and are bounded only by storage.
/// It intentionally has no account or HeardRight server dependency.
struct HRLibraryTranscript: Codable, Identifiable, Equatable, Sendable {
    let id: UUID
    var text: String
    var rawText: String
    var createdAt: Date
    var updatedAt: Date
    var source: String
    var audioRelativePath: String?
    var tags: [String]
    var folder: String?
    var metadata: [String: String]
    /// The device that originated this transcript, per `HeardRightShared.deviceID`.
    /// `nil` only for rows written before this field existed (or CloudKit
    /// entries synced from a peer running an older build); `TranscriptLibrary`
    /// treats a `nil` origin as locally owned so pre-existing history stays
    /// editable. A non-nil origin that does not match this device's own ID is
    /// "foreign" — content mutation (`update`) is rejected, and (per
    /// HR-HISTORY-006/007) so is deletion: a foreign-origin record remains
    /// fully readable but is otherwise view-only; see `TranscriptLibrary.remove`.
    var originDeviceID: String? = nil

    /// True when this record was written by a different device than the one
    /// currently running — the "view-only" condition `update()` enforces.
    var isForeignOrigin: Bool {
        guard let originDeviceID else { return false }
        return originDeviceID != HeardRightShared.deviceID
    }
}

final class TranscriptLibrary: @unchecked Sendable {
    static let shared = TranscriptLibrary()

    /// Data-protection class for both the library directory and every file
    /// written into it. `.completeUntilFirstUserAuthentication` ("after first
    /// unlock") is the strongest class that still meets this store's
    /// background-access contract: `rehydrateRecentTranscripts`, CloudKit
    /// merge (`TranscriptCloudSync.sync`), and Watch-recording ingest can all
    /// run from a background task/session after the device has been unlocked
    /// once since boot, even while currently locked — unlike
    /// `.completeFileProtection`, which would make every one of those paths
    /// fail whenever the screen is locked. It is stricter than
    /// `.completeFileProtectionUnlessOpen`: that
    /// class only stays readable across a lock if a handle was already open
    /// before the lock, which a batched load-mutate-save cycle like `save(_:)`
    /// cannot guarantee holds true at the moment the device locks. See the
    /// iOS/watchOS Security & architecture note on this exact tradeoff in
    /// `docs/DEEP_AUDIT_2026-08-31.md`.
    private static let protectionClass = FileProtectionType.completeUntilFirstUserAuthentication

    private let lock = NSLock()
    private let fileURL: URL
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder
    /// Decoded library cache keyed on the file's (mtime, size). This class is
    /// the file's only writer and every load/save runs under `lock`, so the
    /// cache can only go stale if something outside this process rewrites the
    /// file — which the stamp check catches. Previously every mutation paid a
    /// full JSON decode of the whole library.
    private var cachedRecords: [HRLibraryTranscript]?
    private var cachedFileStamp: (Date, UInt64)?
    /// A failed read is a write barrier. The last known index remains available
    /// for display, but no mutation may replace bytes that were not decoded.
    private var loadBlocked = false
    /// Deduplicated stable-ID index maintained in lockstep with the stamped
    /// cache above so `update`/`remove`/`upsert` resolve their record in O(1)
    /// instead of re-scanning the decoded array once per mutation. Invariant:
    /// when `load()` succeeds, `recordIndex` always equals `Self.indexByID` of
    /// the array that call produced — including `[:]` for a missing file — and
    /// after a successful `save()` it equals
    /// `Self.indexByID` of the array just persisted. A mutator may edit it in
    /// place between `load()` and `save()`, but only under `lock`, and MUST
    /// restore the previous entry when `save` fails so the index never
    /// describes library state that was not durably persisted (the same
    /// discard-on-failure guarantee the local `records` copy used to give).
    private var recordIndex: [UUID: HRLibraryTranscript] = [:]

    init(fileURL: URL? = nil) {
        let root = fileURL?.deletingLastPathComponent()
            ?? Self.applicationSupportDirectory()
        self.fileURL = fileURL ?? root.appendingPathComponent("TranscriptLibrary.json")
        self.encoder = JSONEncoder()
        self.decoder = JSONDecoder()
        encoder.dateEncodingStrategy = .iso8601
        decoder.dateDecodingStrategy = .iso8601
        try? FileManager.default.createDirectory(
            at: root,
            withIntermediateDirectories: true,
            attributes: [.protectionKey: Self.protectionClass]
        )
    }

    func all() -> [HRLibraryTranscript] {
        lock.lock(); defer { lock.unlock() }
        _ = load()
        return recordIndex.values.sorted {
            if $0.createdAt != $1.createdAt { return $0.createdAt > $1.createdAt }
            return $0.id.uuidString > $1.id.uuidString
        }
    }

    func search(_ query: String, tags: Set<String> = [], folder: String? = nil) -> [HRLibraryTranscript] {
        let normalized = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return all().filter { item in
            let matchesText = normalized.isEmpty ||
                item.text.range(of: normalized, options: [.caseInsensitive, .diacriticInsensitive]) != nil
            let matchesTags = tags.isEmpty || tags.isSubset(of: Set(item.tags))
            let matchesFolder = folder == nil || item.folder == folder
            return matchesText && matchesTags && matchesFolder
        }
    }

    /// Throws when existing history cannot be decoded/read or when the new
    /// value cannot be durably saved. A failed read never falls back to an
    /// empty write base.
    @discardableResult
    func upsert(
        id: UUID = UUID(),
        text: String,
        rawText: String? = nil,
        createdAt: Date = Date(),
        source: String = "iPhone",
        audioURL: URL? = nil,
        tags: [String] = [],
        folder: String? = nil,
        metadata: [String: String] = [:],
        mutationOrigin: TranscriptLibraryMutationOrigin = .local
    ) throws -> HRLibraryTranscript {
        lock.lock(); defer { lock.unlock() }
        guard load() else { throw TranscriptLibraryError.unreadable }
        let now = Date()
        let relativeAudioPath = audioURL.map(Self.relativeAudioPath(for:))
        let existing = recordIndex[id]
        let value = HRLibraryTranscript(
            id: id,
            text: text,
            rawText: rawText ?? text,
            createdAt: existing?.createdAt ?? createdAt,
            updatedAt: now,
            source: source,
            audioRelativePath: relativeAudioPath,
            tags: Array(Set(tags.map(Self.normalizedTag))).sorted(),
            folder: folder?.trimmingCharacters(in: .whitespacesAndNewlines),
            metadata: metadata,
            // A brand-new row is stamped with this device's identity. A
            // pre-existing row keeps whatever origin it already had — this
            // path is also how a locally-recorded transcript gets its trailing
            // polish/correction pass merged in, and that must not silently
            // reassign ownership away from wherever the row actually came from.
            originDeviceID: existing?.originDeviceID ?? HeardRightShared.deviceID
        )
        recordIndex[id] = value
        if !save(Array(recordIndex.values)) {
            // Roll the index back to the persisted state: a failed write must
            // not leave the new row visible to later loads.
            recordIndex[id] = existing
            DogfoodTraceStore.record("transcript_library_upsert_save_failed", fields: ["id": id.uuidString])
            throw TranscriptLibraryError.writeFailed
        } else {
            postMutationIfNeeded([id], origin: mutationOrigin)
        }
        return value
    }

    /// Applies `change` to the record and returns whether it was applied.
    /// Returns `false` without mutating anything when the record does not
    /// exist, or when it originated on a different device (`isForeignOrigin`)
    /// — a synced-in transcript from another Mac/iPhone/Watch is read-only for
    /// content edits on this device. Deletion is a separate, always-allowed
    /// contract; see `remove(_:)`.
    @discardableResult
    func update(
        _ id: UUID,
        mutationOrigin: TranscriptLibraryMutationOrigin = .local,
        _ change: (inout HRLibraryTranscript) -> Void
    ) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard load() else { return false }
        guard let original = recordIndex[id] else { return false }
        guard !original.isForeignOrigin else { return false }
        var record = original
        change(&record)
        record.updatedAt = Date()
        record.tags = Array(Set(record.tags.map(Self.normalizedTag))).sorted()
        recordIndex[id] = record
        guard save(Array(recordIndex.values)) else {
            // A failed write must not report the edit as applied — restore
            // the indexed record so the in-memory mutation is discarded if
            // `save` did not durably land.
            recordIndex[id] = original
            DogfoodTraceStore.record("transcript_library_update_save_failed", fields: ["id": id.uuidString])
            return false
        }
        postMutationIfNeeded([id], origin: mutationOrigin)
        return true
    }

    /// Removes the record and returns whether it is now durably gone.
    /// Returns `true` when the id was already absent (deletion is idempotent)
    /// and when the removal was written successfully; returns `false` only
    /// when the record existed and the write failed — a failed delete must
    /// never be reported as a success, since callers use this to decide
    /// whether it is safe to advance tombstone/change-token state.
    ///
    /// HR-HISTORY-006/007: a record that originated on
    /// a DIFFERENT device (`isForeignOrigin`) is rejected here too, not
    /// only left to the History UI's swipe/select affordances, which hide the
    /// delete control for a foreign row but are not themselves enforcement.
    /// A rejected removal returns `false`, mirroring
    /// `update(_:)`'s existing foreign-origin guard, so a caller cannot
    /// mistake "refused" for "already gone". Note this device's own
    /// `originDeviceID` covers both this iPhone's own recordings AND any
    /// Watch recording it processed locally (HR-CONTROL-006 — iPhone owns
    /// Watch transcription/history persistence, so a Watch-origin row is
    /// written here with THIS iPhone's device id, not a foreign one); only a
    /// row synced in from a genuinely different device (e.g. Mac, via
    /// CloudKit) is foreign.
    @discardableResult
    func remove(_ id: UUID, mutationOrigin: TranscriptLibraryMutationOrigin = .local) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard load() else { return false }
        guard let original = recordIndex[id] else { return true }
        guard !original.isForeignOrigin else {
            DogfoodTraceStore.record("transcript_library_delete_rejected_foreign_origin", fields: ["id": id.uuidString])
            return false
        }
        recordIndex[id] = nil
        guard save(Array(recordIndex.values)) else {
            recordIndex[id] = original
            DogfoodTraceStore.record("transcript_library_delete_failed", fields: ["id": id.uuidString])
            return false
        }
        postMutationIfNeeded([id], origin: mutationOrigin)
        return true
    }

    /// Applies a deletion already accepted from CloudKit. Remote sync is a
    /// trusted replication path, so it may remove a foreign-origin row that
    /// ordinary local UI deletion correctly refuses.
    @discardableResult
    func removeForCloudSync(_ id: UUID) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard load() else { return false }
        guard let original = recordIndex[id] else { return true }
        recordIndex[id] = nil
        guard save(Array(recordIndex.values)) else {
            recordIndex[id] = original
            return false
        }
        return true
    }

    /// Replaces the whole library and returns whether the write succeeded.
    /// Callers merging a CloudKit download (`TranscriptCloudSync.sync`) must
    /// check this: reporting a merge as downloaded when the local write
    /// silently failed would advance the sync cursor past data that was
    /// never actually saved.
    @discardableResult
    func replaceAll(
        _ records: [HRLibraryTranscript],
        mutationOrigin: TranscriptLibraryMutationOrigin = .local,
        cloudTombstones: () -> Set<UUID> = { [] }
    ) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard load() else { return false }
        var values = deduplicatedByID(records)
        if mutationOrigin == .cloudMerge {
            // CloudKit fetches happen across awaits. Preserve any local row
            // that changed after the sync snapshot instead of letting a stale
            // replaceAll overwrite an edit or newly inserted transcript.
            for current in recordIndex.values {
                guard let incoming = values.first(where: { $0.id == current.id }) else {
                    values.append(current)
                    continue
                }
                if current.updatedAt > incoming.updatedAt {
                    values.removeAll { $0.id == current.id }
                    values.append(current)
                }
            }
            let blocked = cloudTombstones()
            values = deduplicatedByID(values).filter { !blocked.contains($0.id) }
        }
        guard save(values) else { return false }
        postMutationIfNeeded(Set(values.map(\.id)), origin: mutationOrigin)
        return true
    }

    /// One request in a batch write — see `upsertBatch`. Mirrors `upsert`'s
    /// parameters minus the default values, which don't carry across a batch.
    struct BatchUpsertRequest {
        let id: UUID
        let text: String
        let rawText: String?
        let createdAt: Date
        let source: String
        let tags: [String]
        let folder: String?
        let metadata: [String: String]

        init(
            id: UUID,
            text: String,
            rawText: String? = nil,
            createdAt: Date = Date(),
            source: String = "iPhone",
            tags: [String] = [],
            folder: String? = nil,
            metadata: [String: String] = [:]
        ) {
            self.id = id
            self.text = text
            self.rawText = rawText
            self.createdAt = createdAt
            self.source = source
            self.tags = tags
            self.folder = folder
            self.metadata = metadata
        }
    }

    /// Applies every request under one load-mutate-save cycle instead of one
    /// per record. `upsert(_:)` in a loop of N records does N full-library
    /// decodes and N full-library re-encodes; a rehydration pass merging
    /// dozens of records at once (`DictationMicrophone.rehydrateRecentTranscripts`)
    /// is exactly that loop, and it is the audited perf gap this exists to
    /// close (`docs/DEEP_AUDIT_2026-08-31.md` iOS/watchOS Performance & UX,
    /// item 2). Returns the written records in request order; returns `[]`
    /// (and logs durably) if the single batched save failed, so a caller
    /// cannot mistake a failed batch for "nothing needed writing".
    @discardableResult
    func upsertBatch(
        _ requests: [BatchUpsertRequest],
        mutationOrigin: TranscriptLibraryMutationOrigin = .local
    ) -> [HRLibraryTranscript] {
        guard !requests.isEmpty else { return [] }
        lock.lock(); defer { lock.unlock() }
        guard load() else { return [] }
        // Copy-on-write: `byID` shares the index until the first request
        // writes through it, so a failed batch save leaves `recordIndex`
        // untouched — the same discard-on-failure the array copy gave.
        var byID = recordIndex
        let now = Date()
        var written: [HRLibraryTranscript] = []
        written.reserveCapacity(requests.count)
        for request in requests {
            let existing = byID[request.id]
            let value = HRLibraryTranscript(
                id: request.id,
                text: request.text,
                rawText: request.rawText ?? request.text,
                createdAt: existing?.createdAt ?? request.createdAt,
                updatedAt: now,
                source: request.source,
                audioRelativePath: existing?.audioRelativePath,
                tags: Array(Set(request.tags.map(Self.normalizedTag))).sorted(),
                folder: request.folder?.trimmingCharacters(in: .whitespacesAndNewlines),
                metadata: request.metadata,
                originDeviceID: existing?.originDeviceID ?? HeardRightShared.deviceID
            )
            byID[request.id] = value
            written.append(value)
        }
        guard save(Array(byID.values)) else {
            DogfoodTraceStore.record("transcript_library_batch_save_failed", fields: ["count": requests.count])
            return []
        }
        postMutationIfNeeded(Set(written.map(\.id)), origin: mutationOrigin)
        return written
    }

    /// Repairs legacy rows created when one Watch transfer was processed by
    /// two concurrent tasks. Distinct recordings retain distinct timestamps.
    func deduplicateLegacyWatchTransfers() {
        lock.lock(); defer { lock.unlock() }
        guard load() else { return }
        let records = recordIndex.values.sorted { $0.updatedAt > $1.updatedAt }
        var seen = Set<String>()
        var retained: [HRLibraryTranscript] = []
        retained.reserveCapacity(records.count)
        for record in records {
            guard record.source.caseInsensitiveCompare("Watch") == .orderedSame else {
                retained.append(record)
                continue
            }
            // Legacy duplicate lacks session metadata; exact source timestamp
            // + raw payload remains stable across both writes of one transfer.
            let transferKey = "\(record.createdAt.timeIntervalSinceReferenceDate.bitPattern):\(record.rawText)"
            guard seen.insert(transferKey).inserted else { continue }
            retained.append(record)
        }
        guard retained.count != records.count else { return }
        guard save(retained) else {
            DogfoodTraceStore.record("transcript_library_dedupe_save_failed", fields: ["count": retained.count])
            return
        }
    }

    private func postMutationIfNeeded(_ ids: Set<UUID>, origin: TranscriptLibraryMutationOrigin) {
        guard !ids.isEmpty, origin != .cloudMerge else { return }
        NotificationCenter.default.post(
            name: .heardRightTranscriptLibraryDidMutate,
            object: nil,
            userInfo: ["ids": ids.map(\.uuidString)]
        )
    }

    func audioURL(for item: HRLibraryTranscript) -> URL? {
        guard let path = item.audioRelativePath else { return nil }
        return Self.applicationSupportDirectory().appendingPathComponent(path)
    }

    func exportJSON(_ records: [HRLibraryTranscript]? = nil) throws -> Data {
        try encoder.encode(records ?? all())
    }

    func exportMarkdown(_ records: [HRLibraryTranscript]? = nil) -> String {
        let values = (records ?? all()).sorted { $0.createdAt < $1.createdAt }
        guard !values.isEmpty else { return "# HeardRight transcripts\n\n_No transcripts._\n" }
        var lines = ["# HeardRight transcripts", ""]
        for item in values {
            lines.append("## \(item.createdAt.formatted(.iso8601.year().month().day().time(includingFractionalSeconds: false)))")
            if !item.tags.isEmpty { lines.append("Tags: \(item.tags.joined(separator: ", "))") }
            lines.append("")
            lines.append(item.text)
            lines.append("")
        }
        return lines.joined(separator: "\n")
    }

    /// Durable readback: a missing file (first run) is a normal empty state
    /// and stays silent, but a file that exists and fails to read or decode
    /// is a real fault — treating it the same as "empty" would silently
    /// present a full history loss as if nothing had ever been recorded, so
    /// it is logged to the durable trace instead of swallowed.
    private func fileStamp() -> (Date, UInt64)? {
        guard let attrs = try? FileManager.default.attributesOfItem(atPath: fileURL.path),
              let mtime = attrs[.modificationDate] as? Date,
              let size = attrs[.size] as? UInt64 else { return nil }
        return (mtime, size)
    }

    @discardableResult
    private func load() -> Bool {
        guard FileManager.default.fileExists(atPath: fileURL.path) else {
            cachedRecords = []
            cachedFileStamp = nil
            recordIndex = [:]
            loadBlocked = false
            return true
        }
        let stamp = fileStamp()
        if cachedRecords != nil, let cachedFileStamp, let stamp, cachedFileStamp == stamp {
            loadBlocked = false
            return true
        }
        guard let data = try? Data(contentsOf: fileURL) else {
            DogfoodTraceStore.record("transcript_library_read_failed", fields: [:])
            // Preserve the last known index and make this a write barrier.
            // Replacing unreadable bytes with a new empty base would destroy
            // recoverable history on the next mutation.
            cachedRecords = nil
            cachedFileStamp = nil
            loadBlocked = true
            return false
        }
        do {
            let records = try decoder.decode([HRLibraryTranscript].self, from: data)
            cachedRecords = records
            cachedFileStamp = stamp
            recordIndex = Self.indexByID(records)
            loadBlocked = false
            return true
        } catch {
            DogfoodTraceStore.record("transcript_library_decode_failed", fields: [
                "error": String(error.localizedDescription.prefix(200)),
            ])
            cachedRecords = nil
            cachedFileStamp = nil
            loadBlocked = true
            return false
        }
    }

    /// A legacy import or a sync merge can contain the same logical transcript
    /// more than once. Keep newest version so every consumer sees stable IDs.
    private func deduplicatedByID(_ records: [HRLibraryTranscript]) -> [HRLibraryTranscript] {
        Array(Self.indexByID(records).values)
    }

    /// The newest-wins dedup rule in dictionary form — the shape `recordIndex`
    /// is stored in so mutations resolve IDs in O(1). A record already
    /// deduplicated by this rule maps back to itself unchanged.
    private static func indexByID(_ records: [HRLibraryTranscript]) -> [UUID: HRLibraryTranscript] {
        var byID: [UUID: HRLibraryTranscript] = [:]
        byID.reserveCapacity(records.count)
        for record in records {
            guard let current = byID[record.id] else {
                byID[record.id] = record
                continue
            }
            if record.updatedAt > current.updatedAt ||
               (record.updatedAt == current.updatedAt && record.createdAt > current.createdAt) ||
               (record.updatedAt == current.updatedAt && record.createdAt == current.createdAt &&
                record.rawText > current.rawText) {
                byID[record.id] = record
            }
        }
        return byID
    }

    /// Returns whether the write actually landed durably. Every mutating
    /// method above must check this rather than assuming success — see
    /// `remove(_:)`/`replaceAll(_:)`/`upsertBatch(_:)`.
    @discardableResult
    private func save(_ records: [HRLibraryTranscript]) -> Bool {
        guard !loadBlocked else {
            DogfoodTraceStore.record("transcript_library_save_blocked_unreadable", fields: ["count": records.count])
            return false
        }
        guard let data = try? encoder.encode(records) else {
            DogfoodTraceStore.record("transcript_library_encode_failed", fields: ["count": records.count])
            return false
        }
        let temporary = fileURL.appendingPathExtension("tmp")
        do {
            // `Data.WritingOptions` has no direct `FileProtectionType` value,
            // so this must be kept in sync by hand with `Self.protectionClass`
            // above (`.completeUntilFirstUserAuthentication`) — the directory
            // attribute set at init and this per-file write option are meant
            // to agree exactly (see the `protectionClass` doc comment for why
            // a mismatch would matter).
            try data.write(to: temporary, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            if FileManager.default.fileExists(atPath: fileURL.path) {
                _ = try FileManager.default.replaceItemAt(fileURL, withItemAt: temporary)
            } else {
                try FileManager.default.moveItem(at: temporary, to: fileURL)
            }
            // Refresh the cache to the just-written bytes; a nil stamp simply
            // forces the next load to re-read. The index is rebuilt from the
            // persisted array so it never describes state that failed to
            // save — callers that edited it in place roll back on `false`.
            cachedRecords = records
            cachedFileStamp = fileStamp()
            recordIndex = Self.indexByID(records)
            return true
        } catch {
            try? FileManager.default.removeItem(at: temporary)
            DogfoodTraceStore.record("transcript_library_save_failed", fields: [
                "error": String(error.localizedDescription.prefix(200)),
            ])
            return false
        }
    }

    private static func normalizedTag(_ value: String) -> String {
        value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }

    private static func applicationSupportDirectory() -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("HeardRight", isDirectory: true)
    }

    private static func relativeAudioPath(for url: URL) -> String {
        let audioRoot = applicationSupportDirectory().appendingPathComponent("Audio", isDirectory: true)
        try? FileManager.default.createDirectory(
            at: audioRoot, withIntermediateDirectories: true,
            attributes: [.protectionKey: protectionClass])
        let destination = audioRoot.appendingPathComponent(url.lastPathComponent)
        if url.standardizedFileURL != destination.standardizedFileURL {
            try? FileManager.default.copyItem(at: url, to: destination)
            try? FileManager.default.setAttributes(
                [.protectionKey: protectionClass], ofItemAtPath: destination.path)
        }
        return "Audio/\(destination.lastPathComponent)"
    }
}
