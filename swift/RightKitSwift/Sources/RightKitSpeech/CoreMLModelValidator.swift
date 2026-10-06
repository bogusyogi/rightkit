import CoreML
import Foundation

struct CoreMLValidationReport {
    let stages: [String]
    let pipeline: ParakeetPipelineConfig
    let vocabSize: Int
    let validatedAt: Date
}

struct CoreMLArtifactValidationReport {
    let backend: String
    let stages: [String]
    let validatedAt: Date
}

struct ParakeetPipelineConfig {
    let isTDT: Bool
    let nMel: Int
    let windowSec: Double
    let melFrames: Int
    let encFrames: Int
    let encoderWidth: Int
    let predLayers: Int
    let predHidden: Int
    let blankID: Int
    let durations: [Int]

    var windowSamples: Int {
        Int(windowSec * 16_000)
    }

    static func load(from modelDirectory: URL) throws -> ParakeetPipelineConfig {
        let url = modelDirectory.appendingPathComponent("pipeline.json")
        let data = try Data(contentsOf: url)
        let object = try JSONSerialization.jsonObject(with: data)
        guard let json = object as? [String: Any] else {
            throw SpeechError.invalidPipeline("pipeline.json must be a JSON object.")
        }

        let parsedDurations = (json["tdt_durations"] as? [Any])?
            .compactMap { ($0 as? NSNumber)?.intValue }
            .filter { $0 >= 0 }
        let durations = parsedDurations?.isEmpty == false ? parsedDurations ?? [] : [0, 1, 2, 3, 4]
        let config = ParakeetPipelineConfig(
            isTDT: (json["is_tdt"] as? Bool) ?? true,
            nMel: intValue(json["n_mel"], default: 128),
            windowSec: doubleValue(json["window_sec"], default: 15.0),
            melFrames: intValue(json["mel_frames"], default: 1500),
            encFrames: intValue(json["enc_frames"], default: 187),
            encoderWidth: intValue(json["enc_d_model"], default: 1024),
            predLayers: intValue(json["pred_layers"], default: 2),
            predHidden: intValue(json["pred_hidden"], default: 640),
            blankID: intValue(json["blank_id"], default: 8192),
            durations: durations
        )
        try config.validate()
        return config
    }

    private func validate() throws {
        guard nMel > 0 else { throw SpeechError.invalidPipeline("n_mel must be positive.") }
        guard windowSec.isFinite, windowSec > 0 else {
            throw SpeechError.invalidPipeline("window_sec must be positive.")
        }
        guard melFrames > 0 else { throw SpeechError.invalidPipeline("mel_frames must be positive.") }
        guard encFrames > 0 else { throw SpeechError.invalidPipeline("enc_frames must be positive.") }
        guard encoderWidth > 0 else { throw SpeechError.invalidPipeline("enc_d_model must be positive.") }
        guard predLayers > 0 else { throw SpeechError.invalidPipeline("pred_layers must be positive.") }
        guard predHidden > 0 else { throw SpeechError.invalidPipeline("pred_hidden must be positive.") }
        guard blankID >= 0 else { throw SpeechError.invalidPipeline("blank_id must be non-negative.") }
        guard !durations.isEmpty else {
            throw SpeechError.invalidPipeline("tdt_durations must not be empty.")
        }
    }

    private static func intValue(_ value: Any?, default defaultValue: Int) -> Int {
        (value as? NSNumber)?.intValue ?? defaultValue
    }

    private static func doubleValue(_ value: Any?, default defaultValue: Double) -> Double {
        (value as? NSNumber)?.doubleValue ?? defaultValue
    }
}

struct ParakeetVocabulary {
    let pieces: [String]

    static func load(from modelDirectory: URL, blankID: Int) throws -> ParakeetVocabulary {
        let text = try String(contentsOf: modelDirectory.appendingPathComponent("vocab.txt"), encoding: .utf8)
        let pieces = text
            .split(whereSeparator: \.isNewline)
            .map { line in
                line.split(separator: "\t", maxSplits: 1, omittingEmptySubsequences: false)
                    .first
                    .map(String.init) ?? ""
            }
        guard !pieces.isEmpty else {
            throw SpeechError.invalidVocabulary("vocab.txt is empty.")
        }
        guard blankID <= pieces.count else {
            throw SpeechError.invalidVocabulary("blank_id \(blankID) is outside vocab size \(pieces.count).")
        }
        return ParakeetVocabulary(pieces: pieces)
    }
}

struct CoreMLModelValidator {
    static let readyMarkerName = "coreml-ready.json"
    private static let stageNames = ["MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint"]

