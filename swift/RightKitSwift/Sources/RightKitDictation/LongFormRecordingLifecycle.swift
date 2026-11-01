import Foundation

/// Adds Muesli-style checkpoint durability to every HeardRight recording.
/// Checkpoints are independent WAV files; the manifest is the recovery
/// boundary and is updated atomically after each file is complete.
///
/// Concurrency: cursors and chunk bookkeeping stay on the MainActor; the WAV
/// encode + manifest rewrite run on a serial `Task.detached` chain
/// (`pendingWrite`). Every new link awaits the previous link before touching
/// disk, so `begin` → `appendChunk`* → the terminal repair/seal always
/// land on disk in order and `RecordingCheckpointStore`'s atomic-replace
/// manifest write is unchanged. The main actor now pays only the bounded
/// sample-array copy per 30 s checkpoint, never the encode or the manifest
/// round-trip.
///
/// Disposition: nothing consumes a sealed manifest today — `recoverable()`
/// is the reader a recovery surface would use, and it is not wired. A
/// successful `finish` seals its repaired manifest but retains the directory
/// for a future recovery/cleanup owner. A failed repair remains incomplete
/// and recoverable; `cancel` can discard an active or still-finishing chain.
/// A new `start` never sweeps old sessions: an interrupted recording is user
/// data, not disposable cache.
@MainActor
final class LongFormRecordingLifecycle {
    static let checkpointInterval: TimeInterval = 30

    private let store: RecordingCheckpointStore
    private var sessionID: String?
    private var nextSampleIndex = 0
    private var nextChunkIndex = 0
    /// Every chunk range handed to the write chain, in order. `finish` diffs
    /// this against the landed manifest to repair failed writes while the
    /// sample buffer is still resident.
    private var issuedChunks: [(index: Int, start: Int, count: Int)] = []
    private var checkpointTask: Task<Void, Never>?
    /// Tail of the off-main write chain; see the type comment. Each detached
    /// link awaits its predecessor, which is what keeps checkpoint writes
    /// serialized in order now that they no longer run on the main actor.
    private var pendingWrite: Task<Void, Never>?
    /// A finished session remains owned until its terminal write chain lands.
    /// This lets an explicit cancel during that await discard the old chain
    /// without touching a newer active session.
    private var finishingSessionID: String?
    private var finishingTail: Task<Void, Never>?
    /// Changes whenever a new session or explicit discard takes ownership of
    /// the write chain. `finish` uses it to avoid clearing a newer chain after
    /// an actor reentrancy point.
    private var lifecycleGeneration = 0

    init(store: RecordingCheckpointStore = .shared) {
        self.store = store
    }

