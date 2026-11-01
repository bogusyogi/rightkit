#if os(watchOS)
import Foundation
import WatchConnectivity
import RightKitWatchBridge

// Binds WatchConnectivity callbacks to RightKitWatchBridge's durable transfer
// ledger while retaining HeardRight's recording policy, telemetry, & wire
// compatibility.
final class WatchRecordingTransfer: NSObject, WCSessionDelegate {
    static let shared = WatchRecordingTransfer()

    private struct LegacyPendingTransfer: Codable {
        let recording: WatchRecording
        var fileURL: URL
        var deliveryState: String?
        /// When this item entered `.awaitingAcknowledgement` (or was last
        /// reset there by a confirmed OS-level transport completion in
        /// `didFinish`). Missing/nil on records persisted before this field
        /// existed; treated as "retry now" rather than "never retry".
        var awaitingSince: Date?
    }

    private static let pendingKey = "watch.pending-transfers.v1"
    private static let acknowledgedFilesKey = "watch.acknowledged-files.v1"
    /// `didFinish` confirms the OS finished moving bytes to the phone; after
    /// that, only the phone-side `watchRecordingAck` round trip is
    /// outstanding. Without an explicit move back to `.queued`, a transfer
    /// whose ack never arrives (e.g. the watch suspended mid round-trip)
    /// sits stuck until a relaunch or the next recording's `enqueue`.
    /// Re-sending past this timeout is
    /// safe: the phone's receive() dedups by recording ID and re-acks either
    /// way.
    /// Debounce for the window between `transferFile` returning and the OS
    /// listing it in `outstandingFileTransfers`. Not a retry deadline.
    private let stateLock = NSRecursiveLock()
    private var pending = TransferLedger<WatchRecording>(registrationGrace: 3)
    /// `transferFile` can return before WatchConnectivity exposes its handle
    /// in `outstandingFileTransfers`. Retain returned handles through that
    /// registration window so delete can cancel the exact transfer.
    private var activeTransfers: [String: WCSessionFileTransfer] = [:]
    private var lastError: String?
    /// Keeps the watch app alive past the few seconds watchOS normally
    /// allows after `stop()` returns, so a just-enqueued transfer (and its
    /// acknowledgement) has a real chance to drain instead of sitting queued
    /// until the next recording wakes the app.
    var onStateChange: ((WatchTransferSnapshot) -> Void)?
    var onModesChange: (([WatchMode]) -> Void)?
    var onProcessingResult: ((WatchProcessingResult) -> Void)?

    var snapshot: WatchTransferSnapshot {
        stateLock.lock()
        defer { stateLock.unlock() }
        let pendingCount = pending.pendingCount
        if let lastError {
            return WatchTransferSnapshot(status: .failed(lastError, pending: pendingCount), pendingCount: pendingCount)
        }
        guard pendingCount > 0 else {
            return WatchTransferSnapshot(status: .idle, pendingCount: 0)
        }
        if pending.entries.contains(where: { $0.state == .queued || $0.state == nil }) {
            return WatchTransferSnapshot(status: .queued(pendingCount), pendingCount: pendingCount)
        }
        return WatchTransferSnapshot(status: .awaitingAcknowledgement(pendingCount), pendingCount: pendingCount)
    }

    override private init() {
        if let data = UserDefaults.standard.data(forKey: Self.pendingKey) {
            if let decoded = try? JSONDecoder().decode([TransferLedgerEntry<WatchRecording>].self, from: data) {
                pending = TransferLedger(entries: decoded, registrationGrace: 3)
            } else if let legacy = try? JSONDecoder().decode([LegacyPendingTransfer].self, from: data) {
                pending = TransferLedger(
                    entries: legacy.map { item in
                        TransferLedgerEntry(
                            id: item.recording.id.uuidString,
                            payload: item.recording,
                            fileName: item.recording.fileName,
                            state: item.deliveryState == "awaitingAcknowledgement" ? .awaitingAcknowledgement : .queued,
                            awaitingSince: item.awaitingSince
                        )
                    },
                    registrationGrace: 3
                )
            }
        }
        super.init()
        persistPending()
        sweepAcknowledgedOrphans()
    }

