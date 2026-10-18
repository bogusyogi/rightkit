#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import AVFoundation
import CoreML
import Foundation

struct ParakeetDecodeControlflowResult {
    let hits: [(id: Int, startSec: Float, durationFrames: Int)]
    let predictorCalls: Int
    let jointCalls: Int
    let biasApplications: Int
}

/// Fixed-shape Joint inputs reused for every frame in one decode window.
/// `MLModel.prediction(from:)` is synchronous, so Core ML has finished reading
/// these arrays before the next call mutates them. This removes two
/// per-step MLMultiArray allocations while retaining existing output arrays.
final class ParakeetJointInputBuffers {
    let encoderStep: MLMultiArray
    let decoderStep: MLMultiArray

    init(encoderWidth: Int, predictorWidth: Int) throws {
        encoderStep = try ParakeetCoreMLArray.float16(
            shape: [1, encoderWidth], values: [Float](repeating: 0, count: encoderWidth)
        )
        decoderStep = try ParakeetCoreMLArray.float16(
            shape: [1, predictorWidth], values: [Float](repeating: 0, count: predictorWidth)
        )
    }

    func write(encoder: [Float], decoder: [Float]) throws {
        try ParakeetCoreMLArray.writeFloat16(encoderStep, values: encoder)
        try ParakeetCoreMLArray.writeFloat16(decoderStep, values: decoder)
    }
}

/// Production beam controlflow with model calls supplied as a seam. The app
/// passes Core ML predictor/Joint closures; smoke tests pass deterministic
/// fakes, so cache-on/off output and call-count parity are directly asserted.
func parakeetDecodeBeam(
    realFrames: Int,
    maxSymbols: Int,
    blank: Int32,
    width: Int,
    isTDT: Bool,
    durations: [Int],
    initialStateH: [Float],
    initialStateC: [Float],
    windowOffset: Float,
    frameSeconds: Float,
    bias: ASRVocabularyBias,
    predictorCacheEnabled: Bool,
    emitReward: Float = parakeetDefaultEmitReward,
    encoderStep: (Int) -> [Float],
    predict: (ParakeetBeamHyp) throws -> DecoderPredictorStep,
    joint: (DecoderPredictorStep, [Float]) throws -> [Float]
) throws -> ParakeetDecodeControlflowResult {
    let width = max(width, 1)
    var beam: [(hyp: ParakeetBeamHyp, predictor: DecoderPredictorStep?)] = [
        (ParakeetBeamHyp(
            h: initialStateH,
            c: initialStateC,
            lastTok: blank,
            t: 0,
            sym: 0,
            score: 0,
            hits: []
        ), nil)
    ]
    var predictorCalls = 0
    var jointCalls = 0
    var biasApplications = 0
    let maxRounds = (realFrames + 1) * max(maxSymbols, 1) + 4
    var rounds = 0

    while beam.contains(where: { $0.hyp.t < realFrames }) {
        rounds += 1
        if rounds > maxRounds { break }
        var next: [(hyp: ParakeetBeamHyp, predictor: DecoderPredictorStep?)] = []
        next.reserveCapacity(beam.count * width)

        for entry in beam {
            let hyp = entry.hyp
            if hyp.t >= realFrames {
                next.append(entry)
                continue
            }
            if hyp.sym >= maxSymbols {
                next.append((parakeetForceTimeAdvance(hyp), entry.predictor))
                continue
            }

            let predictor: DecoderPredictorStep
            if predictorCacheEnabled, let cached = entry.predictor {
                predictor = cached
            } else {
                predictor = try predict(hyp)
                predictorCalls += 1
            }
            let logits = try joint(predictor, encoderStep(hyp.t))
            jointCalls += 1
            guard !logits.isEmpty else {
                throw SpeechError.invalidContract("Joint output is empty.")
            }

            let emitted = hyp.hits.map(\.id)
            let bonuses = bias.nextTokenBonuses(emitted: emitted)
            if !bonuses.isEmpty { biasApplications += 1 }
            let children: [ParakeetBeamHyp]
            if isTDT {
                guard logits.count > durations.count else {
                    throw SpeechError.invalidContract("Joint TDT output is missing token logits.")
                }
                children = parakeetTdtStep(
                    hyp: hyp,
                    logits: logits,
                    numDurations: durations.count,
                    durations: durations,
                    blank: blank,
                    maxSymbols: maxSymbols,
                    width: width,
                    winStartSec: windowOffset,
                    frameSeconds: frameSeconds,
                    bonuses: bonuses,
                    emitReward: emitReward,
                    newH: predictor.stateH,
                    newC: predictor.stateC
                )
            } else {
                children = parakeetRnntStep(
                    hyp: hyp,
                    logits: logits,
                    blank: blank,
                    width: width,
                    winStartSec: windowOffset,
                    frameSeconds: frameSeconds,
                    bonuses: bonuses,
                    emitReward: emitReward,
                    newH: predictor.stateH,
                    newC: predictor.stateC
                )
            }
            let parentHitCount = hyp.hits.count
            next.append(contentsOf: children.map { child in
                (
                    child,
                    parakeetCarryPredictorCache(
                        enabled: predictorCacheEnabled,
                        parentHitCount: parentHitCount,
                        childHitCount: child.hits.count,
                        predictor: predictor
                    )
                )
            })
        }

        // Rust's stable sort preserves insertion order for equal scores. Swift
        // sort is unstable, so carry the original offset into the comparator.
        next = next.enumerated().sorted { lhs, rhs in
            if lhs.element.hyp.score == rhs.element.hyp.score {
                return lhs.offset < rhs.offset
            }
            return lhs.element.hyp.score > rhs.element.hyp.score
        }.map(\.element)
        if next.count > width {
            next.removeLast(next.count - width)
        }
        beam = next
    }

    beam = beam.enumerated().sorted { lhs, rhs in
        if lhs.element.hyp.score == rhs.element.hyp.score {
            return lhs.offset < rhs.offset
        }
        return lhs.element.hyp.score > rhs.element.hyp.score
    }.map(\.element)
    return ParakeetDecodeControlflowResult(
        hits: beam.first?.hyp.hits ?? [],
        predictorCalls: predictorCalls,
        jointCalls: jointCalls,
        biasApplications: biasApplications
    )
}

