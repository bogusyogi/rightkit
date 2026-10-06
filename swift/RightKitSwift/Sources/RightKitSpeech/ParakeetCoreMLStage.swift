import CoreML
import Foundation

struct ParakeetCoreMLContract: Equatable {
    let melOutput: String
    let encoderOutput: String
    let decoderEmbeddingOutput: String
    let decoderStateHOutput: String
    let decoderStateCOutput: String
    let jointOutput: String

    static func resolve(
        pipeline: ParakeetPipelineConfig,
        mel: ParakeetCoreMLStage,
        encoder: ParakeetCoreMLStage,
        decoder: ParakeetCoreMLStage,
        joint: ParakeetCoreMLStage
    ) throws -> ParakeetCoreMLContract {
        let stateShape = [pipeline.predLayers, 1, pipeline.predHidden]
        let stateOutputs = decoder.outputNames(matching: stateShape)
        guard stateOutputs.count >= 2 else {
            throw SpeechError.invalidContract(
                "TextDecoder needs two state outputs shaped \(stateShape), found \(stateOutputs.count)."
            )
        }
        let decoderStates = try resolveDecoderStateOutputs(stateOutputs, stateShape: stateShape)

        return ParakeetCoreMLContract(
            melOutput: try mel.firstOutputName(stage: "MelSpectrogram"),
            encoderOutput: try encoder.firstOutputName(stage: "AudioEncoder"),
            decoderEmbeddingOutput: try decoder.outputName(matching: [1, 1, pipeline.predHidden], stage: "TextDecoder"),
            decoderStateHOutput: decoderStates.h,
            decoderStateCOutput: decoderStates.c,
            jointOutput: try joint.firstOutputName(stage: "Joint")
        )
    }

    static func resolveDecoderStateOutputs(
        _ stateOutputs: [String],
        stateShape: [Int]
    ) throws -> (h: String, c: String) {
        if let semantic = semanticDecoderStateOutputs(stateOutputs) {
            return semantic
        }

        guard stateOutputs.count == 2 else {
            throw SpeechError.invalidContract(
                "TextDecoder state outputs shaped \(stateShape) are ambiguous: \(stateOutputs.joined(separator: ", "))."
            )
        }

        // Current converted artifacts expose state outputs as var_38/var_39.
        // Those are h/c in the MIL output order; keep compatibility while
        // rejecting future ambiguous artifacts with more than two candidates.
        return (h: stateOutputs[0], c: stateOutputs[1])
    }

    private static func semanticDecoderStateOutputs(_ stateOutputs: [String]) -> (h: String, c: String)? {
        let hMatches = stateOutputs.filter { isSemanticStateName($0, suffix: "h") }
        let cMatches = stateOutputs.filter { isSemanticStateName($0, suffix: "c") }
        guard hMatches.count == 1, cMatches.count == 1 else { return nil }
        return (h: hMatches[0], c: cMatches[0])
    }

    private static func isSemanticStateName(_ name: String, suffix: String) -> Bool {
        let normalized = name
            .lowercased()
            .replacingOccurrences(of: "-", with: "_")
        return normalized == "state_\(suffix)"
            || normalized == "new_state_\(suffix)"
            || normalized.hasSuffix("_state_\(suffix)")
            || normalized.hasSuffix("_\(suffix)")
    }
}

struct ParakeetCoreMLStage {
    private let model: MLModel
    private let outputs: [String: [Int]]

    static func load(_ url: URL, computeUnits: MLComputeUnits = .cpuAndNeuralEngine) throws -> ParakeetCoreMLStage {
        let loadURL = url.pathExtension == "mlpackage" ? try MLModel.compileModel(at: url) : url
        let config = MLModelConfiguration()
        // CPU+ANE is the measured production route (HeardRight `ParakeetCoreMLStage.load`): `.all`
        // lets Core ML keep choosing GPU/CPU and starts an ANE bundle compile on every load that is
        // never finalized, orphaning a ~531 MB `.tmp` bundle each time.
        config.computeUnits = computeUnits
        let model = try MLModel(contentsOf: loadURL, configuration: config)
        return ParakeetCoreMLStage(
            model: model,
            outputs: model.modelDescription.outputDescriptionsByName.mapValues { description in
                description.multiArrayConstraint?.shape.map(\.intValue) ?? []
            }
        )
    }

    func firstOutputName(stage: String) throws -> String {
        guard let name = outputs.keys.sorted().first else {
            throw SpeechError.invalidContract("\(stage) has no outputs.")
        }
        return name
    }

    func outputName(matching shape: [Int], stage: String) throws -> String {
        guard let name = outputNames(matching: shape).first else {
            throw SpeechError.invalidContract("\(stage) has no output shaped \(shape).")
        }
        return name
    }

    func outputNames(matching shape: [Int]) -> [String] {
        outputs
            .filter { $0.value == shape }
            .map(\.key)
            .sorted()
    }

    func predict(inputs: [String: MLMultiArray], outputName: String) throws -> MLMultiArray {
        try predict(inputs: inputs, outputNames: [outputName])[0]
    }

    func predict(inputs: [String: MLMultiArray], outputNames: [String]) throws -> [MLMultiArray] {
        let features = try MLDictionaryFeatureProvider(
            dictionary: inputs.mapValues { MLFeatureValue(multiArray: $0) }
        )
        let result = try model.prediction(from: features)
        return try outputNames.map { name in
            guard let array = result.featureValue(for: name)?.multiArrayValue else {
                throw SpeechError.invalidContract("Output \(name) is missing.")
            }
            return array
        }
    }
}