    func activate() {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard WCSession.isSupported() else {
            WatchTelemetry.record("session_unsupported")
            return
        }
        sweepAcknowledgedOrphans()
        WCSession.default.delegate = self
        WCSession.default.activate()
        let store = WatchModeStore()
        store.replaceModes(from: WCSession.default.receivedApplicationContext)
        WatchTelemetry.setTransferEnabled(
            WCSession.default.receivedApplicationContext[WatchTelemetryKeys.telemetryEnabledKey] as? Bool == true
        )
        onModesChange?(store.modes)
        publish()
    }

    func refresh() {
        stateLock.lock()
        defer { stateLock.unlock() }
        let session = WCSession.default
        guard session.activationState == .activated else { return }
        reconcileReceipts(through: session)
        flushIfReady()
    }

    func enqueue(recording: WatchRecording, fileURL: URL) {
        stateLock.lock()
        defer { stateLock.unlock() }
        if !pending.contains(recording.id.uuidString) {
            _ = pending.enqueue(TransferLedgerEntry(
                id: recording.id.uuidString,
                payload: recording,
                fileName: recording.fileName
            ))
            persistPending()
        }
        lastError = nil
        WatchTelemetry.record("transfer_queued", fields: [
            "recording_id": recording.id.uuidString,
            "pending": pending.pendingCount,
            "bytes": recording.byteCount,
        ])
        publish()
        guard WCSession.isSupported() else { return }
        flushIfReady()
    }

