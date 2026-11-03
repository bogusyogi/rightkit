import Foundation
import RightKitHTTP

// Streaming, tool-calling model client for app agents (rightkit#891). One value speaks two wire dialects:
// the OpenAI chat protocol (every preset in ``LLMProvider/presets``) and Anthropic Messages
// (``LLMProvider/anthropic``). Keys come from a ``LLMCredentialSource`` (the Keychain through
// ``SecretsStoreCredentials``), every call is bounded by a durable ``LLMBudget`` that refuses before the call
// that would pass the cap, and a failure never moves to another paid provider unless the app's
// ``LLMFallbackPolicy`` says so for that exact hop.

/// The result of ``LLMAgentClient/run(_:routes:maxSteps:execute:)``.
public struct AgentRun: Sendable, Equatable {
    /// Every model turn, in order. The last one has no tool calls.
    public var turns: [AgentTurn]
    /// The request's messages followed by every turn and tool result.
    public var messages: [AgentMessage]
    public var finalText: String { turns.last?.text ?? "" }
}

public struct LLMAgentClient: Sendable {
    public let http: HTTPClient
    public let credentials: any LLMCredentialSource
    public let userAgent: String
    public let quirks: [LLMQuirkRule]
    public let budget: LLMBudget
    public let prices: LLMPriceBook
    public let fallback: LLMFallbackPolicy
    /// A response stream larger than this is cut and fails (default 16 MiB).
    public let maxStreamBytes: Int

    /// - Parameters:
    ///   - budget: the app's durable cap. Required: every call reserves its worst case first.
    ///   - prices: what each route costs. A route with no price is refused (``LLMError/unpriced(provider:model:)``).
    ///   - fallback: default ``LLMFallbackPolicy/none``.
    public init(
        credentials: any LLMCredentialSource, userAgent: String, budget: LLMBudget, prices: LLMPriceBook,
        fallback: LLMFallbackPolicy = .none, http: HTTPClient = HTTPClient(), quirks: [LLMQuirkRule] = LLMQuirkRule.defaults,
        maxStreamBytes: Int = 16 * 1024 * 1024,
    ) {
        self.credentials = credentials
        self.userAgent = userAgent
        self.budget = budget
        self.prices = prices
        self.fallback = fallback
        self.http = http
        self.quirks = quirks
        self.maxStreamBytes = maxStreamBytes
    }

    // MARK: Public calls

