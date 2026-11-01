import Foundation

/// Anthropic Messages API (https://api.anthropic.com/v1/messages) with the owner's key and typed model id.
struct AnthropicProvider: AgentProvider {
    let apiKey: String

    func complete(system: String, turns: [AgentTurn], tools: [AgentToolSpec], model: String,
                  maxOutputTokens: Int) async throws -> AgentReply {
        var messages: [[String: Any]] = []
        for turn in turns {
            switch turn {
            case .user(let text):
                messages.append(["role": "user", "content": text])
            case .assistant(let text, let calls):
                var blocks: [[String: Any]] = []
                if !text.isEmpty { blocks.append(["type": "text", "text": text]) }
                for call in calls {
                    blocks.append(["type": "tool_use", "id": call.id, "name": call.name,
                                   "input": AgentJSON.object(call.argumentsJSON)])
                }
                messages.append(["role": "assistant", "content": blocks])
            case .toolResult(let id, _, let text):
                let block: [String: Any] = ["type": "tool_result", "tool_use_id": id, "content": text]
                // Results of one assistant turn travel together in one user message.
                if var last = messages.last, last["role"] as? String == "user",
                   var content = last["content"] as? [[String: Any]], content.first?["type"] as? String == "tool_result" {
                    content.append(block)
                    last["content"] = content
                    messages[messages.count - 1] = last
                } else {
                    messages.append(["role": "user", "content": [block]])
                }
            }
        }
        var body: [String: Any] = [
            "model": model,
            "max_tokens": maxOutputTokens,
            "system": system,
            "messages": messages,
        ]
        if !tools.isEmpty {
            body["tools"] = tools.map {
                ["name": $0.name, "description": $0.description, "input_schema": AgentJSON.object($0.schemaJSON)] as [String: Any]
            }
        }
        let payload = try AgentJSON.data(body)
        guard let url = URL(string: AgentProviderKind.anthropic.defaultEndpoint) else {
            throw AgentError.notSent("The endpoint is not a web address.")
        }
        let data = try await AgentHTTP.post(
            url: url,
            headers: ["x-api-key": apiKey, "anthropic-version": "2023-06-01"],
            body: payload,
            secret: apiKey
        )
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let content = json["content"] as? [[String: Any]] else {
            throw AgentError.malformed("No content in the answer.")
        }
        var text = ""
        var calls: [AgentToolCall] = []
        for block in content {
            switch block["type"] as? String {
            case "text":
                text += block["text"] as? String ?? ""
            case "tool_use":
                guard let id = block["id"] as? String, let name = block["name"] as? String else { continue }
                calls.append(AgentToolCall(id: id, name: name, argumentsJSON: AgentJSON.text(block["input"] ?? [String: Any]())))
            default:
                break
            }
        }
        let usage = json["usage"] as? [String: Any]
        return AgentReply(text: text, calls: calls,
                          inputTokens: usage?["input_tokens"] as? Int,
                          outputTokens: usage?["output_tokens"] as? Int)
    }
}
