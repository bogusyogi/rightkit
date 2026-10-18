#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import CoreML
import Foundation

/// Asynchronous Core ML stage loading with the compute units the caller chooses.
///
/// `.mlpackage` stages are compiled first (`MLModel.compileModel(at:)`); `.mlmodelc` stages load directly.
/// The default compute units are `.cpuAndNeuralEngine`, the measured production route: `.all` lets Core ML
/// keep choosing the GPU and starts an ANE bundle compile on every load that is never finalized.
public enum CoreMLModelLoader {
    /// The URL Core ML can load: an `.mlpackage` compiles to a temporary `.mlmodelc`, anything else is returned as is.
    public static func loadableURL(for stageURL: URL) async throws -> URL {
        stageURL.pathExtension == "mlpackage" ? try await MLModel.compileModel(at: stageURL) : stageURL
    }

    /// Loads one stage (`.mlmodelc` or `.mlpackage`) without blocking the caller's thread.
    public static func load(
        stageAt stageURL: URL,
        computeUnits: MLComputeUnits = .cpuAndNeuralEngine
    ) async throws -> MLModel {
        let configuration = MLModelConfiguration()
        configuration.computeUnits = computeUnits
        return try await MLModel.load(contentsOf: loadableURL(for: stageURL), configuration: configuration)
    }

    /// Loads `<stage>.mlmodelc` or `<stage>.mlpackage` from an installed model directory.
    public static func load(
        stage: String,
        in modelDirectory: URL,
        computeUnits: MLComputeUnits = .cpuAndNeuralEngine
    ) async throws -> MLModel {
        try await load(
            stageAt: CoreMLModelValidator.stageURL(for: stage, in: modelDirectory),
            computeUnits: computeUnits)
    }

    /// The Core ML compute plan for one stage: which operations the planner assigns to the CPU, GPU and
    /// Neural Engine under `computeUnits`.
    @available(iOS 17.4, macOS 14.4, watchOS 10.4, *)
    public static func loadComputePlan(
        stageAt stageURL: URL,
        computeUnits: MLComputeUnits = .cpuAndNeuralEngine
    ) async throws -> MLComputePlan {
        let configuration = MLModelConfiguration()
        configuration.computeUnits = computeUnits
        return try await MLComputePlan.load(contentsOf: loadableURL(for: stageURL), configuration: configuration)
    }
}
#endif
