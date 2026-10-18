import Foundation
import RightKitHTTP
import Testing
@testable import RightKitLLM

private final class FakeTransport: HTTPTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var replies: [(Int, String)]
    private var _requests: [URLRequest] = []
    init(_ replies: [(Int, String)]) { self.replies = replies }
    var requests: [URLRequest] { lock.withLock { _requests } }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let (status, body) = lock.withLock { () -> (Int, String) in
            _requests.append(request)
            return replies.count > 1 ? replies.removeFirst() : replies[0]
        }
        return (Data(body.utf8), HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!)
    }

    func stream(_ request: URLRequest) async throws -> (HTTPURLResponse, AsyncThrowingStream<Data, Error>) {
        throw URLError(.unsupportedURL)
    }
}

private func completion(_ text: String, finish: String = "stop") -> String {
    let content = String(data: try! JSONEncoder().encode(text), encoding: .utf8)!
    return #"{"model":"m","choices":[{"message":{"content":\#(content)},"finish_reason":"\#(finish)"}],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}}"#
}

private func client(_ fake: FakeTransport, keys: [String: String] = ["groq": "gk", "openrouter": "ok"]) -> LLMClient {
    LLMClient(credentials: StaticLLMCredentials(keys), userAgent: "test/1", http: HTTPClient(transport: fake))
}

private func body(_ request: URLRequest) throws -> [String: Any] {
    let data = try #require(request.httpBody)
    return try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
}

private let groqQwen = LLMRoute(provider: .groq, model: "qwen/qwen3.6-27b")
private let req = ChatRequest(messages: [.system("s"), .user("hi")], temperature: 0, maxTokens: 100)

@Test func sendsOpenAICompatibleBodyWithUserAgentAndKey() async throws {
    let fake = FakeTransport([(200, completion("  hello  "))])
    let result = try await client(fake).complete(req, route: groqQwen)
    #expect(result.text == "hello")
    #expect(result.usage?.totalTokens == 7)
    let sent = try #require(fake.requests.first)
    #expect(sent.url?.absoluteString == "https://api.groq.com/openai/v1/chat/completions")
    #expect(sent.value(forHTTPHeaderField: "Authorization") == "Bearer gk")
    #expect(sent.value(forHTTPHeaderField: "User-Agent") == "test/1")
    let json = try body(sent)
    #expect(json["model"] as? String == "qwen/qwen3.6-27b")
    #expect(json["stream"] as? Bool == false)
    #expect(json["max_tokens"] as? Int == 100)
    #expect(json["reasoning_effort"] as? String == "none")
    #expect((json["messages"] as? [[String: String]])?.count == 2)
}

