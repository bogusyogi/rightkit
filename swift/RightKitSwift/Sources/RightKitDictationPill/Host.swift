#if os(macOS)
import AVFoundation
import AppKit
import Foundation

// The seam between the pill and the host process: the callbacks the pill raises, the thread
// hops it needs, and the action sounds. The host maps these onto whatever carries them (a C
// callback table, for one host); nothing here knows what is on the other side.

/// How the newest transcript's copy ended.
public enum RightKitPillCopyLastResult: Sendable, Equatable {
    case copied, empty, failed

    /// 0 copied, 1 history empty, anything else failed.
    public init(code: Int32) {
        switch code {
        case 0: self = .copied
        case 1: self = .empty
        default: self = .failed
        }
    }
}

/// What the pill asks of its host. Every member is required: there is no default that reports
/// success. Each closure is called off the main thread unless noted, and may block.
public struct RightKitPillHost: Sendable {
    /// Copies the text and returns whether the clipboard holds it now. Blocks until the write finished.
    public var copyText: @Sendable (_ text: String) -> Bool
    public var updateHistoryItem: @Sendable (_ id: String, _ transcript: String) -> Void
    public var openHub: @Sendable (_ tab: Int32) -> Void
    public var dismissClipboard: @Sendable () -> Void
    public var dismissHint: @Sendable () -> Void
    public var setScreenshotDestination: @Sendable (_ destination: Int32) -> Void
    public var log: @Sendable (_ surface: String, _ message: String) -> Void
    public var stopRecording: @Sendable () -> Void
    public var cancelProcessing: @Sendable () -> Void
    public var startDictation: @Sendable () -> Void
    public var stopAndSend: @Sendable () -> Void
    public var requestClipboardItems: @Sendable () -> Void
    public var recoverTerminalError: @Sendable (_ generation: Int64) -> Void
    /// Generation of the current error, -1 when unknown. Called on the main thread.
    public var terminalErrorGeneration: @Sendable () -> Int64
    /// Whether startup has stalled. Called on the main thread.
    public var splashStalled: @Sendable (_ waitedSeconds: Int32) -> Bool
    public var splashAction: @Sendable (_ action: Int32) -> Void
    /// Copies the newest transcript. Blocks.
    public var copyLast: @Sendable () -> RightKitPillCopyLastResult

    public init(
        copyText: @escaping @Sendable (String) -> Bool,
        updateHistoryItem: @escaping @Sendable (String, String) -> Void,
        openHub: @escaping @Sendable (Int32) -> Void,
        dismissClipboard: @escaping @Sendable () -> Void,
        dismissHint: @escaping @Sendable () -> Void,
        setScreenshotDestination: @escaping @Sendable (Int32) -> Void,
        log: @escaping @Sendable (String, String) -> Void,
        stopRecording: @escaping @Sendable () -> Void,
        cancelProcessing: @escaping @Sendable () -> Void,
        startDictation: @escaping @Sendable () -> Void,
        stopAndSend: @escaping @Sendable () -> Void,
        requestClipboardItems: @escaping @Sendable () -> Void,
        recoverTerminalError: @escaping @Sendable (Int64) -> Void,
        terminalErrorGeneration: @escaping @Sendable () -> Int64,
        splashStalled: @escaping @Sendable (Int32) -> Bool,
        splashAction: @escaping @Sendable (Int32) -> Void,
        copyLast: @escaping @Sendable () -> RightKitPillCopyLastResult
    ) {
        self.copyText = copyText
        self.updateHistoryItem = updateHistoryItem
        self.openHub = openHub
        self.dismissClipboard = dismissClipboard
        self.dismissHint = dismissHint
        self.setScreenshotDestination = setScreenshotDestination
        self.log = log
        self.stopRecording = stopRecording
        self.cancelProcessing = cancelProcessing
        self.startDictation = startDictation
        self.stopAndSend = stopAndSend
        self.requestClipboardItems = requestClipboardItems
        self.recoverTerminalError = recoverTerminalError
        self.terminalErrorGeneration = terminalErrorGeneration
        self.splashStalled = splashStalled
        self.splashAction = splashAction
        self.copyLast = copyLast
    }
}

enum RightKitPillBounds {
    static let maxLogSurfaceBytes = 64
    static let maxLogMessageBytes = 1024
    static let maxIdentifierBytes = 256
    static let maxTextBytes = 4 * 1024 * 1024
    static let maxPendingLogs = 256

    /// `value` cut to at most `maxBytes` UTF-8 bytes at a character boundary.
    static func cut(_ value: String, maxBytes: Int) -> String {
        guard value.utf8.count > maxBytes else { return value }
        var out = ""
        var used = 0
        for character in value {
            let next = String(character).utf8.count
            if used + next > maxBytes { break }
            out.append(character)
            used += next
        }
        return out
    }
}

