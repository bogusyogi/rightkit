import Foundation

public enum VoiceActivityEvent: Equatable, Sendable {
    case speechStarted
    case speechContinued
    case speechEnded
    case silence
}

/// Lightweight, allocation-free VAD for capture UX and checkpoint policy.
/// ASR remains the authority for transcript content; this only classifies
/// energy with an adaptive noise floor and hysteresis.
public final class VoiceActivityDetector: @unchecked Sendable {
    public init() {}
    private let lock = NSLock()
    private var noiseFloor: Float = 0.008
    private var speechActive = false
    private var quietFrames = 0
    private let startMultiplier: Float = 2.6
    private let endMultiplier: Float = 1.45
    private let endFrameCount = 8

    public func ingest(samples: UnsafeBufferPointer<Float>) -> VoiceActivityEvent {
        guard !samples.isEmpty else { return .silence }
        var sum: Float = 0
        for sample in samples { sum += sample * sample }
        let rms = (sum / Float(samples.count)).squareRoot()
        return ingest(rms: rms)
    }

    public func ingest(rms: Float) -> VoiceActivityEvent {
        lock.lock(); defer { lock.unlock() }
        let boundedRMS = max(0, rms)
        if !speechActive {
            noiseFloor = noiseFloor * 0.96 + min(boundedRMS, noiseFloor * 2) * 0.04
        }
        let startThreshold = max(0.012, noiseFloor * startMultiplier)
        let endThreshold = max(0.008, noiseFloor * endMultiplier)
        if speechActive {
            if boundedRMS < endThreshold {
                quietFrames += 1
                if quietFrames >= endFrameCount {
                    speechActive = false
                    quietFrames = 0
                    return .speechEnded
                }
            } else {
                quietFrames = 0
            }
            return .speechContinued
        }
        guard boundedRMS >= startThreshold else { return .silence }
        speechActive = true
        quietFrames = 0
        return .speechStarted
    }

    public var isSpeechActive: Bool {
        lock.lock(); defer { lock.unlock() }
        return speechActive
    }

    public func reset() {
        lock.lock()
        noiseFloor = 0.008
        speechActive = false
        quietFrames = 0
        lock.unlock()
    }
}
