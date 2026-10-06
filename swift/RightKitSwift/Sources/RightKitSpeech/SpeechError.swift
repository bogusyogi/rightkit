import Foundation

/// Every failure RightKitSpeech reports. One type, so callers `catch` a single error and switch on
/// the case; each case carries enough context to be actionable without a stack trace.
public enum SpeechError: Error, LocalizedError, Equatable, Sendable {
    // Parakeet / Core ML runtime
    case invalidArrayShape
    case invalidContract(String)
    case modelNotReady
    case unsupportedArrayType
    case unsupportedAudioFormat
    // Model artifact validation
    case invalidPipeline(String)
    case invalidVocabulary(String)
    case missingFile(String)
    case missingStage(String)
    // Whisper
    case invalidEncodedOutput
    case invalidLogitShape
    case invalidMelShape(Int)
    case invalidFilterbank(Int)
    case missingFilterbank
    case invalidTokenizerJSON
    case unknownToken(Int)
    // Engine selection / platform speech
    case engineUnavailable(String)
    case noAudioTrack
    case decodeFailed(String)
    case permissionDenied(String)

    public var errorDescription: String? {
        switch self {
        case .invalidArrayShape: return "Invalid Core ML array shape."
        case let .invalidContract(reason): return "Invalid Parakeet Core ML contract: \(reason)"
        case .modelNotReady: return "The Core ML speech model is not installed and validated."
        case .unsupportedArrayType: return "Unsupported Core ML array type."
        case .unsupportedAudioFormat: return "Speech models expect 16 kHz mono float audio."
        case let .invalidPipeline(reason): return "Invalid pipeline.json: \(reason)"
        case let .invalidVocabulary(reason): return "Invalid vocab.txt: \(reason)"
        case let .missingFile(name): return "Model is missing \(name)."
        case let .missingStage(name): return "Model is missing \(name)."
        case .invalidEncodedOutput: return "Invalid Whisper encoder output."
        case .invalidLogitShape: return "Invalid Whisper decoder logits."
        case let .invalidMelShape(count): return "Whisper mel input must contain 128 * 3000 values, got \(count)."
        case let .invalidFilterbank(count):
            return "Whisper mel filterbank has \(count) values, expected \(WhisperMelSpectrogram.nMels * WhisperMelSpectrogram.nFreq)."
        case .missingFilterbank: return "Whisper mel filterbank resource is missing."
        case .invalidTokenizerJSON: return "Whisper tokenizer.json is invalid."
        case let .unknownToken(id): return "Whisper token \(id) is not in tokenizer.json."
        case let .engineUnavailable(reason): return "No speech engine is available: \(reason)"
        case .noAudioTrack: return "This file has no audio track."
        case let .decodeFailed(reason): return "Could not decode the audio: \(reason)"
        case let .permissionDenied(what): return "\(what) permission is required."
        }
    }
}