/// Dispatches the pill's host callbacks off the main thread, in the order the original bridge
/// did, and keeps log lines raised before the host is ready.
///
/// Thread-safe. Created by the host once, before the first callback can arrive, and held for the process.
public final class RightKitPillCallbackQueue: @unchecked Sendable {
    private let host: RightKitPillHost
    private let lock = NSLock()
    private var ready = false
    private var pendingLogs: [(String, String)] = []

    public init(host: RightKitPillHost) { self.host = host }

    /// The host calls this once its callbacks can be reached; held log lines (at most 256, oldest
    /// dropped first) are sent then.
    public func markCallbacksReady() {
        lock.lock()
        ready = true
        let pending = pendingLogs
        pendingLogs.removeAll(keepingCapacity: false)
        lock.unlock()
        for (surface, message) in pending { emitLog(surface, message) }
    }

    /// Diagnostic line. `surface` and `message` are stable tags and counters, never user content:
    /// the caller guarantees that, and both are cut to 64 and 1024 bytes.
    public func log(_ surface: String, _ message: String) {
        lock.lock()
        guard ready else {
            if pendingLogs.count >= RightKitPillBounds.maxPendingLogs { pendingLogs.removeFirst() }
            pendingLogs.append((surface, message))
            lock.unlock()
            return
        }
        lock.unlock()
        emitLog(surface, message)
    }

    private func emitLog(_ surface: String, _ message: String) {
        let host = host
        let surface = RightKitPillBounds.cut(surface, maxBytes: RightKitPillBounds.maxLogSurfaceBytes)
        let message = RightKitPillBounds.cut(message, maxBytes: RightKitPillBounds.maxLogMessageBytes)
        DispatchQueue.global(qos: .userInitiated).async { host.log(surface, message) }
    }

    private func background(_ work: @escaping @Sendable () -> Void) {
        DispatchQueue.global(qos: .userInitiated).async(execute: work)
    }

    /// Copies `text` on a background queue and runs `completion` on the main queue with whether the
    /// clipboard holds it now. A caller shows "copied" only when `completion(true)` runs.
    public func copyText(_ text: String, completion: @escaping @MainActor @Sendable (Bool) -> Void) {
        let host = host
        guard text.utf8.count <= RightKitPillBounds.maxTextBytes else {
            DispatchQueue.main.async { MainActor.assumeIsolated { completion(false) } }
            return
        }
        background {
            let copied = host.copyText(text)
            DispatchQueue.main.async { MainActor.assumeIsolated { completion(copied) } }
        }
    }

    public func updateHistoryItem(_ id: String, transcript: String) {
        guard id.utf8.count <= RightKitPillBounds.maxIdentifierBytes,
              transcript.utf8.count <= RightKitPillBounds.maxTextBytes else { return }
        let host = host
        background { host.updateHistoryItem(id, transcript) }
    }

    public func openHub(_ tab: Int32) { let host = host; background { host.openHub(tab) } }
    public func dismissClipboard() { let host = host; background { host.dismissClipboard() } }
    public func dismissHint() { let host = host; background { host.dismissHint() } }
    public func setScreenshotDestination(_ destination: Int32) {
        let host = host; background { host.setScreenshotDestination(destination) }
    }
    public func stopRecording() { let host = host; background { host.stopRecording() } }
    public func cancelProcessing() { let host = host; background { host.cancelProcessing() } }
    public func startDictation() { let host = host; background { host.startDictation() } }
    public func stopAndSend() { let host = host; background { host.stopAndSend() } }
    public func requestClipboardItems() { let host = host; background { host.requestClipboardItems() } }
    public func recoverTerminalError(generation: Int64) {
        let host = host; background { host.recoverTerminalError(generation) }
    }
    public func splashAction(_ action: Int32) { let host = host; background { host.splashAction(action) } }

    /// Copies the newest transcript on a background queue and reports on the main queue.
    public func copyLast(completion: @escaping @MainActor @Sendable (RightKitPillCopyLastResult) -> Void) {
        let host = host
        background {
            let result = host.copyLast()
            DispatchQueue.main.async { MainActor.assumeIsolated { completion(result) } }
        }
    }

    /// Generation of the current error (-1 = unknown); read on the main thread when the error timer arms.
    public func terminalErrorGeneration() -> Int64 { host.terminalErrorGeneration() }

    /// The host owns the stall decision, so every splash agrees. Called on the main thread.
    public func splashStalled(waitedSecs: Int) -> Bool { host.splashStalled(Int32(clamping: waitedSecs)) }
}

