import Foundation
import RightKitHTTP

/// Sends chat completions to OpenAI-compatible providers.
public struct LLMClient: Sendable {
    public let http: HTTPClient
    public let credentials: any LLMCredentialSource
    public let userAgent: String
    public let quirks: [LLMQuirkRule]

    /// - Parameters:
    ///   - userAgent: sent on every call. Some providers (Groq) refuse the default URLSession agent,
    ///     so give it your app's name and version, for example `heardright-ios/0.1`.
    ///   - quirks: provider/model body adjustments; default ``LLMQuirkRule/defaults``.
    public init(
        credentials: any LLMCredentialSource,
        userAgent: String,
        http: HTTPClient = HTTPClient(),
        quirks: [LLMQuirkRule] = LLMQuirkRule.defaults,
    ) {
        self.credentials = credentials
        self.userAgent = userAgent
        self.http = http
        self.quirks = quirks
    }

    private static let reserved: Set<String> = [
        "model", "messages", "stream", "temperature", "max_tokens", "max_completion_tokens",
    ]

    /// One completion from one route.
    public func complete(_ request: ChatRequest, route: LLMRoute) async throws -> ChatCompletion {
        let provider = route.provider
        var key: String?
        if provider.requiresCredential {
            do { key = try credentials.credential(provider: provider.id) } catch { throw LLMError.missingCredential(provider: provider.id) }
            guard let found = key, !found.isEmpty else { throw LLMError.missingCredential(provider: provider.id) }
        }
        let body = try makeBody(request, route: route)
        let httpRequest = HTTPRequest(
            method: .post, url: provider.completionsURL,
            headers: ["Content-Type": "application/json", "User-Agent": userAgent],
            body: body, bearer: key, timeout: request.timeout,
        )
        let response: HTTPResponse
        do { response = try await http.send(httpRequest) } catch let error as HTTPError {
            throw Self.map(error)
        }
        return try Self.parse(response.data, provider: provider.id, requested: route.model, rejectTruncated: request.rejectTruncatedOutput)
    }

    /// Tries `routes` in order and returns the first completion `accept` keeps (default: any non-empty
    /// text). Cancellation and invalid requests stop at once; every other failure moves to the next
    /// route. One failed route rethrows its own error; several give ``LLMError/allRoutesFailed(_:)``.
    public func complete(
        _ request: ChatRequest,
        routes: [LLMRoute],
        accept: @Sendable (String) -> String? = { $0 },
    ) async throws -> ChatCompletion {
        guard !routes.isEmpty else { throw LLMError.noRoutes }
        var attempts: [LLMAttempt] = []
        var lastError: LLMError = .noRoutes
        for route in routes {
            do {
                var completion = try await complete(request, route: route)
                guard let accepted = accept(completion.text) else { throw LLMError.rejectedOutput }
                completion.text = accepted
                return completion
            } catch let error as LLMError {
                if case .invalidRequest = error { throw error }
                if error == .cancelled { throw error }
                lastError = error
                attempts.append(LLMAttempt(provider: route.provider.id, model: route.model, reason: Self.describe(error)))
            }
        }
        if attempts.count == 1 { throw lastError }
        throw LLMError.allRoutesFailed(attempts)
    }

    // MARK: Wire

    func makeBody(_ request: ChatRequest, route: LLMRoute) throws -> Data {
        guard !request.messages.isEmpty else { throw LLMError.invalidRequest("messages is empty") }
        var fields: [String: JSONValue] = [
            "model": .string(route.model),
            "messages": .array(request.messages.map {
                .object(["role": .string($0.role.rawValue), "content": .string($0.content)])
            }),
            "stream": .bool(false),
        ]
        if let temperature = request.temperature { fields["temperature"] = .number(temperature) }
        var maxTokens = request.maxTokens
        var extras: [String: JSONValue] = [:]
        for rule in quirks where rule.matches(providerID: route.provider.id, model: route.model) {
            for (k, v) in rule.extraBody { extras[k] = v }
            if let base = maxTokens, rule.extraOutputTokens > 0 {
                let raised = base + rule.extraOutputTokens
                maxTokens = rule.maxTokensCap.map { min($0, raised) } ?? raised
            }
        }
        for key in request.extraBody.keys where Self.reserved.contains(key) {
            throw LLMError.invalidRequest("extraBody sets reserved field \(key)")
        }
        // The caller's own fields win over a quirk default.
        for (k, v) in request.extraBody { extras[k] = v }
        if let maxTokens { fields[route.provider.maxTokensField.rawValue] = .number(Double(maxTokens)) }
        for (k, v) in extras { fields[k] = v }
        do { return try JSONEncoder().encode(fields) } catch { throw LLMError.invalidRequest("request is not encodable") }
    }

    private struct Wire: Decodable {
        struct Choice: Decodable {
            struct Message: Decodable { var content: String?; var refusal: String? }
            var message: Message?
            var finishReason: String?
            enum CodingKeys: String, CodingKey { case message; case finishReason = "finish_reason" }
        }
        var model: String?
        var choices: [Choice]?
        var usage: TokenUsage?
    }

    static func parse(_ data: Data, provider: String, requested: String, rejectTruncated: Bool) throws -> ChatCompletion {
        let wire: Wire
        do {
            wire = try JSONDecoder().decode(Wire.self, from: data)
        } catch { throw LLMError.malformed("response is not a chat completion") }
        guard let choice = wire.choices?.first, let message = choice.message else {
            throw LLMError.malformed("response has no choices")
        }
        if choice.finishReason == "length", rejectTruncated { throw LLMError.truncated }
        guard let content = message.content else { throw LLMError.malformed("response has no content") }
        let text = content.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { throw LLMError.emptyOutput }
        return ChatCompletion(
            text: text, finishReason: choice.finishReason, model: wire.model ?? requested, provider: provider, usage: wire.usage,
        )
    }

    static func map(_ error: HTTPError) -> LLMError {
        switch error {
        case let .status(code, message):
            return (code == 401 || code == 403) ? .auth(code: code, message: message) : .status(code: code, message: message)
        case .cancelled: return .cancelled
        default: return .http(error)
        }
    }

    private static func describe(_ error: LLMError) -> String {
        switch error {
        case let .status(code, message), let .auth(code, message): return "HTTP \(code): \(message)"
        case let .http(inner): return "transport: \(inner)"
        case let .missingCredential(provider): return "no credential for \(provider)"
        case let .malformed(why): return "malformed: \(why)"
        case .emptyOutput: return "empty output"
        case .truncated: return "truncated at the output limit"
        case .rejectedOutput: return "output rejected by the caller"
        default: return String(describing: error)
        }
    }
}