    func validateInstalledModel(
        at modelDirectory: URL,
        loadModel: (URL) throws -> Void = CoreMLModelValidator.loadModel
    ) throws -> CoreMLValidationReport {
        guard FileManager.default.fileExists(atPath: modelDirectory.appendingPathComponent("pipeline.json").path) else {
            throw SpeechError.missingFile("pipeline.json")
        }
        guard FileManager.default.fileExists(atPath: modelDirectory.appendingPathComponent("vocab.txt").path) else {
            throw SpeechError.missingFile("vocab.txt")
        }

        let pipeline = try ParakeetPipelineConfig.load(from: modelDirectory)
        let vocabulary = try ParakeetVocabulary.load(from: modelDirectory, blankID: pipeline.blankID)

        var loadedStages: [String] = []
        for stage in Self.stageNames {
            let stageURL = try Self.stageURL(for: stage, in: modelDirectory)
            try loadModel(stageURL)
            loadedStages.append(stage)
        }

        let report = CoreMLValidationReport(
            stages: loadedStages,
            pipeline: pipeline,
            vocabSize: vocabulary.pieces.count,
            validatedAt: Date()
        )
        try Self.writeReadyMarker(report, to: modelDirectory)
        return report
    }

    func validateInstalledArtifact(
        backend: String,
        requiredFiles: [String],
        requiredStageNames: [String],
        at modelDirectory: URL,
        loadModel: (URL) throws -> Void = CoreMLModelValidator.loadModel
    ) throws -> CoreMLArtifactValidationReport {
        if requiredFiles.contains("pipeline.json"), requiredFiles.contains("vocab.txt") {
            let report = try validateInstalledModel(at: modelDirectory, loadModel: loadModel)
            return CoreMLArtifactValidationReport(
                backend: backend,
                stages: report.stages,
                validatedAt: report.validatedAt
            )
        }

        for file in requiredFiles {
            guard FileManager.default.fileExists(atPath: modelDirectory.appendingPathComponent(file).path) else {
                throw SpeechError.missingFile(file)
            }
        }

        var loadedStages: [String] = []
        for stage in requiredStageNames {
            let stageURL = try Self.stageURL(for: stage, in: modelDirectory)
            try loadModel(stageURL)
            loadedStages.append(stage)
        }

        let report = CoreMLArtifactValidationReport(
            backend: backend,
            stages: loadedStages,
            validatedAt: Date()
        )
        try Self.writeReadyMarker(report, to: modelDirectory)
        return report
    }

    static func hasReadyMarker(at modelDirectory: URL) -> Bool {
        FileManager.default.fileExists(atPath: modelDirectory.appendingPathComponent(readyMarkerName).path)
    }

    static func stageURL(for stage: String, in modelDirectory: URL) throws -> URL {
        let compiled = modelDirectory.appendingPathComponent("\(stage).mlmodelc", isDirectory: true)
        if FileManager.default.fileExists(atPath: compiled.path) {
            return compiled
        }

        let package = modelDirectory.appendingPathComponent("\(stage).mlpackage", isDirectory: true)
        if FileManager.default.fileExists(atPath: package.path) {
            return package
        }

        throw SpeechError.missingStage(stage)
    }

    private static func loadModel(_ url: URL) throws {
        let loadURL = url.pathExtension == "mlpackage" ? try MLModel.compileModel(at: url) : url
        let config = MLModelConfiguration()
        // Match production runtime loading. `.all` may route through GPU and
        // repeatedly trigger expensive ANE compilation; CPU+ANE is the
        // measured production path used by `ParakeetCoreMLStage.load`.
        config.computeUnits = .cpuAndNeuralEngine
        _ = try MLModel(contentsOf: loadURL, configuration: config)
    }

    private static func writeReadyMarker(_ report: CoreMLValidationReport, to modelDirectory: URL) throws {
        let payload: [String: Any] = [
            "blankID": report.pipeline.blankID,
            "durations": report.pipeline.durations,
            "encDModel": report.pipeline.encoderWidth,
            "melFrames": report.pipeline.melFrames,
            "nMel": report.pipeline.nMel,
            "predHidden": report.pipeline.predHidden,
            "predLayers": report.pipeline.predLayers,
            "status": "coreml-ready",
            "stages": report.stages,
            "validatedAt": ISO8601DateFormatter().string(from: report.validatedAt),
            "vocabSize": report.vocabSize,
            "windowSamples": report.pipeline.windowSamples,
            "windowSec": report.pipeline.windowSec,
        ]
        let data = try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
        try data.write(to: modelDirectory.appendingPathComponent(readyMarkerName), options: [.atomic])
    }

    private static func writeReadyMarker(_ report: CoreMLArtifactValidationReport, to modelDirectory: URL) throws {
        let payload: [String: Any] = [
            "backend": report.backend,
            "status": "coreml-ready",
            "stages": report.stages,
            "validatedAt": ISO8601DateFormatter().string(from: report.validatedAt),
        ]
        let data = try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
        try data.write(to: modelDirectory.appendingPathComponent(readyMarkerName), options: [.atomic])
    }
}