    /// Streams one model turn. The last event is ``AgentEvent/completed(_:)``; cancel the consuming task to stop
    /// the call (the budget then charges what the provider reported, or the reserved ceiling when it reported nothing).
    public func stream(_ request: AgentRequest, routes: [LLMRoute]) -> AsyncThrowingStream<AgentEvent, Error> {
        AsyncThrowingStream<AgentEvent, Error> { continuation in
            let task = Task {
                do {
                    try await self.drive(request, routes: routes) { _ = continuation.yield($0) }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// One model turn, collected.
    public func respond(_ request: AgentRequest, routes: [LLMRoute]) async throws -> AgentTurn {
        var finished: AgentTurn?
        for try await event in stream(request, routes: routes) {
            if case let .completed(turn) = event { finished = turn }
        }
        guard let finished else { throw LLMError.malformed("stream ended without a turn") }
        return finished
    }

    /// The tool loop: asks the model, runs the tool calls it makes through `execute`, sends the results back, and
    /// repeats until the model answers without a tool call or `maxSteps` model calls were used
    /// (``LLMError/toolLoopLimit(steps:)``). Every model call is budgeted like any other.
    public func run(
        _ request: AgentRequest, routes: [LLMRoute], maxSteps: Int = 8,
        execute: @Sendable (ToolCall) async -> ToolResult,
    ) async throws -> AgentRun {
        guard maxSteps > 0 else { throw LLMError.invalidRequest("maxSteps must be positive") }
        var current = request
        var turns: [AgentTurn] = []
        for _ in 0..<maxSteps {
            let turn = try await respond(current, routes: routes)
            turns.append(turn)
            current.messages.append(turn.message)
            if turn.toolCalls.isEmpty { return AgentRun(turns: turns, messages: current.messages) }
            for call in turn.toolCalls {
                if Task.isCancelled { throw LLMError.cancelled }
                current.messages.append(.toolResult(await execute(call)))
            }
        }
        throw LLMError.toolLoopLimit(steps: maxSteps)
    }

    // MARK: Routing

    private func drive(_ request: AgentRequest, routes: [LLMRoute], emit: @Sendable (AgentEvent) -> Void) async throws {
        guard !routes.isEmpty else { throw LLMError.noRoutes }
        var attempts: [LLMAttempt] = []
        var failure: LLMError = .noRoutes
        var failedRoute = routes[0]
        for (index, route) in routes.enumerated() {
            if index > 0, !(await mayFallBack(from: failedRoute, to: route, reason: failure)) { continue }
            var emitted = false
            do {
                try await attempt(request, route: route, emitted: &emitted, emit: emit)
                return
            } catch let error as LLMError {
                // Output already reached the caller, or the request itself is wrong: another route cannot fix that.
                if emitted { throw error }
                switch error {
                case .invalidRequest, .cancelled: throw error
                default: break
                }
                attempts.append(LLMAttempt(provider: route.provider.id, model: route.model, reason: Self.describe(error)))
                failure = error
                failedRoute = route
                if case .none = fallback { throw error }
            }
        }
        if attempts.count == 1 { throw failure }
        throw LLMError.allRoutesFailed(attempts)
    }

    private func isFree(_ route: LLMRoute) -> Bool {
        !route.provider.requiresCredential || prices.price(for: route) == LLMPrice.free
    }

    private func mayFallBack(from failed: LLMRoute, to next: LLMRoute, reason: LLMError) async -> Bool {
        switch fallback {
        case .none: return false
        case .freeOnly: return isFree(next)
        case let .confirmed(ask):
            if isFree(next) { return true }
            return await ask(failed, next, reason)
        }
    }

    // MARK: One call

    private func attempt(
        _ request: AgentRequest, route: LLMRoute, emitted: inout Bool, emit: @Sendable (AgentEvent) -> Void,
    ) async throws {
        let provider = route.provider
        guard request.maxTokens > 0 else { throw LLMError.invalidRequest("maxTokens must be positive") }
        guard let price = prices.price(for: route) else { throw LLMError.unpriced(provider: provider.id, model: route.model) }
        var key: String?
        if provider.requiresCredential {
            do { key = try credentials.credential(provider: provider.id) } catch { throw LLMError.missingCredential(provider: provider.id) }
            guard let found = key, !found.isEmpty else { throw LLMError.missingCredential(provider: provider.id) }
        }
        let body = try makeBody(request, route: route)
        let url = Self.endpoint(provider)
        try await validate(url)

        // Worst case: every body byte is one input token (a token is never shorter), and the whole output ceiling is used.
        guard let ceiling = price.cost(inputTokens: body.count, outputTokens: request.maxTokens) else {
            throw LLMError.invalidRequest("cost bound overflows")
        }
        var urlRequest = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: request.timeout)
        urlRequest.httpMethod = "POST"
        urlRequest.httpBody = body
        urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        urlRequest.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        urlRequest.setValue(userAgent, forHTTPHeaderField: "User-Agent")
        if let key {
            switch provider.wire {
            case .openAICompatible: urlRequest.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization")
            case .anthropic:
                urlRequest.setValue(key, forHTTPHeaderField: "x-api-key")
                urlRequest.setValue(Self.anthropicVersion, forHTTPHeaderField: "anthropic-version")
            }
        } else if provider.wire == .anthropic {
            urlRequest.setValue(Self.anthropicVersion, forHTTPHeaderField: "anthropic-version")
        }

        let hold = try await budget.reserve(estimateMicro: ceiling)
        do { try await budget.markInFlight(hold) } catch {
            await budget.release(hold)
            throw error
        }

        let head: HTTPURLResponse
        let chunks: AsyncThrowingStream<Data, Error>
        do {
            let streamed = try await http.transport.stream(urlRequest)
            head = streamed.0
            chunks = streamed.1
        } catch {
            let mapped = Self.map(error)
            // A timeout may mean the provider is still working on it, so it is charged at the ceiling; a refused
            // connection or a cancel before any answer sent nothing billable.
            if case .http(.timeout) = mapped { await budget.commit(hold, actualMicro: ceiling, unknown: true) } else { await budget.release(hold) }
            throw mapped
        }

        guard (200..<300).contains(head.statusCode) else {
            var errorBody = Data()
            do {
                for try await chunk in chunks {
                    errorBody.append(chunk)
                    if errorBody.count > 65_536 { break }
                }
            } catch {}
            // A definite non-2xx answer was not billed.
            await budget.release(hold)
            let message = HTTPClient.message(from: errorBody, redacting: key)
            throw (head.statusCode == 401 || head.statusCode == 403)
                ? LLMError.auth(code: head.statusCode, message: message)
                : LLMError.status(code: head.statusCode, message: message)
        }

        var decoder: any AgentStreamDecoder
        switch provider.wire {
        case .openAICompatible: decoder = OpenAIStreamDecoder(provider: provider.id, requested: route.model)
        case .anthropic: decoder = AnthropicStreamDecoder(provider: provider.id, requested: route.model)
        }
        var parser = SSEParser()
        var received = 0
        var turn: AgentTurn
        do {
            for try await chunk in chunks {
                received += chunk.count
                guard received <= maxStreamBytes else { throw LLMError.http(.tooLarge(limit: maxStreamBytes)) }
                for event in parser.feed(chunk) {
                    for out in try decoder.consume(event) {
                        emitted = true
                        emit(out)
                    }
                }
            }
            for event in parser.flush() {
                for out in try decoder.consume(event) {
                    emitted = true
                    emit(out)
                }
            }
            if Task.isCancelled { throw LLMError.cancelled }
            turn = try decoder.finish()
        } catch {
            // The provider answered, so the call may be billed: charge what it reported, else the ceiling.
            let spent = Self.cost(of: decoder.usage, price: price)
            await budget.commit(hold, actualMicro: spent ?? ceiling, unknown: spent == nil)
            throw Self.map(error)
        }
        let spent = Self.cost(of: turn.usage, price: price)
        await budget.commit(hold, actualMicro: spent ?? ceiling, unknown: spent == nil)
        turn.costMicro = spent ?? ceiling
        turn.billedAtCeiling = spent == nil
        emitted = true
        emit(.completed(turn))
    }

    private static func cost(of usage: TokenUsage?, price: LLMPrice) -> Int64? {
        guard let usage, let input = usage.promptTokens, let output = usage.completionTokens else { return nil }
        return price.cost(inputTokens: input, outputTokens: output)
    }

    private func validate(_ url: URL) async throws {
        do {
            if let policy = http.policy {
                try await policy.vet(url)
            } else if !HTTPClient.isPermitted(url, allowInsecureLoopback: http.allowInsecureLoopback) {
                throw HTTPError.insecureURL(url.host ?? "")
            }
        } catch let error as HTTPError {
            throw LLMError.http(error)
        }
    }

    static let anthropicVersion = "2023-06-01"

    static func endpoint(_ provider: LLMProvider) -> URL {
        switch provider.wire {
        case .openAICompatible: return provider.baseURL.appendingPathComponent("chat/completions")
        case .anthropic: return provider.baseURL.appendingPathComponent("messages")
        }
    }

    static func map(_ error: Error) -> LLMError {
        if let error = error as? LLMError { return error }
        if error is CancellationError { return .cancelled }
        if let error = error as? HTTPError { return error == .cancelled ? .cancelled : .http(error) }
        if let error = error as? URLError {
            switch error.code {
            case .cancelled: return .cancelled
            case .timedOut: return .http(.timeout)
            default: return .http(.transport("URLError \(error.code.rawValue)"))
            }
        }
        return .http(.transport(String(describing: Swift.type(of: error))))
    }

    private static func describe(_ error: LLMError) -> String {
        switch error {
        case let .status(code, message), let .auth(code, message): return "HTTP \(code): \(message)"
        case let .http(inner): return "transport: \(inner)"
        case let .missingCredential(provider): return "no credential for \(provider)"
        case let .malformed(why): return "malformed: \(why)"
        case .emptyOutput: return "empty output"
        case .budgetExceeded: return "budget would be exceeded"
        case let .unpriced(provider, model): return "no price for \(provider) \(model)"
        default: return String(describing: error)
        }
    }

    // MARK: Request bodies

    func makeBody(_ request: AgentRequest, route: LLMRoute) throws -> Data {
        guard !request.messages.isEmpty else { throw LLMError.invalidRequest("messages is empty") }
        var seen = Set<String>()
        for tool in request.tools {
            guard !tool.name.isEmpty, seen.insert(tool.name).inserted else { throw LLMError.invalidRequest("tool names must be unique and non-empty") }
        }
        if case let .tool(name) = request.toolChoice, !seen.contains(name) {
            throw LLMError.invalidRequest("toolChoice names a tool that is not in tools")
        }
        let fields: [String: JSONValue]
        switch route.provider.wire {
        case .openAICompatible: fields = try openAIFields(request, route: route)
        case .anthropic: fields = try anthropicFields(request, route: route)
        }
        do { return try JSONEncoder().encode(fields) } catch { throw LLMError.invalidRequest("request is not encodable") }
    }

    private static let reservedOpenAI: Set<String> = [
        "model", "messages", "stream", "stream_options", "tools", "tool_choice", "temperature", "max_tokens", "max_completion_tokens",
    ]
    private static let reservedAnthropic: Set<String> = [
        "model", "messages", "system", "stream", "tools", "tool_choice", "temperature", "max_tokens",
    ]

    private static func argumentsText(_ arguments: JSONValue) throws -> String {
        guard let data = try? JSONEncoder().encode(arguments) else { throw LLMError.invalidRequest("tool arguments are not encodable") }
        return String(decoding: data, as: UTF8.self)
    }

    private func openAIFields(_ request: AgentRequest, route: LLMRoute) throws -> [String: JSONValue] {
        for key in request.extraBody.keys where Self.reservedOpenAI.contains(key) {
            throw LLMError.invalidRequest("extraBody sets reserved field \(key)")
        }
        var messages: [JSONValue] = []
        for message in request.messages {
            switch message {
            case let .system(text): messages.append(.object(["role": .string("system"), "content": .string(text)]))
            case let .user(text): messages.append(.object(["role": .string("user"), "content": .string(text)]))
            case let .assistant(text, calls):
                var object: [String: JSONValue] = ["role": .string("assistant"), "content": text.isEmpty && !calls.isEmpty ? .null : .string(text)]
                if !calls.isEmpty {
                    object["tool_calls"] = .array(try calls.map {
                        .object([
                            "id": .string($0.id), "type": .string("function"),
                            "function": .object(["name": .string($0.name), "arguments": .string(try Self.argumentsText($0.arguments))]),
                        ])
                    })
                }
                messages.append(.object(object))
            case let .toolResult(result):
                messages.append(.object(["role": .string("tool"), "tool_call_id": .string(result.callID), "content": .string(result.content)]))
            }
        }
        var fields: [String: JSONValue] = [
            "model": .string(route.model), "messages": .array(messages), "stream": .bool(true),
            "stream_options": .object(["include_usage": .bool(true)]),
        ]
        if let temperature = request.temperature { fields["temperature"] = .number(temperature) }
        if !request.tools.isEmpty {
            fields["tools"] = .array(request.tools.map {
                .object(["type": .string("function"), "function": .object([
                    "name": .string($0.name), "description": .string($0.description), "parameters": $0.parameters,
                ])])
            })
            switch request.toolChoice {
            case .auto: break
            case .required: fields["tool_choice"] = .string("required")
            case .none: fields["tool_choice"] = .string("none")
            case let .tool(name):
                fields["tool_choice"] = .object(["type": .string("function"), "function": .object(["name": .string(name)])])
            }
        }
        var maxTokens = request.maxTokens
        var extras: [String: JSONValue] = [:]
        for rule in quirks where rule.matches(providerID: route.provider.id, model: route.model) {
            for (k, v) in rule.extraBody { extras[k] = v }
            if rule.extraOutputTokens > 0 {
                let raised = maxTokens + rule.extraOutputTokens
                maxTokens = rule.maxTokensCap.map { min($0, raised) } ?? raised
            }
        }
        for (k, v) in request.extraBody { extras[k] = v }
        fields[route.provider.maxTokensField.rawValue] = .number(Double(maxTokens))
        for (k, v) in extras { fields[k] = v }
        return fields
    }

    private func anthropicFields(_ request: AgentRequest, route: LLMRoute) throws -> [String: JSONValue] {
        for key in request.extraBody.keys where Self.reservedAnthropic.contains(key) {
            throw LLMError.invalidRequest("extraBody sets reserved field \(key)")
        }
        var system: [String] = []
        // Adjacent turns of one role are one message; a tool result precedes any user text that follows it.
        var turns: [(role: String, blocks: [JSONValue])] = []
        func append(_ role: String, _ blocks: [JSONValue]) {
            if let last = turns.last, last.role == role {
                turns[turns.count - 1].blocks += blocks
            } else {
                turns.append((role, blocks))
            }
        }
        for message in request.messages {
            switch message {
            case let .system(text): system.append(text)
            case let .user(text): append("user", [.object(["type": .string("text"), "text": .string(text)])])
            case let .assistant(text, calls):
                var blocks: [JSONValue] = []
                if !text.isEmpty { blocks.append(.object(["type": .string("text"), "text": .string(text)])) }
                for call in calls {
                    blocks.append(.object(["type": .string("tool_use"), "id": .string(call.id), "name": .string(call.name), "input": call.arguments]))
                }
                if blocks.isEmpty { blocks.append(.object(["type": .string("text"), "text": .string("(no output)")])) }
                append("assistant", blocks)
            case let .toolResult(result):
                var block: [String: JSONValue] = ["type": .string("tool_result"), "tool_use_id": .string(result.callID), "content": .string(result.content)]
                if result.isError { block["is_error"] = .bool(true) }
                append("user", [.object(block)])
            }
        }
        guard !turns.isEmpty else { throw LLMError.invalidRequest("messages has no user or assistant turn") }
        var fields: [String: JSONValue] = [
            "model": .string(route.model), "max_tokens": .number(Double(request.maxTokens)), "stream": .bool(true),
            "messages": .array(turns.map { .object(["role": .string($0.role), "content": .array($0.blocks)]) }),
        ]
        if !system.isEmpty { fields["system"] = .string(system.joined(separator: "\n\n")) }
        if let temperature = request.temperature { fields["temperature"] = .number(temperature) }
        if !request.tools.isEmpty {
            fields["tools"] = .array(request.tools.map {
                .object(["name": .string($0.name), "description": .string($0.description), "input_schema": $0.parameters])
            })
            switch request.toolChoice {
            case .auto: fields["tool_choice"] = .object(["type": .string("auto")])
            case .required: fields["tool_choice"] = .object(["type": .string("any")])
            case .none: fields["tool_choice"] = .object(["type": .string("none")])
            case let .tool(name): fields["tool_choice"] = .object(["type": .string("tool"), "name": .string(name)])
            }
        }
        for (k, v) in request.extraBody { fields[k] = v }
        return fields
    }
}

// MARK: - Server-sent events

struct SSEEvent: Equatable {
    var event: String?
    var data: String
}

/// Incremental `text/event-stream` parser: bytes in, whole events out, whatever the chunk boundaries.
struct SSEParser {
    private var buffer = Data()
    private var eventName: String?
    private var dataLines: [String] = []

    mutating func feed(_ chunk: Data) -> [SSEEvent] {
        buffer.append(chunk)
        var events: [SSEEvent] = []
        while let newline = buffer.firstIndex(of: 0x0A) {
            var line = Data(buffer[buffer.startIndex..<newline])
            buffer.removeSubrange(buffer.startIndex...newline)
            if line.last == 0x0D { line.removeLast() }
            if let event = process(String(decoding: line, as: UTF8.self)) { events.append(event) }
        }
        return events
    }

    /// The stream ended: a last line with no newline, and an event with no blank line after it, still count.
    mutating func flush() -> [SSEEvent] {
        var events: [SSEEvent] = []
        if !buffer.isEmpty {
            var line = buffer
            buffer = Data()
            if line.last == 0x0D { line.removeLast() }
            if let event = process(String(decoding: line, as: UTF8.self)) { events.append(event) }
        }
        if let event = process("") { events.append(event) }
        return events
    }

    private mutating func process(_ line: String) -> SSEEvent? {
        if line.isEmpty {
            defer { eventName = nil; dataLines = [] }
            guard !dataLines.isEmpty else { return nil }
            return SSEEvent(event: eventName, data: dataLines.joined(separator: "\n"))
        }
        if line.hasPrefix(":") { return nil }
        let name: String
        var value: String
        if let colon = line.firstIndex(of: ":") {
            name = String(line[line.startIndex..<colon])
            value = String(line[line.index(after: colon)...])
            if value.hasPrefix(" ") { value.removeFirst() }
        } else {
            name = line
            value = ""
        }
        switch name {
        case "event": eventName = value
        case "data": dataLines.append(value)
        default: break
        }
        return nil
    }
}

// MARK: - Stream decoders

protocol AgentStreamDecoder: Sendable {
    mutating func consume(_ event: SSEEvent) throws -> [AgentEvent]
    /// The turn, once the stream ended cleanly; throws when it ended before the model finished.
    func finish() throws -> AgentTurn
    /// What the provider reported so far.
    var usage: TokenUsage? { get }
}

private func parseArguments(_ text: String) throws -> JSONValue {
    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    if trimmed.isEmpty { return .object([:]) }
    guard let value = try? JSONDecoder().decode(JSONValue.self, from: Data(trimmed.utf8)), case .object = value else {
        throw LLMError.malformed("tool call arguments are not a JSON object")
    }
    return value
}

private func parseObject(_ data: String) throws -> [String: Any] {
    guard let object = (try? JSONSerialization.jsonObject(with: Data(data.utf8))) as? [String: Any] else {
        throw LLMError.malformed("stream event is not a JSON object")
    }
    return object
}

struct OpenAIStreamDecoder: AgentStreamDecoder {
    private struct Partial {
        var id: String
        var name: String
        var arguments: String
        var announced: Bool
    }

    let provider: String
    let requested: String
    private var text = ""
    private var partials: [Int: Partial] = [:]
    private var order: [Int] = []
    private var finishReason: String?
    private var model: String?
    private var done = false
    private(set) var usage: TokenUsage?

    init(provider: String, requested: String) {
        self.provider = provider
        self.requested = requested
    }

    mutating func consume(_ event: SSEEvent) throws -> [AgentEvent] {
        if event.data == "[DONE]" {
            done = true
            return []
        }
        let object = try parseObject(event.data)
        if let error = object["error"] as? [String: Any] {
            throw LLMError.status(code: error["code"] as? Int ?? 500, message: String((error["message"] as? String ?? "provider error").prefix(512)))
        }
        if let id = object["model"] as? String { model = id }
        if let raw = object["usage"] as? [String: Any] {
            let prompt = raw["prompt_tokens"] as? Int
            let completion = raw["completion_tokens"] as? Int
            usage = TokenUsage(promptTokens: prompt, completionTokens: completion, totalTokens: raw["total_tokens"] as? Int)
        }
        guard let choice = (object["choices"] as? [[String: Any]])?.first else { return [] }
        var out: [AgentEvent] = []
        if let delta = choice["delta"] as? [String: Any] {
            if let content = delta["content"] as? String, !content.isEmpty {
                text += content
                out.append(.textDelta(content))
            }
            for call in delta["tool_calls"] as? [[String: Any]] ?? [] {
                let index = call["index"] as? Int ?? 0
                if partials[index] == nil {
                    partials[index] = Partial(id: "", name: "", arguments: "", announced: false)
                    order.append(index)
                }
                var partial = partials[index]!
                let function = call["function"] as? [String: Any]
                if let id = call["id"] as? String, !id.isEmpty { partial.id = id }
                if let name = function?["name"] as? String, !name.isEmpty { partial.name += name }
                if !partial.announced, !partial.name.isEmpty {
                    if partial.id.isEmpty { partial.id = "call_\(index)" }
                    partial.announced = true
                    out.append(.toolCallStarted(id: partial.id, name: partial.name))
                }
                if let fragment = function?["arguments"] as? String, !fragment.isEmpty {
                    partial.arguments += fragment
                    if partial.announced { out.append(.toolCallArgumentsDelta(id: partial.id, fragment: fragment)) }
                }
                partials[index] = partial
            }
        }
        if let reason = choice["finish_reason"] as? String { finishReason = reason }
        return out
    }

    func finish() throws -> AgentTurn {
        guard done || finishReason != nil else { throw LLMError.malformed("stream ended before the model finished") }
        var calls: [ToolCall] = []
        for index in order {
            guard let partial = partials[index], !partial.name.isEmpty else { throw LLMError.malformed("tool call has no name") }
            calls.append(ToolCall(id: partial.id.isEmpty ? "call_\(index)" : partial.id, name: partial.name, arguments: try parseArguments(partial.arguments)))
        }
        let reason: AgentFinishReason
        switch finishReason {
        case "stop": reason = .stop
        case "tool_calls", "function_call": reason = .toolCalls
        case "length": reason = .length
        case let other?: reason = .other(other)
        case nil: reason = calls.isEmpty ? .stop : .toolCalls
        }
        if calls.isEmpty, text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, reason == .stop { throw LLMError.emptyOutput }
        return AgentTurn(text: text, toolCalls: calls, finishReason: reason, model: model ?? requested, provider: provider, usage: usage)
    }
}

struct AnthropicStreamDecoder: AgentStreamDecoder {
    private struct Block {
        var id: String
        var name: String
        var json: String
    }

    let provider: String
    let requested: String
    private var text = ""
    private var tools: [Int: Block] = [:]
    private var order: [Int] = []
    private var stopReason: String?
    private var model: String?
    private var inputTokens: Int?
    private var outputTokens: Int?
    private var done = false

    init(provider: String, requested: String) {
        self.provider = provider
        self.requested = requested
    }

    /// Cache reads and writes are counted at the input rate (an upper bound for reads), so the charge never undercounts.
    var usage: TokenUsage? {
        guard inputTokens != nil || outputTokens != nil else { return nil }
        let total = (inputTokens != nil && outputTokens != nil) ? inputTokens! + outputTokens! : nil
        return TokenUsage(promptTokens: inputTokens, completionTokens: outputTokens, totalTokens: total)
    }

    mutating func consume(_ event: SSEEvent) throws -> [AgentEvent] {
        let object = try parseObject(event.data)
        let type = object["type"] as? String ?? event.event ?? ""
        var out: [AgentEvent] = []
        switch type {
        case "message_start":
            let message = object["message"] as? [String: Any]
            if let id = message?["model"] as? String { model = id }
            if let raw = message?["usage"] as? [String: Any] {
                let input = (raw["input_tokens"] as? Int ?? 0) + (raw["cache_creation_input_tokens"] as? Int ?? 0) + (raw["cache_read_input_tokens"] as? Int ?? 0)
                inputTokens = input
                // message_start carries a placeholder output count; only message_delta's is final.
            }
        case "content_block_start":
            let index = object["index"] as? Int ?? 0
            if let block = object["content_block"] as? [String: Any], block["type"] as? String == "tool_use" {
                let id = block["id"] as? String ?? "call_\(index)"
                let name = block["name"] as? String ?? ""
                tools[index] = Block(id: id, name: name, json: "")
                order.append(index)
                out.append(.toolCallStarted(id: id, name: name))
            }
        case "content_block_delta":
            let index = object["index"] as? Int ?? 0
            guard let delta = object["delta"] as? [String: Any] else { break }
            switch delta["type"] as? String {
            case "text_delta":
                if let piece = delta["text"] as? String, !piece.isEmpty {
                    text += piece
                    out.append(.textDelta(piece))
                }
            case "input_json_delta":
                if let fragment = delta["partial_json"] as? String, var block = tools[index] {
                    block.json += fragment
                    tools[index] = block
                    if !fragment.isEmpty { out.append(.toolCallArgumentsDelta(id: block.id, fragment: fragment)) }
                }
            default: break
            }
        case "message_delta":
            if let delta = object["delta"] as? [String: Any], let reason = delta["stop_reason"] as? String { stopReason = reason }
            if let raw = object["usage"] as? [String: Any], let output = raw["output_tokens"] as? Int { outputTokens = output }
        case "message_stop":
            done = true
        case "error":
            let error = object["error"] as? [String: Any]
            let message = String((error?["message"] as? String ?? "provider error").prefix(512))
            throw LLMError.status(code: error?["type"] as? String == "overloaded_error" ? 529 : 500, message: message)
        default: break
        }
        return out
    }

    func finish() throws -> AgentTurn {
        guard done else { throw LLMError.malformed("stream ended before the model finished") }
        var calls: [ToolCall] = []
        for index in order {
            guard let block = tools[index], !block.name.isEmpty else { throw LLMError.malformed("tool call has no name") }
            calls.append(ToolCall(id: block.id, name: block.name, arguments: try parseArguments(block.json)))
        }
        let reason: AgentFinishReason
        switch stopReason {
        case "end_turn", "stop_sequence": reason = .stop
        case "tool_use": reason = .toolCalls
        case "max_tokens": reason = .length
        case let other?: reason = .other(other)
        case nil: reason = calls.isEmpty ? .stop : .toolCalls
        }
        if calls.isEmpty, text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, reason == .stop { throw LLMError.emptyOutput }
        return AgentTurn(text: text, toolCalls: calls, finishReason: reason, model: model ?? requested, provider: provider, usage: usage)
    }
}
