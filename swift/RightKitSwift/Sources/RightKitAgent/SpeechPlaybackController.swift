// CodeRight-owned: RightKitSwift 0.8.1 has no TTS playback; nothing to adopt (not serious, report I-4)
import AVFoundation
import Foundation
import Observation

/// Session-scoped native playback for completed ``SpeechSynthesisResult`` values.
/// Synthesis and playback stay on device; no provider or network path is involved.
@Observable
@MainActor
final class SpeechPlaybackController {
    enum State: Equatable {
        case idle
        case synthesizing
        case playing
    }

    private enum PlaybackError: LocalizedError {
        case invalidResult
        case audioBufferUnavailable

        var errorDescription: String? {
            switch self {
            case .invalidResult:
                "The speech result had an invalid PCM payload."
            case .audioBufferUnavailable:
                "The device could not allocate a speech playback buffer."
            }
        }
    }

    private(set) var state: State = .idle
    private(set) var errorMessage: String?

    var isReading: Bool { state != .idle }
    var isPlaying: Bool { state == .playing }
    var isSynthesizing: Bool { state == .synthesizing }

    private var synthesisTask: Task<Void, Never>?
    private var audioEngine: AVAudioEngine?
    private var playerNode: AVAudioPlayerNode?
    private var requestID = UUID()
    private var playbackID = UUID()

    func read(text: String) {
        stop()
        let requestID = UUID()
        self.requestID = requestID
        state = .synthesizing
        errorMessage = nil

        synthesisTask = Task { [weak self] in
            guard let self else { return }
            do {
                let result = try await SpeechSynthesisPipeline().synthesize(text)
                try Task.checkCancellation()
                guard self.requestID == requestID else { return }
                try self.play(result, requestID: requestID)
            } catch is CancellationError {
                guard self.requestID == requestID else { return }
                self.state = .idle
            } catch {
                guard self.requestID == requestID else { return }
                self.state = .idle
                self.errorMessage = error.localizedDescription
            }
        }
    }

    /// Stops pending synthesis and native playback, releasing the audio session.
    func stop() {
        requestID = UUID()
        playbackID = UUID()
        synthesisTask?.cancel()
        synthesisTask = nil
        playerNode?.stop()
        audioEngine?.stop()
        playerNode = nil
        audioEngine = nil
        try? AVAudioSession.sharedInstance().setActive(
            false,
            options: [.notifyOthersOnDeactivation]
        )
        state = .idle
        errorMessage = nil
    }

    func dismissError() {
        errorMessage = nil
    }

    private func play(_ result: SpeechSynthesisResult, requestID: UUID) throws {
        guard result.format.channels == 1,
              result.format.bitsPerSample == 32,
              result.format.isFloat,
              result.frameCount > 0,
              result.audio.count == result.frameCount * MemoryLayout<Float>.size
        else {
            throw PlaybackError.invalidResult
        }
        guard let format = AVAudioFormat(
            standardFormatWithSampleRate: result.format.sampleRate,
            channels: AVAudioChannelCount(result.format.channels)
        ), let buffer = AVAudioPCMBuffer(
            pcmFormat: format,
            frameCapacity: AVAudioFrameCount(result.frameCount)
        ), let channelData = buffer.floatChannelData?[0]
        else {
            throw PlaybackError.audioBufferUnavailable
        }

        result.audio.withUnsafeBytes { (bytes: UnsafeRawBufferPointer) in
            guard let source = bytes.baseAddress else { return }
            channelData.update(
                from: source.assumingMemoryBound(to: Float.self),
                count: result.frameCount
            )
        }
        buffer.frameLength = AVAudioFrameCount(result.frameCount)

        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playback, mode: .spokenAudio, options: [.duckOthers])
        try session.setActive(true)

        let engine = AVAudioEngine()
        let player = AVAudioPlayerNode()
        engine.attach(player)
        engine.connect(player, to: engine.mainMixerNode, format: format)
        try engine.start()

        let playbackID = UUID()
        self.playbackID = playbackID
        player.scheduleBuffer(buffer, completionCallbackType: .dataPlayedBack) { @Sendable [weak self] _ in
            Task { @MainActor [weak self] in
                guard let self,
                      self.requestID == requestID,
                      self.playbackID == playbackID
                else { return }
                self.finishPlayback()
            }
        }
        audioEngine = engine
        playerNode = player
        state = .playing
        player.play()
    }

    private func finishPlayback() {
        playerNode?.stop()
        audioEngine?.stop()
        playerNode = nil
        audioEngine = nil
        try? AVAudioSession.sharedInstance().setActive(
            false,
            options: [.notifyOthersOnDeactivation]
        )
        state = .idle
        synthesisTask = nil
    }
}