/// Runs work on the main actor from any thread. The main-thread check and the `DispatchQueue.main`
/// hop establish the executor `assumeIsolated` asserts, and it traps if that were ever wrong.
public enum RightKitPillMain {
    public static func async(_ action: @escaping @MainActor @Sendable () -> Void) {
        if Thread.isMainThread {
            MainActor.assumeIsolated(action)
        } else {
            DispatchQueue.main.async { MainActor.assumeIsolated(action) }
        }
    }

    /// Blocks the caller until the main thread has run `action`. Never call it from the main thread's
    /// own critical sections; the caller is a host thread that is allowed to wait.
    public static func sync<T: Sendable>(_ action: @MainActor @Sendable () -> T) -> T {
        if Thread.isMainThread { return MainActor.assumeIsolated(action) }
        return DispatchQueue.main.sync { MainActor.assumeIsolated(action) }
    }
}

func pillMainAsync(_ work: @escaping @MainActor @Sendable () -> Void) {
    DispatchQueue.main.async { MainActor.assumeIsolated(work) }
}

func pillMainAsyncAfter(_ seconds: Double, _ work: @escaping @MainActor @Sendable () -> Void) {
    DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { MainActor.assumeIsolated(work) }
}

func pillWorkItem(_ work: @escaping @MainActor @Sendable () -> Void) -> DispatchWorkItem {
    DispatchWorkItem { MainActor.assumeIsolated(work) }
}

// MARK: Action sounds

public enum RightKitPillActionSound: String, CaseIterable, Hashable, Sendable {
    case start, stop, stopAndSend, cancel
}

public struct RightKitPillSoundSettings: Sendable, Equatable {
    public var enabled: Bool
    /// 0...100; nil uses the host's `defaultVolumePercent`.
    public var volumePercent: Double?
    public init(enabled: Bool, volumePercent: Double?) {
        self.enabled = enabled
        self.volumePercent = volumePercent
    }
}

/// Plays the pill's short action sounds on one serial queue. The files, the settings and the log
/// sink are the host's; nothing here names a path or a product.
///
/// Thread-safe: all state lives on `queue`. Players are kept for the process lifetime (one per
/// sound, at most four).
public final class RightKitPillActionSounds: @unchecked Sendable {
    private let queue: DispatchQueue
    private let soundURL: @Sendable (RightKitPillActionSound) -> URL?
    private let loadSettings: @Sendable () -> RightKitPillSoundSettings?
    private let defaultVolumePercent: Double
    private let log: @Sendable (String, String) -> Void
    private var players: [RightKitPillActionSound: AVAudioPlayer] = [:]
    private var cachedSettings: RightKitPillSoundSettings?

    /// `queueLabel` names the serial queue (the host picks it). `defaultVolumePercent` is clamped to 0...100.
    public init(
        queueLabel: String,
        defaultVolumePercent: Double,
        soundURL: @escaping @Sendable (RightKitPillActionSound) -> URL?,
        loadSettings: @escaping @Sendable () -> RightKitPillSoundSettings?,
        log: @escaping @Sendable (String, String) -> Void
    ) {
        self.queue = DispatchQueue(label: queueLabel, qos: .userInitiated)
        self.defaultVolumePercent = min(max(defaultVolumePercent.isFinite ? defaultVolumePercent : 0, 0), 100)
        self.soundURL = soundURL
        self.loadSettings = loadSettings
        self.log = log
    }

    public func play(_ sound: RightKitPillActionSound) {
        queue.async { [weak self] in self?.playOnQueue(sound) }
    }

    public func prewarm() {
        queue.async { [weak self] in
            guard let self else { return }
            self.cachedSettings = self.loadSettings()
            for sound in RightKitPillActionSound.allCases { _ = self.player(for: sound) }
        }
    }

    public func refreshSettings() {
        queue.async { [weak self] in
            guard let self else { return }
            self.cachedSettings = self.loadSettings()
        }
    }

    private func playOnQueue(_ sound: RightKitPillActionSound) {
        guard let settings = cachedSettings, settings.enabled else { return }
        guard let player = player(for: sound) else { return }
        player.stop()
        player.currentTime = 0
        let percent = settings.volumePercent.flatMap { $0.isFinite ? $0 : nil } ?? defaultVolumePercent
        player.volume = Float(min(max(percent, 0), 100) / 100)
        let started = player.play()
        log("pill_action_sound", "sound=\(sound.rawValue) outcome=\(started ? "started" : "rejected")")
    }

    private func player(for sound: RightKitPillActionSound) -> AVAudioPlayer? {
        if let player = players[sound] { return player }
        guard let url = soundURL(sound) else {
            log("pill_action_sound", "sound=\(sound.rawValue) outcome=missing")
            return nil
        }
        do {
            let player = try AVAudioPlayer(contentsOf: url)
            player.prepareToPlay()
            players[sound] = player
            return player
        } catch {
            log("pill_action_sound", "sound=\(sound.rawValue) outcome=error")
            return nil
        }
    }
}
#endif
