import CoreML
import Foundation
import RightKitSpeech  // deliberately not @testable: this file proves the surface is public
import Testing

struct CoreMLPublicSurfaceTests {
    private func modelDirectory(stageForm: String = "mlmodelc") throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("rightkit-coreml-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try #"{"is_tdt": true, "n_mel": 128, "blank_id": 2, "tdt_durations": [0, 1, 2]}"#
            .write(to: dir.appendingPathComponent("pipeline.json"), atomically: true, encoding: .utf8)
        try "<unk>\t0\nhello\t1\n<blank>\t2\n".write(to: dir.appendingPathComponent("vocab.txt"), atomically: true, encoding: .utf8)
        for stage in ["MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint"] {
            try FileManager.default.createDirectory(at: dir.appendingPathComponent("\(stage).\(stageForm)"), withIntermediateDirectories: true)
        }
        return dir
    }

    @Test func pipelineConfigAndVocabularyParseThroughThePublicAPI() throws {
        let dir = try modelDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let config = try ParakeetPipelineConfig.load(from: dir)
        #expect(config.isTDT && config.nMel == 128 && config.blankID == 2)
        #expect(config.durations == [0, 1, 2])
        #expect(config.windowSec == 15 && config.windowSamples == 240_000, "unset keys take the Parakeet TDT defaults")
        let vocabulary = try ParakeetVocabulary.load(from: dir, blankID: config.blankID)
        #expect(vocabulary.pieces == ["<unk>", "hello", "<blank>"])
        #expect(throws: SpeechError.self) { try ParakeetVocabulary.load(from: dir, blankID: 99) }
    }

    @Test func stageURLPrefersACompiledStageAndReportsAMissingOne() throws {
        let dir = try modelDirectory(stageForm: "mlpackage")
        defer { try? FileManager.default.removeItem(at: dir) }
        #expect(try CoreMLModelValidator.stageURL(for: "Joint", in: dir).pathExtension == "mlpackage")
        try FileManager.default.createDirectory(at: dir.appendingPathComponent("Joint.mlmodelc"), withIntermediateDirectories: true)
        #expect(try CoreMLModelValidator.stageURL(for: "Joint", in: dir).pathExtension == "mlmodelc")
        #expect(throws: SpeechError.missingStage("Nope")) { try CoreMLModelValidator.stageURL(for: "Nope", in: dir) }
    }

    @Test func aModelThatDoesNotLoadIsNotMarkedReady() throws {
        let dir = try modelDirectory()  // empty stage folders are not Core ML models
        defer { try? FileManager.default.removeItem(at: dir) }
        #expect(!CoreMLModelValidator.hasReadyMarker(at: dir))
        #expect(throws: (any Error).self) { try CoreMLModelValidator().validateInstalledModel(at: dir) }
        #expect(throws: (any Error).self) {
            try CoreMLModelValidator().validateInstalledArtifact(
                backend: "test", requiredFiles: ["pipeline.json"], requiredStageNames: ["Joint"], at: dir)
        }
        #expect(!CoreMLModelValidator.hasReadyMarker(at: dir))
        #expect(CoreMLModelValidator.readyMarkerName == "coreml-ready.json")
    }

    @Test func theAsyncLoaderRejectsAMissingStageAndABrokenOne() async throws {
        let dir = try modelDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        await #expect(throws: SpeechError.missingStage("Missing")) { _ = try await CoreMLModelLoader.load(stage: "Missing", in: dir) }
        await #expect(throws: (any Error).self) { _ = try await CoreMLModelLoader.load(stage: "Joint", in: dir, computeUnits: .cpuOnly) }
        let package = dir.appendingPathComponent("Broken.mlpackage")
        try FileManager.default.createDirectory(at: package, withIntermediateDirectories: true)
        await #expect(throws: (any Error).self) { _ = try await CoreMLModelLoader.load(stageAt: package) }
    }
}
