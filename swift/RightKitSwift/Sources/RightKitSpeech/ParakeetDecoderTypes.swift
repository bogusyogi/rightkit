import AVFoundation
import CoreML
import Foundation

struct DecoderJointStep {
    let stateH: [Float]
    let stateC: [Float]
    let logits: [Float]
}

/// TextDecoder output for one predictor state. It depends only on the last
/// emitted token and LSTM state, so blank descendants can reuse it while
/// Joint still runs for each changed encoder frame.
struct DecoderPredictorStep: Equatable {
    let embedding: [Float]
    let stateH: [Float]
    let stateC: [Float]
}

/// Mac parity: blank descendants retain predictor output; any emitted token
/// changes decoder input/state and invalidates the cache. Forced time advances
/// preserve hit count, so their cache remains valid too.
func parakeetCarryPredictorCache(
    enabled: Bool,
    parentHitCount: Int,
    childHitCount: Int,
    predictor: DecoderPredictorStep
) -> DecoderPredictorStep? {
    guard enabled, childHitCount == parentHitCount else { return nil }
    return predictor
}