    func start(sessionID: String, capture: WarmAudioCapture) {
        cancelTask()
        self.sessionID = sessionID
        nextSampleIndex = 0
        nextChunkIndex = 0
        issuedChunks = []
        lifecycleGeneration &+= 1
        // Links from a previous session may still be in flight; they carry
        // their own sessionID/store. The new session's chain starts fresh
        // here, while the old directory remains available for recovery.
        pendingWrite = nil
        let store = UncheckedSendableBox(value: self.store)
        // The manifest must exist before any appendChunk tries to load it.
        // Interrupted sessions are deliberately left untouched; only an
        // explicit cancel is allowed to discard the current session chain.
        enqueueWrite {
            try? store.value.begin(
                sessionID: sessionID,
                startedAt: Date(),
                sampleRate: Int(WarmAudioCapture.targetSampleRate)
            )
        }
        let startedSessionID = sessionID
        checkpointTask = Task { [weak self, weak capture] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(Self.checkpointInterval * 1_000_000_000))
                guard !Task.isCancelled, let self, let capture else { return }
                // The MainActor hop now only snapshots the new samples (an
                // array copy bounded to one interval); the encode + manifest
                // write happen in the detached chain. The sessionID pin keeps
                // a hop queued across `finish`/`start` from writing a stale
                // capture's samples into the new session's stream.
                await MainActor.run { self.checkpoint(capture: capture, sessionID: startedSessionID) }
            }
        }
    }

    /// Writes only samples not covered by the previous checkpoint.
    /// `sessionID` is the session the snapshot was scheduled under: a hop
    /// already queued when `cancel`/`finish`/`start` ran must not write into
    /// a session that has moved on.
    func checkpoint(capture: WarmAudioCapture, sessionID expected: String) {
        guard sessionID == expected else { return }
        let sampleCount = capture.sampleCount
        guard sampleCount > nextSampleIndex else { return }
        let samples = capture.samples(from: nextSampleIndex)
        write(samples: samples, startSample: nextSampleIndex)
    }

    /// Seals any tail after the last periodic checkpoint, repairs chunks a
    /// failed write left out of the manifest (the sample buffer is still
    /// resident), and retains the sealed directory. If repair cannot make
    /// every expected chunk durable, the manifest stays incomplete so a
    /// recovery consumer can find it. Async so the caller waits out the write
    /// chain: the terminal link is the last checkpoint mutation, and `finish`
    /// must not return until it has landed.
    func finish(samples: [Float]) async {
        guard let sessionID else { return }
        cancelTask()
        if samples.count > nextSampleIndex {
            write(samples: Array(samples[nextSampleIndex...]), startSample: nextSampleIndex)
        }
        // Close the session BEFORE the await: a `MainActor.run` hop already
        // queued by the cancelled `checkpointTask` still executes when the
        // actor frees up, and `checkpoint` snapshots the live capture — past
        // a keyword-cut tail that would write discarded audio into an orphan
        // chunk the manifest never lists. The `expected` pin already gates
        // it; this makes the intended ordering explicit rather than relying
        // on the pin alone.
        self.sessionID = nil
        finishingSessionID = sessionID
        let store = UncheckedSendableBox(value: self.store)
        let issued = issuedChunks
        issuedChunks = []
        let finishGeneration = lifecycleGeneration
        enqueueWrite {
            // The chain is serial, so every earlier link has landed or failed
            // by the time this runs: the manifest is ground truth for which
            // chunks are missing. Rewrite them from the full buffer — bounded
            // to `samples.count` so a keyword-cut tail is never resurrected.
            if let manifest = try? store.value.load(sessionID: sessionID) {
                let landed = Set(manifest.chunks.map(\.index))
                var repairFailed = false
                for chunk in issued where !landed.contains(chunk.index) {
                    let end = min(chunk.start + chunk.count, samples.count)
                    guard chunk.start < end else { continue }
                    let url = store.value.chunkURL(sessionID: sessionID, index: chunk.index)
                    do {
                        try PCMWavWriter.write(
                            samples: Array(samples[chunk.start..<end]),
                            sampleRate: Int(WarmAudioCapture.targetSampleRate),
                            to: url
                        )
                        try store.value.appendChunk(
                            sessionID: sessionID,
                            index: chunk.index,
                            startSample: chunk.start,
                            sampleCount: end - chunk.start,
                            fileName: url.lastPathComponent
                        )
                        // Keep repair observable even though the repaired
                        // manifest is retained and sealed below.
                        DogfoodTraceStore.record("recording_checkpoint_repaired", fields: [
                            "session_id": sessionID,
                            "chunk_index": chunk.index
                        ])
                    } catch {
                        repairFailed = true
                        DogfoodTraceStore.record("recording_checkpoint_failed", fields: [
                            "session_id": sessionID,
                            "chunk_index": chunk.index,
                            "attempt": "finish_repair",
                            "error": error.localizedDescription
                        ])
                    }
                }
                if !repairFailed,
                   let sealed = try? store.value.load(sessionID: sessionID),
                   issued.allSatisfy({ chunk in
                       chunk.start >= samples.count || sealed.chunks.contains(where: {
                           $0.index == chunk.index &&
                               $0.startSample == chunk.start &&
                               $0.sampleCount == min(chunk.count, samples.count - chunk.start) &&
                               FileManager.default.fileExists(
                                   atPath: store.value.chunkURL(
                                       sessionID: sessionID, index: chunk.index
                                   ).path
                               )
                       })
                   }) {
                    do {
                        try store.value.complete(sessionID: sessionID)
                        DogfoodTraceStore.record("recording_checkpoint_sealed", fields: [
                            "session_id": sessionID
                        ])
                    } catch {
                        DogfoodTraceStore.record("recording_checkpoint_failed", fields: [
                            "session_id": sessionID,
                            "attempt": "finish_seal",
                            "error": error.localizedDescription
                        ])
                    }
                }
            }
        }
        let finishTail = pendingWrite
        finishingTail = finishTail
        await finishTail?.value
        // The actor may accept a new start while this await is suspended. Do
        // not clear that newer chain's tail when the old finish resumes.
        if lifecycleGeneration == finishGeneration {
            pendingWrite = nil
        }
        if finishingSessionID == sessionID {
            finishingSessionID = nil
            finishingTail = nil
        }
    }

    func cancel() {
        cancelTask()
        if let sessionID {
            lifecycleGeneration &+= 1
            // The removal rides the write chain instead of running inline:
            // the first link's `begin` may not have executed yet, and an
            // inline remove would no-op on the not-yet-created dir and leak
            // it once `begin` landed. On the chain, removal always runs
            // after `begin` (and after every earlier link).
            let store = UncheckedSendableBox(value: self.store)
            enqueueWrite {
                store.value.remove(sessionID: sessionID)
                // Chain-completion signal: like `recording_checkpoint_repaired`,
                // this makes a silent discard observable (and gives callers a
                // way to know the chain settled — the tail handle is dropped).
                DogfoodTraceStore.record("recording_checkpoint_cancelled", fields: [
                    "session_id": sessionID
                ])
            }
        } else if let sessionID = finishingSessionID, let finishTail = finishingTail {
            lifecycleGeneration &+= 1
            finishingSessionID = nil
            finishingTail = nil
            let store = UncheckedSendableBox(value: self.store)
            Task.detached(priority: .utility) {
                await finishTail.value
                store.value.remove(sessionID: sessionID)
                DogfoodTraceStore.record("recording_checkpoint_cancelled", fields: [
                    "session_id": sessionID
                ])
            }
        }
        sessionID = nil
        nextSampleIndex = 0
        nextChunkIndex = 0
        issuedChunks = []
        // A detached link already in flight keeps running (dropping the tail
        // handle does not cancel detached tasks, and task cancellation does
        // not interrupt a synchronous body); its encode/append targets the
        // removed session directory and fails harmlessly. The enqueued
        // removal above still runs to completion after the handle is dropped.
        pendingWrite = nil
    }

    private func write(samples: [Float], startSample: Int) {
        guard let sessionID, !samples.isEmpty else { return }
        let index = nextChunkIndex
        // Reserve the cursor and index up front: the write completes off-main,
        // so the next checkpoint must not re-snapshot this range or collide
        // on the chunk index. A failed write leaves the range out of the
        // manifest; `issuedChunks` remembers it so `finish` can rewrite it
        // from the still-resident buffer before the session is discarded.
        issuedChunks.append((index: index, start: startSample, count: samples.count))
        nextSampleIndex += samples.count
        nextChunkIndex += 1
        let store = UncheckedSendableBox(value: self.store)
        let url = self.store.chunkURL(sessionID: sessionID, index: index)
        let sampleCount = samples.count
        enqueueWrite {
            do {
                try PCMWavWriter.write(
                    samples: samples,
                    sampleRate: Int(WarmAudioCapture.targetSampleRate),
                    to: url
                )
                try store.value.appendChunk(
                    sessionID: sessionID,
                    index: index,
                    startSample: startSample,
                    sampleCount: sampleCount,
                    fileName: url.lastPathComponent
                )
            } catch {
                DogfoodTraceStore.record("recording_checkpoint_failed", fields: [
                    "session_id": sessionID,
                    "chunk_index": index,
                    "error": error.localizedDescription
                ])
            }
        }
    }

    /// Appends one unit of disk work to the serial write chain. Each link is
    /// a detached task that first awaits the previous tail, so all chunk
    /// writes and manifest mutations run off the main actor yet strictly in
    /// order.
    private func enqueueWrite(_ work: @escaping @Sendable () -> Void) {
        let previous = pendingWrite
        pendingWrite = Task.detached(priority: .utility) {
            await previous?.value
            work()
        }
    }

    private func cancelTask() {
        checkpointTask?.cancel()
        checkpointTask = nil
    }
}

/// `RecordingCheckpointStore` holds no mutable instance state — every
/// mutation is an atomic file replace — so it is safe to call from the
/// detached write chain. It cannot be annotated `@unchecked Sendable`
/// directly (that file is outside this change's scope), so it crosses the
/// `@Sendable` boundary boxed instead, matching the `SavedAudioStore`
/// justification for the detached save in RecordingSystemSurface under
/// `SWIFT_STRICT_CONCURRENCY = complete`.
private struct UncheckedSendableBox<T>: @unchecked Sendable {
    let value: T
}
