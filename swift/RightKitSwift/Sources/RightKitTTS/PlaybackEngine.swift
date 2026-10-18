import Foundation
@preconcurrency import AVFoundation

/// The audio graph under ``PlaybackController``: a player node feeding a time-pitch unit. Internal so the
/// controller's state machine can be tested without audio hardware.
@MainActor
protocol PlaybackEngine: AnyObject {
    /// Called when the engine stopped itself because the audio configuration changed (route or format).
    var onConfigurationChange: (@MainActor () -> Void)? { get set }
    /// Builds a fresh graph for `format` and starts the engine. The player does not play until `play()`.
    func start(format: AVAudioFormat, rate: Float) throws
    /// Queues a buffer. `completion` runs, off the main actor, when the buffer has been played out.
    func schedule(_ buffer: AVAudioPCMBuffer, completion: @escaping @Sendable () -> Void)
    func play()
    func pause()
    /// Drops every queued buffer (their completions are not guaranteed to run) without stopping the engine.
    func resetQueue()
    /// Stops the player and the engine and releases the graph.
    func stop()
    func setRate(_ rate: Float)
}

@MainActor
final class AVAudioPlaybackEngine: PlaybackEngine {
    var onConfigurationChange: (@MainActor () -> Void)?

    private var engine: AVAudioEngine?
    private var player: AVAudioPlayerNode?
    #if !os(watchOS)
    private var timePitch: AVAudioUnitTimePitch?
    #endif
    private var observer: NSObjectProtocol?

    func start(format: AVAudioFormat, rate: Float) throws {
        stop()
        let engine = AVAudioEngine()
        let player = AVAudioPlayerNode()
        engine.attach(player)
        #if os(watchOS)
        // watchOS has no AVAudioUnitTimePitch: play at the natural rate.
        engine.connect(player, to: engine.mainMixerNode, format: format)
        #else
        let timePitch = AVAudioUnitTimePitch()
        timePitch.rate = rate
        engine.attach(timePitch)
        engine.connect(player, to: timePitch, format: format)
        engine.connect(timePitch, to: engine.mainMixerNode, format: format)
        #endif
        observer = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.onConfigurationChange?() }
        }
        do {
            try engine.start()
        } catch {
            if let observer { NotificationCenter.default.removeObserver(observer) }
            observer = nil
            throw error
        }
        self.engine = engine
        self.player = player
        #if !os(watchOS)
        self.timePitch = timePitch
        #endif
    }

    func schedule(_ buffer: AVAudioPCMBuffer, completion: @escaping @Sendable () -> Void) {
        player?.scheduleBuffer(buffer, completionCallbackType: .dataPlayedBack) { _ in completion() }
    }

    func play() { player?.play() }
    func pause() { player?.pause() }
    func resetQueue() { player?.stop() }

    func stop() {
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = nil
        player?.stop()
        engine?.stop()
        player = nil
        #if !os(watchOS)
        timePitch = nil
        #endif
        engine = nil
    }

    func setRate(_ rate: Float) {
        #if !os(watchOS)
        timePitch?.rate = rate
        #endif
    }
}
