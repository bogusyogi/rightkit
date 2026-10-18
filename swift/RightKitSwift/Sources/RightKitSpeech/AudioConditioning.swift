#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation

public enum AudioConditioning {
    public static func conditionForASR(_ samples: [Float], sampleRate: UInt32 = 16_000, policy: String = "asr_simple_gain_hpf") -> [Float] {
        guard !samples.isEmpty, policy.lowercased() != "raw" else {
            return samples
        }

        let finite = samples.map { $0.isFinite ? $0 : 0 }
        let mean = finite.reduce(Float(0), +) / Float(finite.count)
        let centered = finite.map { $0 - mean }
        var conditioned = highPassOnePole(centered, sampleRate: max(sampleRate, 1), cutoffHz: 80)
        recenter(&conditioned)
        let level = rms(conditioned)
        if level > 0, level <= dbToAmp(-31) {
            let gain = min(max(dbToAmp(-20) / level, 1), 12)
            return conditioned.map { softLimit($0 * gain) }
        }
        return conditioned.map { min(max($0, -0.99), 0.99) }
    }

    public static func rms(_ samples: [Float]) -> Float {
        guard !samples.isEmpty else { return 0 }
        let sumSquares = samples.reduce(Float(0)) { partial, sample in
            partial + sample * sample
        }
        return sqrt(sumSquares / Float(samples.count))
    }

    private static func dbToAmp(_ db: Float) -> Float { pow(10, db / 20) }

    private static func highPassOnePole(_ samples: [Float], sampleRate: UInt32, cutoffHz: Float) -> [Float] {
        guard !samples.isEmpty else { return [] }
        let dt = Float(1) / Float(sampleRate)
        let rc = Float(1) / (2 * .pi * max(cutoffHz, 1))
        let alpha = rc / (rc + dt)
        var output: [Float] = []
        output.reserveCapacity(samples.count)
        var previousY: Float = 0
        var previousX = samples[0]
        for sample in samples {
            let value = alpha * (previousY + sample - previousX)
            output.append(value)
            previousY = value
            previousX = sample
        }
        return output
    }

    private static func recenter(_ samples: inout [Float]) {
        guard !samples.isEmpty else { return }
        let mean = samples.reduce(Float(0), +) / Float(samples.count)
        for index in samples.indices { samples[index] -= mean }
    }

    private static func softLimit(_ sample: Float) -> Float {
        abs(sample) <= 0.99 ? sample : min(max(tanh(sample), -0.99), 0.99)
    }
}
#endif
