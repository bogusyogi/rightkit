#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import CoreML
import Foundation

/// Result of validating a Parakeet model directory: every stage loaded, `pipeline.json` and `vocab.txt` parsed.
public struct CoreMLValidationReport: Sendable {
    public let stages: [String]
    public let pipeline: ParakeetPipelineConfig
    public let vocabSize: Int
    public let validatedAt: Date

    public init(stages: [String], pipeline: ParakeetPipelineConfig, vocabSize: Int, validatedAt: Date) {
        self.stages = stages
        self.pipeline = pipeline
        self.vocabSize = vocabSize
        self.validatedAt = validatedAt
    }
}

/// Result of validating a non-Parakeet Core ML artifact (for example Whisper): its required files exist
/// and its required stages load.
public struct CoreMLArtifactValidationReport: Sendable {
    public let backend: String
    public let stages: [String]
    public let validatedAt: Date

    public init(backend: String, stages: [String], validatedAt: Date) {
        self.backend = backend
        self.stages = stages
        self.validatedAt = validatedAt
    }
}

/// The parsed `pipeline.json` of a Parakeet model directory.
public struct ParakeetPipelineConfig: Sendable, Equatable {
    public let isTDT: Bool
    public let nMel: Int
    public let windowSec: Double
    public let melFrames: Int
    public let encFrames: Int
    public let encoderWidth: Int
    public let predLayers: Int
    public let predHidden: Int
    public let blankID: Int
    public let durations: [Int]

    public init(
        isTDT: Bool, nMel: Int, windowSec: Double, melFrames: Int, encFrames: Int, encoderWidth: Int,
        predLayers: Int, predHidden: Int, blankID: Int, durations: [Int]
    ) {
        self.isTDT = isTDT
        self.nMel = nMel
        self.windowSec = windowSec
        self.melFrames = melFrames
        self.encFrames = encFrames
        self.encoderWidth = encoderWidth
        self.predLayers = predLayers
        self.predHidden = predHidden
        self.blankID = blankID
        self.durations = durations
    }

    public var windowSamples: Int {
        Int(windowSec * 16_000)
    }

    /// Reads and validates `pipeline.json` from `modelDirectory`. Missing keys take the Parakeet TDT defaults.
    public static func load(from modelDirectory: URL) throws -> ParakeetPipelineConfig {
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

/// The parsed `vocab.txt` of a Parakeet model directory: one piece per line, id = line index.
public struct ParakeetVocabulary: Sendable, Equatable {
    public let pieces: [String]

    public init(pieces: [String]) {
        self.pieces = pieces
    }

    /// Reads `vocab.txt` and checks `blankID` lies inside it.
    public static func load(from modelDirectory: URL, blankID: Int) throws -> ParakeetVocabulary {
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

/// Validates an installed Core ML model directory and writes the `coreml-ready.json` marker.
///
/// Validation loads each stage the way production does: `.mlpackage` stages are compiled first, and every
/// stage loads with `.cpuAndNeuralEngine` (`.all` can route through the GPU and start an ANE compile that
/// never finalizes). Loading is blocking and slow on first use; call it off the main actor.
public struct CoreMLModelValidator: Sendable {
    /// File written into a model directory once every stage has loaded.
    public static let readyMarkerName = "coreml-ready.json"
    private static let stageNames = ["MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint"]

    public init() {}

    /// Validates a Parakeet directory (`pipeline.json`, `vocab.txt`, the four stages) and writes the ready marker.
    public func validateInstalledModel(at modelDirectory: URL) throws -> CoreMLValidationReport {
        try validateInstalledModel(at: modelDirectory, loadModel: Self.loadModel)
    }

    /// Validates any installed artifact: `requiredFiles` exist and `requiredStageNames` load. When the
    /// files include `pipeline.json` and `vocab.txt` it validates as a Parakeet model. Writes the ready marker.
    public func validateInstalledArtifact(
        backend: String,
        requiredFiles: [String],
        requiredStageNames: [String],
        at modelDirectory: URL
    ) throws -> CoreMLArtifactValidationReport {
        try validateInstalledArtifact(
            backend: backend, requiredFiles: requiredFiles, requiredStageNames: requiredStageNames,
            at: modelDirectory, loadModel: Self.loadModel)
    }

    /// Test seam: the stage loader is injectable.
    func validateInstalledModel(
        at modelDirectory: URL,
        loadModel: (URL) throws -> Void
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
        loadModel: (URL) throws -> Void
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

    /// True when the ready marker exists (a previous validation passed).
    public static func hasReadyMarker(at modelDirectory: URL) -> Bool {
        FileManager.default.fileExists(atPath: modelDirectory.appendingPathComponent(readyMarkerName).path)
    }

    /// The compiled `<stage>.mlmodelc` if present, else `<stage>.mlpackage`; throws `SpeechError.missingStage`.
    public static func stageURL(for stage: String, in modelDirectory: URL) throws -> URL {
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
#endif
