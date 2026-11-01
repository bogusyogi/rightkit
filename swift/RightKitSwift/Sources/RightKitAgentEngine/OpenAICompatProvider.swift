import Foundation

/// An OpenAI-style chat-completions endpoint (OpenAI's own or a compatible one) with the owner's key, typed
/// model id and endpoint. Unverified against a live account in this build.
struct OpenAICompatProvider: AgentProvider {
    let apiKey: String
    let endpoint: String

    func complete(system: String, turns: [AgentTurn], tools: [AgentToolSpec], model: String,
                  maxOutputTokens: Int) async throws -> AgentReply {
        var messages: [[String: Any]] = [["role": "system", "content": system]]
        for turn in turns {
            switch turn {
            case .user(let text):
                messages.append(["role": "user", "content": text])
            case .assistant(let text, let calls):
                var message: [String: Any] = ["role": "assistant", "content": text]
                if !calls.isEmpty {
                    message["tool_calls"] = calls.map {
                        ["id": $0.id, "type": "function",
                         "function": ["name": $0.name, "arguments": $0.argumentsJSON]] as [String: Any]
                    }
                }
                messages.append(message)
            case .toolResult(let id, _, let text):
                messages.append(["role": "tool", "tool_call_id": id, "content": text])
            }
        }
        guard let url = URL(string: endpoint) else { throw AgentError.notSent("The endpoint is not a web address.") }
        var body: [String: Any] = ["model": model, "messages": messages]
        // OpenAI's own newer models reject max_tokens; compatible servers expect it.
        body[url.host == "api.openai.com" ? "max_completion_tokens" : "max_tokens"] = maxOutputTokens
        if !tools.isEmpty {
            body["tools"] = tools.map {
                ["type": "function",
                 "function": ["name": $0.name, "description": $0.description,
                              "parameters": AgentJSON.object($0.schemaJSON)] as [String: Any]] as [String: Any]
            }
        }
        let data = try await AgentHTTP.post(
            url: url,
            headers: ["authorization": "Bearer \(apiKey)"],
            body: try AgentJSON.data(body),
            secret: apiKey
        )
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let choice = (json["choices"] as? [[String: Any]])?.first,
              let message = choice["message"] as? [String: Any] else {
            throw AgentError.malformed("No choices in the answer.")
        }
        var calls: [AgentToolCall] = []
        for call in message["tool_calls"] as? [[String: Any]] ?? [] {
            guard let id = call["id"] as? String, let function = call["function"] as? [String: Any],
                  let name = function["name"] as? String else { continue }
            calls.append(AgentToolCall(id: id, name: name, argumentsJSON: function["arguments"] as? String ?? "{}"))
        }
        let usage = json["usage"] as? [String: Any]
        return AgentReply(text: message["content"] as? String ?? "", calls: calls,
                          inputTokens: usage?["prompt_tokens"] as? Int,
                          outputTokens: usage?["completion_tokens"] as? Int)
    }
}