@Test func groqGptOssGetsLowEffortAndRaisedBoundedTokens() async throws {
    let fake = FakeTransport([(200, completion("ok"))])
    _ = try await client(fake).complete(ChatRequest(messages: [.user("x")], maxTokens: 1_900), route: LLMRoute(provider: .groq, model: "openai/gpt-oss-120b"))
    let json = try body(try #require(fake.requests.first))
    #expect(json["reasoning_effort"] as? String == "low")
    #expect(json["include_reasoning"] as? Bool == false)
    #expect(json["max_tokens"] as? Int == 2_048)
    // The same model on another provider gets no gpt-oss quirk.
    let other = FakeTransport([(200, completion("ok"))])
    _ = try await client(other).complete(ChatRequest(messages: [.user("x")], maxTokens: 10), route: LLMRoute(provider: .openRouter, model: "openai/gpt-oss-120b"))
    let otherRequest = try #require(other.requests.first)
    #expect(try body(otherRequest)["reasoning_effort"] == nil)
}

@Test func openAIUsesMaxCompletionTokens() async throws {
    let fake = FakeTransport([(200, completion("ok"))])
    _ = try await client(fake, keys: ["openai": "k"]).complete(req, route: LLMRoute(provider: .openAI, model: "gpt-x"))
    let json = try body(try #require(fake.requests.first))
    #expect(json["max_completion_tokens"] as? Int == 100)
    #expect(json["max_tokens"] == nil)
}

@Test func missingCredentialFailsBeforeAnyRequest() async {
    let fake = FakeTransport([(200, completion("ok"))])
    await #expect(throws: LLMError.missingCredential(provider: "groq")) {
        _ = try await client(fake, keys: [:]).complete(req, route: groqQwen)
    }
    #expect(fake.requests.isEmpty)
}

@Test func localProviderNeedsNoKey() async throws {
    let fake = FakeTransport([(200, completion("ok"))])
    _ = try await client(fake, keys: [:]).complete(req, route: LLMRoute(provider: .ollama, model: "llama"))
    #expect(fake.requests.first?.value(forHTTPHeaderField: "Authorization") == nil)
}

@Test func statusAndAuthErrorsAreTypedAndKeyIsRedacted() async {
    let fake = FakeTransport([(401, #"{"error":{"message":"bad key gk"}}"#)])
    await #expect(throws: LLMError.auth(code: 401, message: "bad key [redacted]")) {
        _ = try await client(fake).complete(req, route: groqQwen)
    }
    let limited = FakeTransport([(429, #"{"error":"slow down"}"#)])
    await #expect(throws: LLMError.status(code: 429, message: "slow down")) {
        _ = try await client(limited).complete(req, route: groqQwen)
    }
}

@Test func truncationAndEmptyAndMalformed() async {
    let cut = FakeTransport([(200, completion("partial", finish: "length"))])
    let strict = ChatRequest(messages: [.user("x")], rejectTruncatedOutput: true)
    await #expect(throws: LLMError.truncated) { _ = try await client(cut).complete(strict, route: groqQwen) }
    let lenient = try? await client(cut).complete(ChatRequest(messages: [.user("x")]), route: groqQwen)
    #expect(lenient?.text == "partial")

    let empty = FakeTransport([(200, completion("   "))])
    await #expect(throws: LLMError.emptyOutput) { _ = try await client(empty).complete(req, route: groqQwen) }
    let junk = FakeTransport([(200, "{}")])
    await #expect(throws: LLMError.self) { _ = try await client(junk).complete(req, route: groqQwen) }
}

@Test func reservedExtraFieldIsRefusedBeforeSending() async {
    let fake = FakeTransport([(200, completion("ok"))])
    let bad = ChatRequest(messages: [.user("x")], extraBody: ["model": .string("evil")])
    await #expect(throws: LLMError.invalidRequest("extraBody sets reserved field model")) {
        _ = try await client(fake).complete(bad, route: groqQwen)
    }
    #expect(fake.requests.isEmpty)
}

@Test func failoverMovesToNextRouteAndAcceptHookCanReject() async throws {
    let fake = FakeTransport([(503, #"{"error":"down"}"#), (200, completion("```fenced```")), (200, completion("clean"))])
    let routes = [groqQwen, LLMRoute(provider: .groq, model: "openai/gpt-oss-120b"), LLMRoute(provider: .openRouter, model: "qwen/qwen3-32b")]
    let result = try await client(fake).complete(req, routes: routes) { $0.hasPrefix("```") ? nil : $0 }
    #expect(result.text == "clean")
    #expect(result.provider == "openrouter")
    #expect(fake.requests.count == 3)
}

@Test func allRoutesFailedListsEveryAttempt() async {
    let fake = FakeTransport([(500, #"{"error":"a"}"#)])
    let routes = [groqQwen, LLMRoute(provider: .openRouter, model: "qwen/qwen3-32b")]
    do {
        _ = try await client(fake).complete(req, routes: routes)
        Issue.record("expected failure")
    } catch let LLMError.allRoutesFailed(attempts) {
        #expect(attempts.map(\.provider) == ["groq", "openrouter"])
    } catch { Issue.record("wrong error \(error)") }
    await #expect(throws: LLMError.noRoutes) { _ = try await client(fake).complete(req, routes: []) }
}

@Test func presetsMatchTheRustTable() {
    #expect(LLMProvider.preset("groq")?.baseURL.absoluteString == "https://api.groq.com/openai/v1")
    #expect(LLMProvider.preset("ollama")?.requiresCredential == false)
    #expect(LLMProvider.preset("nope") == nil)
}