    func isPending(recordingID: UUID) -> Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return pending.contains(recordingID.uuidString)
    }

    /// Discard a local take & remove it from durable delivery state. Cancel
    /// any OS-level transfer first; otherwise a queued retry could reference
    /// an audio file the user already deleted.
    func discard(recordingID: UUID, fileURL: URL) throws {
        stateLock.lock()
        defer { stateLock.unlock() }
        let id = recordingID.uuidString
        let outstanding = WCSession.default.outstandingFileTransfers.filter {
            ($0.file.metadata?["id"] as? String) == id
        }
        var cancellationHandles = outstanding
        if let active = activeTransfers.removeValue(forKey: id),
           !cancellationHandles.contains(where: { $0 === active }) {
            cancellationHandles.append(active)
        }
        for transfer in cancellationHandles {
            transfer.cancel()
        }

        let removedItems = pending.remove(id: id)
        if !removedItems.isEmpty {
            // Persist queue removal before touching audio. If Watch is killed
            // between these operations, relaunch cannot retry a missing file.
            persistPending()
        }

        do {
            if FileManager.default.fileExists(atPath: fileURL.path) {
                try FileManager.default.removeItem(at: fileURL)
            }
        } catch {
            // Keep failed deletion recoverable: restore only removed IDs that
            // were not concurrently re-added, then retry from queued state.
            pending.restore(removedItems)
            persistPending()
            lastError = error.localizedDescription
            publish()
            flushIfReady()
            throw error
        }

        lastError = nil
        WatchTelemetry.record("transfer_discarded", fields: [
            "recording_id": id,
            "pending": pending.pendingCount,
        ])
        publish()
    }

    // MARK: - Unsent retention (#170)

    private static let lastReachableKey = "watch.last-reachable-at.v1"
    private static let lastRecoveryTelemetryKey = "watch.recovery-telemetry-at.v1"
    private static let recoveryTelemetryInterval: TimeInterval = 6 * 3600
    private var lastPublishedListSignature: String?

    /// Everything still owned by the Watch (not yet acked by the phone).
    func pendingItems() -> [WatchPendingItem] {
        stateLock.lock()
        defer { stateLock.unlock() }
        return pending.entries.map {
            WatchPendingItem(
                id: $0.id,
                startedAt: $0.payload.startedAt,
                duration: $0.payload.duration,
                byteCount: $0.payload.byteCount
            )
        }.sorted { $0.startedAt < $1.startedAt }
    }

    /// False once unsent audio reaches the safety cap. Callers refuse the NEW
    /// recording; old ones are never dropped.
    func admitsNewRecording() -> Bool {
        let bytes = pendingItems().reduce(Int64(0)) { $0 + $1.byteCount }
        let admitted = WatchPendingPolicy.admitsNewRecording(unsentBytes: bytes)
        if !admitted {
            WatchTelemetry.record("recording_refused_storage_cap", fields: [
                "pending": pendingItems().count,
                "bytes": bytes,
                "cap_bytes": WatchPendingPolicy.capBytes,
                "severity": "loud",
            ])
        }
        return admitted
    }

    /// Delete the named unsent recordings (phone purge or Watch escape hatch)
    /// and return the IDs actually removed.
    @discardableResult
    func purge(ids: [String], reason: String) -> [String] {
        stateLock.lock()
        defer { stateLock.unlock() }
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        var removed: [String] = []
        for id in WatchPendingPolicy.purgeTargets(requested: ids, pending: pendingItems()) {
            guard let entry = pending.entries.first(where: { $0.id == id }),
                  let uuid = UUID(uuidString: id) else { continue }
            let url = WatchRecordingDelivery.restoredURL(fileName: entry.fileName, documents: documents)
            if (try? discard(recordingID: uuid, fileURL: url)) != nil { removed.append(id) }
        }
        WatchTelemetry.record("unsent_recordings_purged", fields: [
            "reason": reason,
            "count": removed.count,
            "pending": pending.pendingCount,
        ])
        return removed
    }

    /// Watch escape hatch: re-checks the 7-day gate itself, never trusting the UI.
    @discardableResult
    func purgeAllUnsentIfEligible(now: Date = Date()) -> Int {
        let items = pendingItems()
        guard WatchPendingPolicy.escapeHatchAvailable(items, now: now) else { return 0 }
        return purge(ids: items.map(\.id), reason: "watch_escape_hatch").count
    }

    private func retry(ids: [String]) {
        stateLock.lock()
        defer { stateLock.unlock() }
        for id in WatchPendingPolicy.purgeTargets(requested: ids, pending: pendingItems()) {
            _ = pending.rollback(id: id)
        }
        persistPending()
        WatchTelemetry.record("unsent_recordings_retry_requested", fields: ["count": ids.count])
        publish()
        flushIfReady()
    }

    private func handlePendingCommand(_ payload: [String: Any]) {
        stateLock.lock()
        defer { stateLock.unlock() }
        if let ids = payload[WatchPendingPolicy.retryKey] as? [String] { retry(ids: ids) }
        if let ids = payload[WatchPendingPolicy.purgeKey] as? [String] {
            let removed = purge(ids: ids, reason: "phone_purge")
            sendToPhone([WatchPendingPolicy.purgedKey: removed])
        }
        if payload[WatchPendingPolicy.listRequestKey] != nil {
            lastPublishedListSignature = nil
            publish()
        }
    }

    private func sendToPhone(_ payload: [String: Any]) {
        let session = WCSession.default
        guard session.activationState == .activated else { return }
        _ = DualPathSender(transport: WCSessionTransport(session: session)).send(payload)
    }

    /// Mirror the unsent list to the phone (latest-wins application context) and
    /// emit the 24 h recovery telemetry at most every 6 h.
    private func publishPendingListAndRecovery() {
        let items = pendingItems()
        let signature = items.map(\.id).joined(separator: ",")
        let session = WCSession.default
        if signature != lastPublishedListSignature, session.activationState == .activated {
            lastPublishedListSignature = signature
            try? session.updateApplicationContext([
                WatchPendingPolicy.listKey: items.map(\.wire),
                WatchPendingPolicy.listUpdatedKey: Date().timeIntervalSince1970,
            ])
        }
        let now = Date()
        guard WatchPendingPolicy.needsRecoveryPrompt(items, now: now) else { return }
        let defaults = UserDefaults.standard
        let last = defaults.double(forKey: Self.lastRecoveryTelemetryKey)
        guard now.timeIntervalSince1970 - last >= Self.recoveryTelemetryInterval else { return }
        defaults.set(now.timeIntervalSince1970, forKey: Self.lastRecoveryTelemetryKey)
        let summary = WatchPendingPolicy.summary(items, now: now)
        let reachable = defaults.double(forKey: Self.lastReachableKey)
        WatchTelemetry.record("unsent_recordings_waiting", fields: [
            "count": summary.count,
            "oldest_age_s": Int(summary.oldestAge),
            "bytes": summary.bytes,
            "last_reachable_at": reachable > 0
                ? ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: reachable))
                : "never",
        ])
    }

    private func noteReachable(_ session: WCSession) {
        if session.isReachable {
            UserDefaults.standard.set(Date().timeIntervalSince1970, forKey: Self.lastReachableKey)
        }
    }

    private func flushIfReady() {
        stateLock.lock()
        defer { stateLock.unlock() }
        let session = WCSession.default
        guard session.activationState == .activated else { return }
        let outstandingTransfers = session.outstandingFileTransfers
        let outstandingIDs = Set(outstandingTransfers.compactMap {
            $0.file.metadata?["id"] as? String
        })
        for transfer in outstandingTransfers {
            if let id = transfer.file.metadata?["id"] as? String {
                activeTransfers[id] = transfer
            }
        }
        for id in Array(activeTransfers.keys) where !outstandingIDs.contains(id) {
            guard let item = pending.entries.first(where: { $0.id == id }),
                  let awaitingSince = item.awaitingSince,
                  Date().timeIntervalSince(awaitingSince) < pending.registrationGrace else {
                activeTransfers.removeValue(forKey: id)
                continue
            }
        }
        let activeIDs = outstandingIDs.union(activeTransfers.keys)
        let dispatch = pending.claimDispatches(
            now: Date(),
            activeIDs: activeIDs,
            fileExists: { entry in
                let url = WatchRecordingDelivery.restoredURL(
                    fileName: entry.fileName,
                    documents: FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
                )
                return FileManager.default.fileExists(atPath: url.path)
            }
        )
        for id in dispatch.missingFiles {
            lastError = "Saved audio unavailable. Open HeardRight on iPhone to check delivery."
        }
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        for item in dispatch.dispatches {
            guard let entry = pending.entries.first(where: { $0.id == item.id }) else { continue }
            let fileURL = WatchRecordingDelivery.restoredURL(fileName: entry.fileName, documents: documents)
            let transfer = session.transferFile(fileURL, metadata: metadata(for: entry.payload))
            activeTransfers[item.id] = transfer
            WatchTelemetry.record(item.isRetry ? "transfer_retry" : "transfer_started", fields: [
                "recording_id": item.id,
                "pending": pending.pendingCount,
            ])
        }
        persistPending()
        publish()
    }

    /// Best-effort nudge so the phone can wake and prime ASR. Delivery is
    /// never assumed: `sendMessage` requires reachability and simply fails
    /// otherwise, which is why `transferFile` stays canonical.
    func sendWakeHint(phase: String) {
        let session = WCSession.default
        guard session.activationState == .activated, session.isReachable else { return }
        WCSessionTransport(session: session).sendMessage(
            [WatchRecordingDelivery.bridgeKeys.wakeHint: phase],
            onError: { _ in }
        )
    }

    private func metadata(for recording: WatchRecording) -> [String: Any] {
        let result: [String: Any] = [
            "sourceTag": "appleWatch",
            "id": recording.id.uuidString,
            "startedAt": recording.startedAt.timeIntervalSince1970,
            "duration": recording.duration,
            "byteCount": recording.byteCount,
            "fileName": recording.fileName,
        ]
        // iPhone owns recording mode; omit legacy Watch selection so its
        // selectedPolishMode remains authoritative when processing this take.
        return result
    }

    private func persistPending() {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard let data = pending.encoded() else { return }
        UserDefaults.standard.set(data, forKey: Self.pendingKey)
    }

    func session(
        _ session: WCSession,
        activationDidCompleteWith activationState: WCSessionActivationState,
        error: Error?
    ) {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard error == nil, activationState == .activated else {
            lastError = error?.localizedDescription
            WatchTelemetry.record("session_activation_failed", fields: [
                "state": activationState.rawValue,
                "error": error?.localizedDescription ?? "unknown",
            ])
            publish()
            return
        }
        noteReachable(session)
        WatchTelemetry.record("session_activated", fields: ["pending": pending.pendingCount])
        WatchTelemetry.flushPending()
        restoreOutstandingTransfers(from: session)
        reconcileReceipts(through: session)
        flushIfReady()
    }

    /// Retry queued transfers the moment the phone becomes reachable again.
    ///
    /// Running `flushIfReady` only from `enqueue` and session
    /// activation would leave a clip queued while the phone was unreachable
    /// stuck until the NEXT recording was enqueued. Watch/phone reachability flaps constantly, so
    /// this is the natural retry signal.
    func sessionReachabilityDidChange(_ session: WCSession) {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard session.activationState == .activated else { return }
        noteReachable(session)
        WatchTelemetry.record("session_reachability_changed", fields: [
            "reachable": session.isReachable,
            "pending": pending.pendingCount,
        ])
        WatchTelemetry.flushPending()
        reconcileReceipts(through: session)
        flushIfReady()
    }

    /// Ask only about pending IDs; a durable phone receipt is the sole authority
    /// for clearing them, even if the original acknowledgement was lost.
    private func reconcileReceipts(through session: WCSession) {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard session.isReachable, pending.pendingCount > 0 else { return }
        let ids = pending.entries.map(\.id)
        for requested in WatchReceipts.queryChunks(ids) {
            let keys = WatchRecordingDelivery.bridgeKeys
            session.sendMessage([keys.receiptQuery: requested], replyHandler: { [weak self] reply in
                self?.stateLock.lock()
                defer { self?.stateLock.unlock() }
                guard let acknowledged = reply[keys.receipts] as? [String] else { return }
                for id in WatchRecordingDelivery.confirmedIDs(requested: requested, received: Set(acknowledged)) {
                    self?.handleAcknowledgement([WatchRecordingDelivery.bridgeKeys.acknowledgement: id])
                }
            }, errorHandler: { _ in })
        }
    }

    func session(_ session: WCSession, didReceiveUserInfo userInfo: [String: Any] = [:]) {
        stateLock.lock()
        defer { stateLock.unlock() }
        if userInfo["modes"] != nil {
            let store = WatchModeStore()
            store.replaceModes(from: userInfo)
            onModesChange?(store.modes)
        }
        handlePendingCommand(userInfo)
        handleAcknowledgement(userInfo)
        handleRollback(userInfo)
        handleProcessingResult(userInfo)
    }

    func session(_ session: WCSession, didReceiveMessage message: [String: Any]) {
        stateLock.lock()
        defer { stateLock.unlock() }
        handlePendingCommand(message)
        handleAcknowledgement(message)
        handleRollback(message)
        handleProcessingResult(message)
    }

    func session(_ session: WCSession, didReceiveApplicationContext applicationContext: [String: Any]) {
        stateLock.lock()
        defer { stateLock.unlock() }
        let store = WatchModeStore()
        store.replaceModes(from: applicationContext)
        WatchTelemetry.setTransferEnabled(applicationContext[WatchTelemetryKeys.telemetryEnabledKey] as? Bool == true)
        onModesChange?(store.modes)
    }

    func session(
        _ session: WCSession,
        didFinish fileTransfer: WCSessionFileTransfer,
        error: Error?
    ) {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard let id = fileTransfer.file.metadata?["id"] as? String else { return }
        // A failed delete can restore the same ID & start a replacement
        // transfer before canceled transfer's callback arrives. Ignore that
        // stale callback so it cannot clear or mutate replacement state.
        if let current = activeTransfers[id], current !== fileTransfer { return }
        if activeTransfers[id] === fileTransfer {
            activeTransfers.removeValue(forKey: id)
        }
        if let error {
            pending.transportFinished(id: id, failed: true)
            lastError = error.localizedDescription
            WatchTelemetry.record("transfer_failed", fields: [
                "recording_id": id,
                "pending": pending.pendingCount,
                "error": error.localizedDescription,
            ])
        } else {
            // Receiver acknowledgement, not transport completion, closes
            // durable delivery. The watch file remains until
            // WatchRecordingInbox acknowledges its manifest write. The
            // app-level ack wait starts now, not when `transferFile` was
            // called — the retry timeout should measure how long we've
            // waited on the round trip, not how long the OS took to move
            // the bytes.
            pending.transportFinished(id: id, failed: false)
            lastError = nil
            WatchTelemetry.record("transfer_completed", fields: [
                "recording_id": id,
                "pending": pending.pendingCount,
            ])
        }
        persistPending()
        publish()
    }

    private func restoreOutstandingTransfers(from session: WCSession) {
        stateLock.lock()
        defer { stateLock.unlock() }
        let outstandingIDs = Set(session.outstandingFileTransfers.compactMap {
            $0.file.metadata?["id"] as? String
        })
        for transfer in session.outstandingFileTransfers {
            if let id = transfer.file.metadata?["id"] as? String {
                activeTransfers[id] = transfer
            }
        }
        pending.restoreOutstanding(outstandingIDs)
        persistPending()
    }

    private func handleAcknowledgement(_ payload: [String: Any]) {
        stateLock.lock()
        defer { stateLock.unlock() }
        let keys = WatchRecordingDelivery.bridgeKeys
        guard let acknowledgedID = payload[keys.acknowledgement] as? String,
              let item = pending.entries.first(where: { $0.id == acknowledgedID }) else { return }
        let fileURL = WatchRecordingDelivery.restoredURL(
            fileName: item.fileName,
            documents: FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        )
        rememberAcknowledgedFile(item.fileName)
        activeTransfers.removeValue(forKey: acknowledgedID)
        _ = pending.acknowledge(id: acknowledgedID)
        try? FileManager.default.removeItem(at: fileURL)
        lastError = nil
        persistPending()
        WatchTelemetry.record("transfer_acknowledged", fields: [
            "recording_id": acknowledgedID,
            "pending": pending.pendingCount,
        ])
        publish()
    }

    /// Ack can arrive just before a Watch process is killed. Keep a bounded
    /// filename receipt so the next launch can remove that orphan safely;
    /// unacknowledged files remain untouched for retry.
    private func rememberAcknowledgedFile(_ fileName: String) {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard !fileName.isEmpty else { return }
        var files = UserDefaults.standard.stringArray(forKey: Self.acknowledgedFilesKey) ?? []
        files.removeAll { $0 == fileName }
        files.insert(fileName, at: 0)
        UserDefaults.standard.set(Array(files.prefix(250)), forKey: Self.acknowledgedFilesKey)
    }

    private func sweepAcknowledgedOrphans() {
        stateLock.lock()
        defer { stateLock.unlock() }
        let names = Set(UserDefaults.standard.stringArray(forKey: Self.acknowledgedFilesKey) ?? [])
        guard !names.isEmpty else { return }
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let directory = documents.appendingPathComponent("Recordings", isDirectory: true)
        guard let files = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) else { return }
        let pendingNames = Set(pending.entries.map(\.fileName))
        for file in files where names.contains(file.lastPathComponent) && !pendingNames.contains(file.lastPathComponent) {
            try? FileManager.default.removeItem(at: file)
        }
    }

    /// Consumer for `WatchRecordingInbox.rollbackIngest` (phone side,
    /// `HeardRight/WatchRecordingInbox.swift`). The phone sends this when
    /// `WCSession` delivered a file but nothing durable exists for it yet
    /// (see the contract comment there). Without a consumer, an item stuck
    /// `.awaitingAcknowledgement` for a delivery that will never durably ack
    /// just sits there — `flushIfReady`'s `stranded` reconciliation only
    /// fires once WatchConnectivity stops listing it as outstanding, which
    /// does not happen for a rollback. Resetting to `.queued` here makes the
    /// next `flushIfReady` (called immediately, and again on reachability
    /// changes) re-send the same immutable file instead of losing it.
    private func handleRollback(_ payload: [String: Any]) {
        stateLock.lock()
        defer { stateLock.unlock() }
        let keys = WatchRecordingDelivery.bridgeKeys
        guard let recordingID = payload[keys.rollback] as? String else { return }
        let reason = payload[keys.rollbackReason] as? String ?? "unknown"
        guard pending.contains(recordingID) else {
            WatchTelemetry.record("transfer_rollback_unknown", fields: [
                "recording_id": recordingID,
                "reason": reason,
            ])
            return
        }
        _ = pending.rollback(id: recordingID)
        lastError = reason
        persistPending()
        WatchTelemetry.record("transfer_rolled_back", fields: [
            "recording_id": recordingID,
            "reason": reason,
            "pending": pending.pendingCount,
        ])
        publish()
        flushIfReady()
    }

    private func handleProcessingResult(_ payload: [String: Any]) {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard let result = payload[WatchTelemetryKeys.processingResultKey] as? [String: Any],
              let recordingID = result["recordingID"] as? String,
              let succeeded = result["succeeded"] as? Bool,
              let message = result["message"] as? String else { return }
        guard WatchRecordingRecoveryStore.shouldAcceptProcessingResult(recordingID: recordingID) else {
            WatchTelemetry.record("processing_result_stale", fields: [
                "recording_id": recordingID,
            ])
            return
        }
        let preview = (result["preview"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
        let fingerprint = "\(recordingID)|\(succeeded)|\(message)|\(preview ?? "")"
        var seen = Set(UserDefaults.standard.stringArray(forKey: "watch.processing-results.v1") ?? [])
        guard !seen.contains(fingerprint) else { return }
        seen.insert(fingerprint)
        UserDefaults.standard.set(Array(seen.suffix(250)), forKey: "watch.processing-results.v1")
        // Phone acknowledgement may have already removed the Watch file and
        // manifest row. Keep terminal processing state independently so that
        // ack-before-ASR ordering cannot erase the result.
        WatchRecordingRecoveryStore.saveProcessingResult(WatchProcessingResultRecord(
            recordingID: recordingID,
            succeeded: succeeded,
            message: message,
            preview: preview.flatMap {
                $0.isEmpty ? nil : String($0.prefix(WatchRecordingDelivery.transcriptPreviewCharacterLimit))
            }
        ))
        WatchTelemetry.record(succeeded ? "processing_succeeded" : "processing_failed", fields: [
            "recording_id": recordingID,
            "result": succeeded ? "success" : "failure",
        ])
        onProcessingResult?(WatchProcessingResult(
            recordingID: recordingID,
            succeeded: succeeded,
            message: message,
            preview: preview.flatMap {
                $0.isEmpty ? nil : String($0.prefix(WatchRecordingDelivery.transcriptPreviewCharacterLimit))
            }
        ))
    }

    private func publish() {
        stateLock.lock()
        defer { stateLock.unlock() }
        onStateChange?(snapshot)
        publishPendingListAndRecovery()
    }

}
#endif
