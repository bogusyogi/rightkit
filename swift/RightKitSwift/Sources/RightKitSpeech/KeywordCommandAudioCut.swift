import Foundation

/// Matches desktop confident_static15_terminal_cut: cut in a quiet 200 ms
/// window before the emitted token timestamp, which can lag the audible Z.
/// No confident seam means cutting at confirmed KWS onset; final text cleanup
/// removes only measured onset residue.
enum KeywordCommandAudioCut {
    struct Result {
        let cutAt: Int
        let source: String
    }

    static func resolve(samples: [Float], wakeStartSample onset: Int, speechStartSample: Int?) -> Result {
        guard onset > 0, onset <= samples.count else {
            return Result(cutAt: samples.count, source: "implausible_timestamp")
        }
        let floor = speechStartSample ?? 0
        guard floor >= 0, floor < onset else {
            return Result(cutAt: samples.count, source: "no_speech_boundary")
        }
        // This function is entered only for a confirmed Sherpa KWS command.
        // When no quiet seam exists, match desktop's forced no-pause path:
        // retain dictated words in final ASR, then strip measured onset residue
        // through RustSpeechProviders. Never use this fallback for a normal
        // user stop or an unconfirmed wake-like transcript.
        let onsetCut = Result(cutAt: onset, source: "kws_onset")
        let window = 3_200 // 200 ms at capture's canonical 16 kHz
        let hop = 1_600
        let maxRollback = 12_800
        let lo = max(floor, onset - maxRollback - window / 2)
        guard onset - lo >= window else { return onsetCut }
        func rms(_ start: Int) -> Float {
            var sum: Float = 0
            for index in start..<(start + window) {
                sum += samples[index] * samples[index]
            }
            return (sum / Float(window)).squareRoot()
        }
        var cut = 0
        var best = Float.infinity
        for start in stride(from: lo, through: onset - window, by: hop) {
            let value = rms(start)
            if value < best {
                best = value
                cut = start + window / 2
            }
        }
        guard cut > floor, onset - cut <= maxRollback else { return onsetCut }
        var levels: [Float] = []
        for start in stride(from: floor, through: onset - window, by: hop) {
            let value = rms(start)
            if value.isFinite { levels.append(value) }
        }
        guard !levels.isEmpty else { return onsetCut }
        levels.sort()
        let noiseFloor = levels[(levels.count - 1) / 10]
        let median = levels[levels.count / 2]
        guard best <= noiseFloor, noiseFloor < median * 0.8 else { return onsetCut }
        return Result(cutAt: cut, source: "static15_seam")
    }
}
