import Foundation
import Testing

@testable import RightKitSpeech

struct CoreMLValidatorTests {
    private func model() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("rightkit-validator-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try #"{"blank_id": 2}"#.write(to: dir.appendingPathComponent("pipeline.json"), atomically: true, encoding: .utf8)
        try "a\nb\nc\n".write(to: dir.appendingPathComponent("vocab.txt"), atomically: true, encoding: .utf8)
        for stage in ["MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint"] {
            try FileManager.default.createDirectory(at: dir.appendingPathComponent("\(stage).mlmodelc"), withIntermediateDirectories: true)
        }
        return dir
    }

    @Test func everyStageLoadsThenTheReadyMarkerIsWritten() throws {
        let dir = try model()
        defer { try? FileManager.default.removeItem(at: dir) }
        var loaded: [String] = []
        let report = try CoreMLModelValidator().validateInstalledModel(at: dir, loadModel: { loaded.append($0.lastPathComponent) })
        #expect(loaded == ["MelSpectrogram.mlmodelc", "AudioEncoder.mlmodelc", "TextDecoder.mlmodelc", "Joint.mlmodelc"])
        #expect(report.stages.count == 4 && report.vocabSize == 3)
        #expect(CoreMLModelValidator.hasReadyMarker(at: dir))
    }

    @Test func aStageThatFailsToLoadLeavesNoMarker() throws {
        let dir = try model()
        defer { try? FileManager.default.removeItem(at: dir) }
        struct Boom: Error {}
        #expect(throws: Boom.self) {
            try CoreMLModelValidator().validateInstalledModel(at: dir, loadModel: { if $0.lastPathComponent.hasPrefix("TextDecoder") { throw Boom() } })
        }
        #expect(!CoreMLModelValidator.hasReadyMarker(at: dir))
    }

    @Test func aNonParakeetArtifactChecksItsFilesAndStages() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("rightkit-whisper-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir.appendingPathComponent("AudioEncoder.mlmodelc"), withIntermediateDirectories: true)
        try "{}".write(to: dir.appendingPathComponent("config.json"), atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: dir) }
        #expect(throws: SpeechError.missingFile("tokenizer.json")) {
            try CoreMLModelValidator().validateInstalledArtifact(
                backend: "whisper", requiredFiles: ["config.json", "tokenizer.json"], requiredStageNames: ["AudioEncoder"],
                at: dir, loadModel: { _ in })
        }
        let report = try CoreMLModelValidator().validateInstalledArtifact(
            backend: "whisper", requiredFiles: ["config.json"], requiredStageNames: ["AudioEncoder"], at: dir, loadModel: { _ in })
        #expect(report.backend == "whisper" && report.stages == ["AudioEncoder"])
        #expect(CoreMLModelValidator.hasReadyMarker(at: dir))
    }
}
