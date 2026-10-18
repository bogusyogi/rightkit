import Foundation
import RightKitHTTP

// OpenAI-compatible chat-completions client: the Swift counterpart of `rightkit-llm`'s
// `OpenAiCompatProvider`. It owns the wire (request body, response parsing, truncation and
// empty-output handling), the provider presets and quirks, and ordered failover across routes.
// Prompts, output cleanup policy and UI stay with the app (pass `accept:` to apply your own).

/// Free-form JSON for provider-specific request fields.
public enum JSONValue: Sendable, Equatable, Codable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let v = try? c.decode(Bool.self) { self = .bool(v) }
        else if let v = try? c.decode(Double.self) { self = .number(v) }
        else if let v = try? c.decode(String.self) { self = .string(v) }
        else if let v = try? c.decode([JSONValue].self) { self = .array(v) }
        else { self = .object(try c.decode([String: JSONValue].self)) }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case let .bool(v): try c.encode(v)
        case let .number(v): try c.encode(v)
        case let .string(v): try c.encode(v)
        case let .array(v): try c.encode(v)
        case let .object(v): try c.encode(v)
        }
    }
}

public struct ChatMessage: Sendable, Equatable, Codable {
    public enum Role: String, Sendable, Equatable, Codable { case system, user, assistant }
    public var role: Role
    public var content: String

    public init(role: Role, content: String) {
        self.role = role
        self.content = content
    }

    public static func system(_ content: String) -> ChatMessage { .init(role: .system, content: content) }
    public static func user(_ content: String) -> ChatMessage { .init(role: .user, content: content) }
    public static func assistant(_ content: String) -> ChatMessage { .init(role: .assistant, content: content) }
}

/// A provider-specific adjustment applied when a route matches: extra body fields for models whose
/// name contains `modelContains`, optionally limited to one provider id.
public struct LLMQuirkRule: Sendable, Equatable {
    public var providerID: String?
    public var modelContains: String
    public var extraBody: [String: JSONValue]
    /// When set, the request's `max_tokens` is raised by this much, capped at `maxTokensCap`
    /// (reasoning models spend output tokens on hidden reasoning).
    public var extraOutputTokens: Int
    public var maxTokensCap: Int?

    public init(
        providerID: String? = nil, modelContains: String, extraBody: [String: JSONValue],
        extraOutputTokens: Int = 0, maxTokensCap: Int? = nil,
    ) {
        self.providerID = providerID
        self.modelContains = modelContains
        self.extraBody = extraBody
        self.extraOutputTokens = extraOutputTokens
        self.maxTokensCap = maxTokensCap
    }

    func matches(providerID id: String, model: String) -> Bool {
        (providerID == nil || providerID == id) && model.lowercased().contains(modelContains.lowercased())
    }

    /// The quirks the Right Suite's dictation polish relies on: Qwen models run with reasoning off;
    /// Groq-hosted gpt-oss runs with low reasoning effort and no reasoning text in the answer.
    public static let defaults: [LLMQuirkRule] = [
        LLMQuirkRule(modelContains: "qwen", extraBody: ["reasoning_effort": .string("none")]),
        LLMQuirkRule(
            providerID: "groq", modelContains: "gpt-oss",
            extraBody: ["reasoning_effort": .string("low"), "include_reasoning": .bool(false)],
            extraOutputTokens: 768, maxTokensCap: 2_048,
        ),
    ]
}

/// A provider endpoint. All but Anthropic's native API speak the OpenAI chat protocol.
public struct LLMProvider: Sendable, Equatable {
    public enum MaxTokensField: String, Sendable, Equatable { case maxTokens = "max_tokens", maxCompletionTokens = "max_completion_tokens" }

    public var id: String
    /// Base URL; `/chat/completions` is appended.
    public var baseURL: URL
    public var requiresCredential: Bool
    public var maxTokensField: MaxTokensField

    public init(id: String, baseURL: URL, requiresCredential: Bool = true, maxTokensField: MaxTokensField = .maxTokens) {
        self.id = id
        self.baseURL = baseURL
        self.requiresCredential = requiresCredential
        self.maxTokensField = maxTokensField
    }

    private static func make(_ id: String, _ url: String, key: Bool = true, field: MaxTokensField = .maxTokens) -> LLMProvider {
        LLMProvider(id: id, baseURL: URL(string: url)!, requiresCredential: key, maxTokensField: field)
    }

