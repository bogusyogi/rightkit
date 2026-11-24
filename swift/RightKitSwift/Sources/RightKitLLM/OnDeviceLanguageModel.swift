import Foundation
#if canImport(FoundationModels) && (os(iOS) || os(macOS))
import FoundationModels
#endif

/// Why an on-device model call produced no text.
public enum OnDeviceModelError: Error, Equatable, Sendable {
    /// The OS has no usable system language model (older OS, Apple Intelligence off, model not ready).
    case unavailable
    /// The model answered with nothing but whitespace.
    case emptyOutput
}

/// Apple's on-device system language model (Foundation Models, iOS 26 and later) as a one-shot text
/// transformer. Text never leaves the device. It holds no prompt: the caller supplies the instructions
/// and the request text, and decides what an unavailable model means for its feature.
@available(watchOS, unavailable)
public enum OnDeviceLanguageModel {
    public enum Availability: Equatable, Sendable {
        case available
        /// The framework's own description of why the model cannot run.
        case unavailable(String)
    }

    /// Whether the system model can answer now. `reason` is the OS floor message when the framework is absent.
    public static func availability(unsupportedReason: String = "System language model unavailable on this OS") -> Availability {
        #if canImport(FoundationModels) && (os(iOS) || os(macOS))
        if #available(iOS 26.0, macOS 26.0, *) {
            if case .available = SystemLanguageModel.default.availability { return .available }
            return .unavailable(String(describing: SystemLanguageModel.default.availability))
        }
        #endif
        return .unavailable(unsupportedReason)
    }

    /// A conservative token estimate for context budgeting: characters over 4.5, rounded up, plus `overhead`
    /// for the instructions and framing.
    public static func estimatedContextTokens(_ text: String, overhead: Int = 240) -> Int {
        Int((Double(text.count) / 4.5).rounded(.up)) + overhead
    }

    /// One request in a fresh session. `postprocess` runs on the raw answer before it is trimmed and checked
    /// for emptiness (for example to restore newlines the caller encoded).
    public static func respond(
        instructions: String, prompt: String, postprocess: @Sendable (String) -> String = { $0 },
    ) async throws -> String {
        #if canImport(FoundationModels) && (os(iOS) || os(macOS))
        if #available(iOS 26.0, macOS 26.0, *) {
            guard case .available = SystemLanguageModel.default.availability else { throw OnDeviceModelError.unavailable }
            let session = LanguageModelSession(instructions: instructions)
            let response = try await session.respond(to: prompt)
            let output = postprocess(response.content).trimmingCharacters(in: .whitespacesAndNewlines)
            guard !output.isEmpty else { throw OnDeviceModelError.emptyOutput }
            return output
        }
        #endif
        throw OnDeviceModelError.unavailable
    }
}

/// Keeps line breaks through a model that would otherwise reflow them: replace each `\n` with a token
/// before the request and put the newlines back in the answer.
public struct NewlineSentinel: Sendable, Equatable {
    public let token: String
    public init(token: String) { self.token = token }
    public func encode(_ text: String) -> String { text.replacingOccurrences(of: "\n", with: token) }
    public func decode(_ text: String) -> String { text.replacingOccurrences(of: token, with: "\n") }
}