extension ParakeetCoreMLRuntime {
    func decodeWindow(
        _ segment: [Float], maxSymbols: Int, hits: inout [(Int, Float)],
        bias: ASRVocabularyBias, timer: PhaseTimer, windowOffset: Float = 0
    ) throws {
        let windowSamples = pipeline.windowSamples
        guard segment.count <= windowSamples else {
            throw SpeechError.invalidContract(
                "Audio segment has \(segment.count) samples; model window supports \(windowSamples)."
            )
        }
        var padded = Array(repeating: Float(0), count: windowSamples)
        if !segment.isEmpty {
            padded.replaceSubrange(0..<segment.count, with: segment)
        }

        let melStart = CFAbsoluteTimeGetCurrent()
        let melFull = try mel.predict(
            inputs: [
                "audio": ParakeetCoreMLArray.float16(shape: [1, windowSamples], values: padded),
                "audio_len": ParakeetCoreMLArray.int32(shape: [1], values: [Int32(segment.count)]),
            ],
            outputName: contract.melOutput
        )
        timer.melMS += (CFAbsoluteTimeGetCurrent() - melStart) * 1000
        let melReadStart = CFAbsoluteTimeGetCurrent()
        let melFullValues = try ParakeetCoreMLArray.readFloats(melFull)
        timer.melMS += (CFAbsoluteTimeGetCurrent() - melReadStart) * 1000
        let melFramesFull = melFullValues.count / pipeline.nMel
        guard melFramesFull >= pipeline.melFrames else {
            throw SpeechError.invalidContract("Mel output has too few frames.")
        }

        var melValues = Array(repeating: Float(0), count: pipeline.nMel * pipeline.melFrames)
        for channel in 0..<pipeline.nMel {
            for frame in 0..<pipeline.melFrames {
                melValues[channel * pipeline.melFrames + frame] = melFullValues[channel * melFramesFull + frame]
            }
        }

        let encStart = CFAbsoluteTimeGetCurrent()

        let encoderOutput = try encoder.predict(
            inputs: [
                "melspectrogram_features": ParakeetCoreMLArray.float16(
                    shape: [1, pipeline.nMel, pipeline.melFrames],
                    values: melValues
                ),
                "mel_len": ParakeetCoreMLArray.int32(shape: [1], values: [Int32(pipeline.melFrames)]),
            ],
            outputName: contract.encoderOutput
        )
        timer.encoderMS += (CFAbsoluteTimeGetCurrent() - encStart) * 1000
        let encReadStart = CFAbsoluteTimeGetCurrent()
        let encoded = try ParakeetCoreMLArray.readFloats(encoderOutput)
        timer.encoderReadMS += (CFAbsoluteTimeGetCurrent() - encReadStart) * 1000
        let encoderWidth = pipeline.encoderWidth
        let encoderFrames = encoded.count / encoderWidth
        let realFrames = min(encoderFrames, Int(ceil(Float(segment.count) / 160 / 8)))
        let jointInputs = try ParakeetJointInputBuffers(
            encoderWidth: encoderWidth, predictorWidth: pipeline.predHidden
        )

        // Beam search over TDT/RNN-T joint outputs, one LSTM state per
        // hypothesis. Parity port of macOS's `decode_window` — see
        // `ParakeetBeamSearch.swift` for the scoring core this drives.
        // Fresh decoder state per window, one hypothesis to start.
        let blank = Int32(pipeline.blankID)
        let width = parakeetClampBeamWidth(options.beamWidth)
        let predictorCacheEnabled = options.predictorCacheEnabled
        let decodeStart = CFAbsoluteTimeGetCurrent()
        let result = try parakeetDecodeBeam(
            realFrames: realFrames,
            maxSymbols: maxSymbols,
            blank: blank,
            width: width,
            isTDT: pipeline.isTDT,
            durations: pipeline.durations,
            initialStateH: [Float](repeating: 0, count: pipeline.predLayers * pipeline.predHidden),
            initialStateC: [Float](repeating: 0, count: pipeline.predLayers * pipeline.predHidden),
            windowOffset: windowOffset,
            frameSeconds: Self.frameSeconds,
            bias: bias,
            predictorCacheEnabled: predictorCacheEnabled,
            emitReward: options.emitReward,
            encoderStep: { frame in
                let gatherStart = CFAbsoluteTimeGetCurrent()
                var step = [Float](repeating: 0, count: encoderWidth)
                for dim in 0..<encoderWidth {
                    step[dim] = encoded[dim * encoderFrames + frame]
                }
                timer.gatherMS += (CFAbsoluteTimeGetCurrent() - gatherStart) * 1000
                return step
            },
            predict: { hyp in
                try predictDecoder(lastToken: hyp.lastTok, stateH: hyp.h, stateC: hyp.c)
            },
            joint: { predictor, encoderStep in
                let step = try predictJoint(
                    predictor: predictor,
                    encoderStep: encoderStep,
                    timer: timer,
                    inputs: jointInputs
                )
                return step.logits
            }
        )
        timer.predictorCalls = result.predictorCalls
        timer.jointCalls = result.jointCalls
        timer.biasApplications += result.biasApplications
        timer.decoderMS += (CFAbsoluteTimeGetCurrent() - decodeStart) * 1000
        hits.append(contentsOf: result.hits.map { ($0.id, $0.startSec) })
    }

