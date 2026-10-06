import Accelerate
import Foundation


enum WhisperMelSpectrogram {
    static let nFFT = 400
    static let hop = 160
    static let nMels = 128
    static let nFreq = 201
    static let windowSamples = 480_000

    private static let hannWindow: [Float] = (0..<nFFT).map {
        0.5 - 0.5 * cos(2.0 * .pi * Float($0) / Float(nFFT))
    }

    private static let cosTable: [Float] = {
        var table = Array(repeating: Float(0), count: nFFT * nFreq)
        for sample in 0..<nFFT {
            for freq in 0..<nFreq {
                table[sample * nFreq + freq] = cos(2.0 * .pi * Float(sample * freq) / Float(nFFT))
            }
        }
        return table
    }()

    private static let sinTable: [Float] = {
        var table = Array(repeating: Float(0), count: nFFT * nFreq)
        for sample in 0..<nFFT {
            for freq in 0..<nFreq {
                table[sample * nFreq + freq] = -sin(2.0 * .pi * Float(sample * freq) / Float(nFFT))
            }
        }
        return table
    }()

    static func logMel(windowedSamples samples: [Float], filterURL: URL? = nil) throws -> [Float] {
        var padded = Array(repeating: Float(0), count: windowSamples)
        if !samples.isEmpty {
            padded.replaceSubrange(0..<min(samples.count, windowSamples), with: samples.prefix(windowSamples))
        }
        return try logMel(paddedSamples: padded, filterURL: filterURL)
    }

    static func logMel(paddedSamples samples: [Float], filterURL: URL? = nil) throws -> [Float] {
        precondition(samples.count == windowSamples, "Whisper mel expects a 30 s padded window.")
        let filters = try loadFilters(filterURL: filterURL)
        let pad = nFFT / 2
        var reflected = Array(repeating: Float(0), count: samples.count + (2 * pad))
        for index in 0..<pad {
            reflected[index] = samples[pad - index]
        }
        reflected.replaceSubrange(pad..<(pad + samples.count), with: samples)
        for index in 0..<pad {
            reflected[pad + samples.count + index] = samples[samples.count - 2 - index]
        }

        let frames = (reflected.count - nFFT) / hop
        var frameMatrix = Array(repeating: Float(0), count: frames * nFFT)
        for frame in 0..<frames {
            let frameOffset = frame * nFFT
            let sampleOffset = frame * hop
            for sample in 0..<nFFT {
                frameMatrix[frameOffset + sample] = reflected[sampleOffset + sample] * hannWindow[sample]
            }
        }

        var real = Array(repeating: Float(0), count: frames * nFreq)
        var imag = real
        vDSP_mmul(
            frameMatrix, 1,
            cosTable, 1,
            &real, 1,
            vDSP_Length(frames),
            vDSP_Length(nFreq),
            vDSP_Length(nFFT)
        )
        vDSP_mmul(
            frameMatrix, 1,
            sinTable, 1,
            &imag, 1,
            vDSP_Length(frames),
            vDSP_Length(nFreq),
            vDSP_Length(nFFT)
        )

        var power = Array(repeating: Float(0), count: nFreq * frames)
        for frame in 0..<frames {
            for freq in 0..<nFreq {
                let value = (real[frame * nFreq + freq] * real[frame * nFreq + freq])
                    + (imag[frame * nFreq + freq] * imag[frame * nFreq + freq])
                power[freq * frames + frame] = value
            }
        }

        var mel = Array(repeating: Float(0), count: nMels * frames)
        vDSP_mmul(
            filters, 1,
            power, 1,
            &mel, 1,
            vDSP_Length(nMels),
            vDSP_Length(frames),
            vDSP_Length(nFreq)
        )

        var maxValue = -Float.infinity
        for index in mel.indices {
            mel[index] = log10(max(mel[index], 1e-10))
            maxValue = max(maxValue, mel[index])
        }
        let floor = maxValue - 8.0
        for index in mel.indices {
            mel[index] = (max(mel[index], floor) + 4.0) / 4.0
        }
        return mel
    }

    static func loadFilters(filterURL: URL? = nil) throws -> [Float] {
        let url: URL
        if let filterURL {
            url = filterURL
        } else if let bundled = Bundle.module.url(forResource: "whisper_mel_128", withExtension: "bin") {
            url = bundled
        } else {
            throw SpeechError.missingFilterbank
        }

        let data = try Data(contentsOf: url)
        let count = data.count / MemoryLayout<Float>.size
        guard count == nMels * nFreq else {
            throw SpeechError.invalidFilterbank(count)
        }
        return data.withUnsafeBytes { raw in
            Array(raw.bindMemory(to: Float.self))
        }
    }
}
