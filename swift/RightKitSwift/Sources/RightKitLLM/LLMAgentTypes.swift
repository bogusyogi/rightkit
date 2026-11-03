import Foundation

// Tool-calling conversation types for ``LLMAgentClient``. They sit beside the plain text types in
// LLMTypes.swift (``ChatMessage``, ``ChatRequest``), which stay unchanged: a chat with no tools keeps using
// ``LLMClient``.

/// A function the model may call. `parameters` is a JSON Schema object (`{"type":"object","properties":{...}}`).
public struct ToolSpec: Sendable, Equatable {
    public var name: String
    public var description: String
    public var parameters: JSONValue

    public init(name: String, description: String, parameters: JSONValue = .object(["type": .string("object"), "properties": .object([:])])) {
        self.name = name
        self.description = description
        self.parameters = parameters
    }
}

/// One tool call the model asked for. `arguments` is the parsed JSON object the model produced.
public struct ToolCall: Sendable, Equatable {
    public var id: String
    public var name: String
    public var arguments: JSONValue

    public init(id: String, name: String, arguments: JSONValue = .object([:])) {
        self.id = id
        self.name = name
        self.arguments = arguments
    }
}

/// The answer to one ``ToolCall``, sent back to the model.
public struct ToolResult: Sendable, Equatable {
    public var callID: String
    public var content: String
    /// Tells the model the tool failed (`content` is then the error text).
    public var isError: Bool

    public init(callID: String, content: String, isError: Bool = false) {
        self.callID = callID
        self.content = content
        self.isError = isError
    }
}

/// One turn of an agent conversation.
public enum AgentMessage: Sendable, Equatable {
    case system(String)
    case user(String)
    /// What the model said, with the tool calls it made. Append ``AgentTurn/message`` after each model turn.
    case assistant(text: String, toolCalls: [ToolCall])
    case toolResult(ToolResult)
}

public enum ToolChoice: Sendable, Equatable {
    /// The model decides (default).
    case auto
    /// The model must call some tool.
    case required
    /// The model must not call a tool.
    case none
    /// The model must call this tool.
    case tool(String)
}

public struct AgentRequest: Sendable, Equatable {
    public var messages: [AgentMessage]
    public var tools: [ToolSpec]
    public var toolChoice: ToolChoice
    public var temperature: Double?
    /// Output ceiling. Required, because the budget reserves the worst case before the call.
    public var maxTokens: Int
    /// Extra body fields; a key the client sets itself is ``LLMError/invalidRequest(_:)``.
    public var extraBody: [String: JSONValue]
    /// Seconds without a byte before the call fails.
    public var timeout: TimeInterval

    public init(
        messages: [AgentMessage], tools: [ToolSpec] = [], toolChoice: ToolChoice = .auto, temperature: Double? = nil,
        maxTokens: Int = 4_096, extraBody: [String: JSONValue] = [:], timeout: TimeInterval = 120,
    ) {
        self.messages = messages
        self.tools = tools
        self.toolChoice = toolChoice
        self.temperature = temperature
        self.maxTokens = maxTokens
        self.extraBody = extraBody
        self.timeout = timeout
    }
}

public enum AgentFinishReason: Sendable, Equatable {
    case stop
    /// The model stopped to have its tool calls run.
    case toolCalls
    /// The output ceiling was reached.
    case length
    case other(String)
}

/// The finished model turn.
public struct AgentTurn: Sendable, Equatable {
    public var text: String
    public var toolCalls: [ToolCall]
    public var finishReason: AgentFinishReason
    public var model: String
    public var provider: String
    public var usage: TokenUsage?
    /// What the budget charged for this call, in micro-units of the budget's currency.
    public var costMicro: Int64
    /// True when the provider reported no usage, so the call was charged at its reserved ceiling.
    public var billedAtCeiling: Bool

    /// The assistant message to append to the history before sending the tool results.
    public var message: AgentMessage { .assistant(text: text, toolCalls: toolCalls) }

    public init(
        text: String, toolCalls: [ToolCall] = [], finishReason: AgentFinishReason = .stop, model: String, provider: String,
        usage: TokenUsage? = nil, costMicro: Int64 = 0, billedAtCeiling: Bool = false,
    ) {
        self.text = text
        self.toolCalls = toolCalls
        self.finishReason = finishReason
        self.model = model
        self.provider = provider
        self.usage = usage
        self.costMicro = costMicro
        self.billedAtCeiling = billedAtCeiling
    }
}

/// What ``LLMAgentClient/stream(_:routes:)`` yields. The last event is always ``completed(_:)``.
public enum AgentEvent: Sendable, Equatable {
    case textDelta(String)
    case toolCallStarted(id: String, name: String)
    /// A fragment of the JSON arguments text of the call `id`. The parsed arguments arrive in ``completed(_:)``.
    case toolCallArgumentsDelta(id: String, fragment: String)
    case completed(AgentTurn)
}

/// When the client may move to the next route after one fails. Failing over to a paid provider
/// you did not name is the default's refusal, never a silent step.
public enum LLMFallbackPolicy: Sendable {
    /// Use the first route only; its failure is the result (default).
    case none
    /// A later route is tried only when it costs nothing: its provider needs no key (a local model) or its
    /// price is ``LLMPrice/free``.
    case freeOnly
    /// A later paid route is tried only when the closure returns true for that exact hop (ask the user there).
    /// Free routes pass without a question.
    case confirmed(@Sendable (_ failed: LLMRoute, _ next: LLMRoute, _ reason: LLMError) async -> Bool)
}