    func predictDecoder(
        lastToken: Int32,
        stateH: [Float],
        stateC: [Float]
    ) throws -> DecoderPredictorStep {
        let stateShape = [pipeline.predLayers, 1, pipeline.predHidden]
        let decoderOutputs = try decoder.predict(
            inputs: [
                "decoder_input_ids": ParakeetCoreMLArray.int32(shape: [1, 1], values: [lastToken]),
                "state_h": ParakeetCoreMLArray.float16(shape: stateShape, values: stateH),
                "state_c": ParakeetCoreMLArray.float16(shape: stateShape, values: stateC),
            ],
            outputNames: [
                contract.decoderEmbeddingOutput,
                contract.decoderStateHOutput,
                contract.decoderStateCOutput,
            ]
        )
        let embedding = try ParakeetCoreMLArray.readFloats(decoderOutputs[0])
        let newStateH = try ParakeetCoreMLArray.readFloats(decoderOutputs[1])
        let newStateC = try ParakeetCoreMLArray.readFloats(decoderOutputs[2])
        return DecoderPredictorStep(embedding: embedding, stateH: newStateH, stateC: newStateC)
    }

    func predictJoint(
        predictor: DecoderPredictorStep,
        encoderStep: [Float],
        timer: PhaseTimer,
        inputs: ParakeetJointInputBuffers
    ) throws -> DecoderJointStep {
        try inputs.write(encoder: encoderStep, decoder: predictor.embedding)
        let jointStart = CFAbsoluteTimeGetCurrent()
        let jointOutput = try joint.predict(
            inputs: [
                "encoder_step": inputs.encoderStep,
                "decoder_step": inputs.decoderStep,
            ],
            outputName: contract.jointOutput
        )
        timer.jointMS += (CFAbsoluteTimeGetCurrent() - jointStart) * 1000
        let logitsStart = CFAbsoluteTimeGetCurrent()
        let logits = try ParakeetCoreMLArray.readFloats(jointOutput)
        timer.logitsMS += (CFAbsoluteTimeGetCurrent() - logitsStart) * 1000
        return DecoderJointStep(stateH: predictor.stateH, stateC: predictor.stateC, logits: logits)
    }

}
#endif
