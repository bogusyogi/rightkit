import Foundation

// The phone agent's model seam (#408). Providers are bring-your-own-key: the owner's key, the owner's typed
// model id, no provider or model chosen for them. A provider makes exactly one request per `complete`; it never
// retries and never falls back to another provider (a paid retry could spend twice).
//
// Transport is URLSession: RightKitSwift 0.8.1 has no LLM or HTTP module. Recorded ownership exception, request
// rightkit#891; delete these two providers when RightKit publishes one.

enum AgentProviderKind: String, CaseIterable, Identifiable, Sendable {
    case anthropic
    case openAICompatible

    var id: String { rawValue }

    var label: String {
        switch self {
        case .anthropic: "Anthropic"
        case .openAICompatible: "OpenAI or compatible"
        }
    }

    /// The endpoint filled in for the owner to check; the OpenAI-style one can be edited, Anthropic's cannot.
    var defaultEndpoint: String {
        switch self {
        case .anthropic: "https://api.anthropic.com/v1/messages"
        case .openAICompatible: "https://api.openai.com/v1/chat/completions"
        }
    }

    var endpointEditable: Bool { self == .openAICompatible }
}

struct AgentToolSpec: Sendable {
    let name: String
    let description: String
    /// A JSON Schema object, as JSON text.
    let schemaJSON: String
}

struct AgentToolCall: Sendable, Hashable {
    let id: String
    let name: String
    /// The arguments exactly as the model produced them, as JSON text.
    let argumentsJSON: String
}

enum AgentTurn: Sendable {
    case user(String)
    case assistant(text: String, calls: [AgentToolCall])
    case toolResult(callID: String, name: String, text: String)
}

struct AgentReply: Sendable {
    var text: String
    var calls: [AgentToolCall]
    var inputTokens: Int?
    var outputTokens: Int?
}

protocol AgentProvider: Sendable {
    func complete(
        system: String,
        turns: [AgentTurn],
        tools: [AgentToolSpec],
        model: String,
        maxOutputTokens: Int
    ) async throws -> AgentReply
}

/// What a failed call means for the budget.
enum AgentBilling: Sendable {
    /// The provider refused before doing any work (bad key, bad request, rate limit): nothing was spent.
    case notBilled
    /// The request may have been processed: it counts as spent.
    case unknown
}

enum AgentError: Error, LocalizedError {
    case notReady(String)
    case budget(String)
    case http(status: Int, message: String)
    /// Sent (or possibly sent), and no answer was read.
    case unknownOutcome(String)
    /// The request never left the phone.
    case notSent(String)
    case malformed(String)
    case tooManySteps

    var errorDescription: String? {
        switch self {
        case .notReady(let text), .budget(let text): text
        case .http(let status, let message): "The provider refused the request (\(status)). \(message)"
        case .unknownOutcome(let text): "No answer arrived, so that call counts as spent. \(text)"
        case .notSent(let text): text
        case .malformed(let text): "The provider's answer could not be read. \(text)"
        case .tooManySteps: "The agent stopped after too many steps."
        }
    }

    var billing: AgentBilling {
        switch self {
        case .http(let status, _): [400, 401, 403, 404, 413, 422, 429].contains(status) ? .notBilled : .unknown
        case .notSent, .notReady, .budget: .notBilled
        case .unknownOutcome, .malformed, .tooManySteps: .unknown
        }
    }
}

/// One HTTPS POST with the owner's key. The key goes in a header only, never in a URL, a log or an error.
enum AgentHTTP {
    static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 90
        configuration.timeoutIntervalForResource = 120
        configuration.urlCache = nil
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.waitsForConnectivity = false
        return URLSession(configuration: configuration)
    }()

    static func post(url: URL, headers: [String: String], body: Data, secret: String) async throws -> Data {
        guard url.scheme?.lowercased() == "https", url.host != nil else {
            throw AgentError.notSent("The endpoint must be an https address.")
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        for (name, value) in headers { request.setValue(value, forHTTPHeaderField: name) }
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch let error as URLError {
            let neverSent: Set<URLError.Code> = [.notConnectedToInternet, .cannotFindHost, .dnsLookupFailed, .cannotConnectToHost]
            if neverSent.contains(error.code) { throw AgentError.notSent("Couldn't reach the provider.") }
            throw AgentError.unknownOutcome("The connection ended early.")
        } catch {
            throw AgentError.unknownOutcome("The connection ended early.")
        }
        guard let http = response as? HTTPURLResponse else { throw AgentError.malformed("No HTTP answer.") }
        guard (200..<300).contains(http.statusCode) else {
            throw AgentError.http(status: http.statusCode, message: errorMessage(data, redacting: secret))
        }
        return data
    }

    /// The provider's own error sentence, shortened, with the key removed if it was echoed back.
    private static func errorMessage(_ data: Data, redacting secret: String) -> String {
        var text = ""
        if let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            if let error = json["error"] as? [String: Any], let message = error["message"] as? String {
                text = message
            } else if let message = json["message"] as? String {
                text = message
            }
        }
        if !secret.isEmpty { text = text.replacingOccurrences(of: secret, with: "[key]") }
        return String(text.prefix(200))
    }
}

enum AgentJSON {
    static func object(_ json: String) -> Any {
        (try? JSONSerialization.jsonObject(with: Data(json.utf8), options: [.fragmentsAllowed])) ?? [String: Any]()
    }

    static func data(_ value: Any) throws -> Data {
        guard JSONSerialization.isValidJSONObject(value) else { throw AgentError.notSent("The request could not be built.") }
        return try JSONSerialization.data(withJSONObject: value)
    }

    static func text(_ value: Any) -> String {
        guard JSONSerialization.isValidJSONObject(value),
              let data = try? JSONSerialization.data(withJSONObject: value),
              let text = String(data: data, encoding: .utf8) else { return "{}" }
        return text
    }
}
