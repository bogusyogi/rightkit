import AVFoundation
import Foundation
import Testing

@testable import RightKitAudio

struct CaptureRawHealthTests {
    private func health(callbacks: Int = 10, frames: Int = 40_000, zeroFraction: Double = 0.01, maxAbs: Float = 0.2) -> CaptureRawHealth {
        CaptureRawHealth(
            callbackCount: callbacks, frameCount: frames, zeroSampleFraction: zeroFraction, maxAbs: maxAbs, rms: 0.05,
            firstCallbackMs: 20, firstNonzeroMs: 20, inputSampleRate: 48_000, inputChannelCount: 1, elapsedMs: 1_000)
    }

    @Test func verdictSeparatesDeadFromQuietFromHealthy() {
        #expect(health().verdict == .healthy)
        #expect(health(callbacks: 0, frames: 0).verdict == .deadNoCallbacks)
        #expect(health(maxAbs: 0).verdict == .deadZeroSignal)
        #expect(health(zeroFraction: 1).verdict == .deadZeroSignal)
        #expect(health(maxAbs: 0.0005).verdict == .suspectLowSignal)
        #expect(!health(maxAbs: 0.0005).verdict.isDead, "a quiet room must never be reported as a dead path")
        #expect(health(maxAbs: 0).verdict.isDead)
    }

    @Test func deadPathErrorCarriesTheHealthItWasJudgedOn() {
        let error = WarmAudioCaptureError.capturePathDead(health(callbacks: 0, frames: 0))
        #expect(error.errorDescription?.contains("dead_no_callbacks") == true)
    }
}

struct WarmAudioCaptureConversionTests {
    @Test func meterLevelMapsDecibelsToZeroOne() {
        #expect(WarmAudioCapture.meterLevel(rms: 0) == 0)
        #expect(WarmAudioCapture.meterLevel(rms: 1) == 1)
        #expect(abs(WarmAudioCapture.meterLevel(rms: 0.1) - (40.0 / 60.0)) < 1e-6)  // -20 dBFS
        #expect(WarmAudioCapture.meterLevel(rms: 0.0001) == 0)  // -80 dBFS is below the floor
    }

    @Test func defaultOutputFormatIsSixteenKilohertzMonoFloat() {
        let format = WarmAudioCapture.Configuration().outputFormat
        #expect(format.sampleRate == 16_000)
        #expect(format.channelCount == 1)
        #expect(format.commonFormat == .pcmFormatFloat32)
    }

    @Test func tapBuffersConvertToTheCallerFormat() throws {
        let input = try #require(AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48_000, channels: 1, interleaved: false))
        let output = WarmAudioCapture.mono16kFloat
        let converter = try #require(AVAudioConverter(from: input, to: output))
        let buffer = try #require(AVAudioPCMBuffer(pcmFormat: input, frameCapacity: 4_800))
        buffer.frameLength = 4_800
        let samples = try #require(buffer.floatChannelData?[0])
        for index in 0..<4_800 { samples[index] = 0.5 * sin(Float(index) * 2 * .pi * 440 / 48_000) }

        let converted = try #require(WarmAudioCapture.convert(buffer, using: converter, to: output))
        #expect(converted.format.sampleRate == 16_000)
        // 4,800 frames at 48 kHz are 1,600 at 16 kHz; the converter holds back a few frames of filter latency.
        #expect(converted.frameLength > 1_000 && converted.frameLength <= 1_600)
        #expect(WarmAudioCapture.level(of: converted) > 0.5, "a half-scale tone must register on the meter")
    }

    @Test func silenceConvertsToAZeroLevel() throws {
        let input = try #require(AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 44_100, channels: 1, interleaved: false))
        let output = WarmAudioCapture.mono16kFloat
        let converter = try #require(AVAudioConverter(from: input, to: output))
        let buffer = try #require(AVAudioPCMBuffer(pcmFormat: input, frameCapacity: 4_410))
        buffer.frameLength = 4_410
        let converted = try #require(WarmAudioCapture.convert(buffer, using: converter, to: output))
        #expect(WarmAudioCapture.level(of: converted) == 0)
    }

    @Test func aStoppedCaptureReportsNotRunningAndStopIsIdempotent() {
        let capture = WarmAudioCapture()
        #expect(!capture.isRunning)
        capture.stop()
        capture.stop()
        #expect(!capture.isRunning)
        #expect(capture.rawHealth.verdict == .deadNoCallbacks)
    }

    @Test func recoveringWithAStaleGenerationIsRefused() {
        let capture = WarmAudioCapture()
        #expect(throws: WarmAudioCaptureError.lifecycleInvalidated) {
            try capture.recoverAfterConfigurationChange(expectedGeneration: capture.generation &+ 7)
        }
    }

    @Test(.disabled("Needs a live microphone and record permission; run by hand on a device."))
    func startingTwiceThrowsAlreadyRunning() throws {
        let capture = WarmAudioCapture()
        try capture.start()
        defer { capture.stop() }
        #expect(throws: WarmAudioCaptureError.alreadyRunning) { try capture.start() }
    }
}