    // Endpoints match `rightkit-llm/src/presets.rs`.
    public static let openAI = make("openai", "https://api.openai.com/v1", field: .maxCompletionTokens)
    public static let openRouter = make("openrouter", "https://openrouter.ai/api/v1")
    public static let groq = make("groq", "https://api.groq.com/openai/v1")
    public static let deepSeek = make("deepseek", "https://api.deepseek.com/v1")
    public static let together = make("together", "https://api.together.xyz/v1")
    public static let mistral = make("mistral", "https://api.mistral.ai/v1")
    public static let xAI = make("xai", "https://api.x.ai/v1")
    public static let gemini = make("gemini", "https://generativelanguage.googleapis.com/v1beta/openai")
    public static let nvidia = make("nvidia", "https://integrate.api.nvidia.com/v1")
    public static let ollama = make("ollama", "http://127.0.0.1:11434/v1", key: false)
    public static let lmStudio = make("lmstudio", "http://127.0.0.1:1234/v1", key: false)

    public static let presets: [LLMProvider] = [
        openAI, openRouter, groq, deepSeek, together, mistral, xAI, gemini, nvidia, ollama, lmStudio,
    ]

    public static func preset(_ id: String) -> LLMProvider? { presets.first { $0.id == id } }

    var completionsURL: URL { baseURL.appendingPathComponent("chat/completions") }
}

/// A provider and the model to ask it. Routes are tried in order.
public struct LLMRoute: Sendable, Equatable {
    public var provider: LLMProvider
    public var model: String
    public init(provider: LLMProvider, model: String) {
        self.provider = provider
        self.model = model
    }
}

public struct ChatRequest: Sendable, Equatable {
    public var messages: [ChatMessage]
    public var temperature: Double?
    public var maxTokens: Int?
    /// Extra body fields. A key the client sets itself (`model`, `messages`, `stream`, ...) is
    /// ``LLMError/invalidRequest(_:)``.
    public var extraBody: [String: JSONValue]
    /// Treat `finish_reason == "length"` as ``LLMError/truncated`` instead of returning the cut text.
    public var rejectTruncatedOutput: Bool
    public var timeout: TimeInterval

    public init(
        messages: [ChatMessage], temperature: Double? = nil, maxTokens: Int? = nil,
        extraBody: [String: JSONValue] = [:], rejectTruncatedOutput: Bool = false, timeout: TimeInterval = 35,
    ) {
        self.messages = messages
        self.temperature = temperature
        self.maxTokens = maxTokens
        self.extraBody = extraBody
        self.rejectTruncatedOutput = rejectTruncatedOutput
        self.timeout = timeout
    }
}

public struct TokenUsage: Sendable, Equatable, Codable {
    public var promptTokens: Int?
    public var completionTokens: Int?
    public var totalTokens: Int?

    public init(promptTokens: Int? = nil, completionTokens: Int? = nil, totalTokens: Int? = nil) {
        self.promptTokens = promptTokens
        self.completionTokens = completionTokens
        self.totalTokens = totalTokens
    }

    enum CodingKeys: String, CodingKey {
        case promptTokens = "prompt_tokens"
        case completionTokens = "completion_tokens"
        case totalTokens = "total_tokens"
    }
}

public struct ChatCompletion: Sendable, Equatable {
    public var text: String
    public var finishReason: String?
    public var model: String
    public var provider: String
    public var usage: TokenUsage?

    public init(text: String, finishReason: String? = nil, model: String, provider: String, usage: TokenUsage? = nil) {
        self.text = text
        self.finishReason = finishReason
        self.model = model
        self.provider = provider
        self.usage = usage
    }
}

/// One failed route in an ``LLMError/allRoutesFailed(_:)``.
public struct LLMAttempt: Sendable, Equatable {
    public var provider: String
    public var model: String
    public var reason: String

    public init(provider: String, model: String, reason: String) {
        self.provider = provider
        self.model = model
        self.reason = reason
    }
}

public enum LLMError: Error, Equatable, Sendable {
    case missingCredential(provider: String)
    case http(HTTPError)
    /// The provider answered non-2xx. `message` is its own error text, bounded and without the key.
    case status(code: Int, message: String)
    /// 401 or 403: the key was rejected.
    case auth(code: Int, message: String)
    case malformed(String)
    case emptyOutput
    case truncated
    /// The caller's `accept` hook returned nil.
    case rejectedOutput
    case invalidRequest(String)
    case noRoutes
    case allRoutesFailed([LLMAttempt])
    case cancelled
}
