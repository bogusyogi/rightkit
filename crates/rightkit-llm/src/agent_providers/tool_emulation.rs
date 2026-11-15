//! Tool calls for a model that has no native tool calling (route key `tool-calls = false`).
//!
//! The request goes out with no `tools`. The tool list and the two reply shapes are described in
//! the system prompt, earlier tool calls and results are written back into the history as plain
//! text, and the reply is read back for calls:
//!
//! - a line that starts with `$ ` is one `shell` call (or `powershell` when that is the shell
//!   the host offers), the rest of the line being the command;
//! - a fenced block (```` ```json ````) holding `{"tool": "<name>", "arguments": {...}}` (or an
//!   array of them) is one call per object, for any tool in the list.
//!
//! A line or block that does not parse, or names a tool the request did not offer, stays in the
//! text. Matched markup is removed from the text.

use rightkit_agent_engine_protocol::ToolCall;
use serde_json::Value;

/// Upper bound for one tool's description line in the instruction.
const DESCRIPTION_CHARS: usize = 240;

/// `(name, description, [(parameter, required)])` of one offered tool.
type ToolRow = (String, String, Vec<(String, bool)>);

/// What a request's tool list looks like to the emulation.
pub struct Emulation {
    /// One row per offered tool.
    tools: Vec<ToolRow>,
    shell: Option<&'static str>,
}

impl Emulation {
    /// Read the OpenAI-shaped tool definitions (`{"type":"function","function":{...}}`).
    pub fn new(tools: &[Value]) -> Self {
        let mut listed = Vec::new();
        for tool in tools {
            let function = tool.get("function").unwrap_or(tool);
            let Some(name) = function.get("name").and_then(Value::as_str) else {
                continue;
            };
            let description = function
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(DESCRIPTION_CHARS)
                .collect::<String>();
            let schema = function.get("parameters");
            let required: Vec<&str> = schema
                .and_then(|s| s.get("required"))
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let params = schema
                .and_then(|s| s.get("properties"))
                .and_then(Value::as_object)
                .map(|props| {
                    props
                        .keys()
                        .map(|key| (key.clone(), required.contains(&key.as_str())))
                        .collect()
                })
                .unwrap_or_default();
            listed.push((name.to_string(), description, params));
        }
        let has = |wanted: &str| listed.iter().any(|(name, _, _)| name == wanted);
        let shell = if has("shell") {
            Some("shell")
        } else if has("powershell") {
            Some("powershell")
        } else {
            None
        };
        Self {
            tools: listed,
            shell,
        }
    }

    /// The text added to the system prompt.
    pub fn instruction(&self) -> String {
        let mut out = String::from(
            "This model has no native tool calling. To use a tool, write the call in your reply \
             in one of these exact forms and stop; the result comes back as the next message.\n",
        );
        if let Some(shell) = self.shell {
            out.push_str(&format!(
                "- To run a command (the `{shell}` tool), write one line that starts with `$ ` \
                 followed by the command, for example:\n$ ls -la\n"
            ));
        }
        out.push_str(
            "- For any tool, write a fenced json block holding {\"tool\": \"<name>\", \"arguments\": {...}}, for example:\n\
             ```json\n{\"tool\": \"read_file\", \"arguments\": {\"path\": \"src/main.rs\"}}\n```\n\
             Write plain text with no call when you are done.\n\nTools:\n",
        );
        for (name, description, params) in &self.tools {
            out.push_str(&format!("- {name}"));
            if !description.is_empty() {
                out.push_str(&format!(": {description}"));
            }
            if !params.is_empty() {
                let list = params
                    .iter()
                    .map(|(key, required)| {
                        if *required {
                            format!("{key}*")
                        } else {
                            key.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(" (arguments: {list}; * is required)"));
            }
            out.push('\n');
        }
        out
    }

    /// History for a route with no tool fields: the instruction joins the system prompt, an
    /// assistant message's tool calls become the text that would have called them, and a tool
    /// result becomes a user message naming its tool.
    pub fn rewrite_messages(&self, messages: Vec<Value>) -> Vec<Value> {
        let mut names: std::collections::HashMap<String, String> = Default::default();
        let mut out: Vec<Value> = Vec::with_capacity(messages.len() + 1);
        let mut instructed = false;
        let instruction = self.instruction();
        for mut message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            match role.as_str() {
                "system" if !instructed => {
                    instructed = true;
                    if let Some(text) = message.get("content").and_then(Value::as_str) {
                        message["content"] = Value::String(format!("{text}\n\n{instruction}"));
                    } else {
                        out.push(serde_json::json!({"role": "system", "content": instruction}));
                    }
                    out.push(message);
                }
                "assistant" if message.get("tool_calls").is_some_and(|c| !c.is_null()) => {
                    let mut text = content_text(message.get("content"));
                    for call in message["tool_calls"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                    {
                        let function = call.get("function").unwrap_or(&call);
                        let name = function.get("name").and_then(Value::as_str).unwrap_or("");
                        if let Some(id) = call.get("id").and_then(Value::as_str) {
                            names.insert(id.to_string(), name.to_string());
                        }
                        let arguments: Value = function
                            .get("arguments")
                            .and_then(Value::as_str)
                            .and_then(|raw| serde_json::from_str(raw).ok())
                            .unwrap_or_else(|| serde_json::json!({}));
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&self.render_call(name, &arguments));
                    }
                    out.push(serde_json::json!({"role": "assistant", "content": text}));
                }
                "tool" => {
                    let name = message
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .and_then(|id| names.get(id))
                        .cloned()
                        .unwrap_or_else(|| "tool".to_string());
                    let body = content_text(message.get("content"));
                    out.push(serde_json::json!({
                        "role": "user",
                        "content": format!("Result of `{name}`:\n{body}"),
                    }));
                }
                _ => out.push(message),
            }
        }
        if !instructed {
            out.insert(
                0,
                serde_json::json!({"role": "system", "content": instruction}),
            );
        }
        out
    }

    fn render_call(&self, name: &str, arguments: &Value) -> String {
        let only_command = arguments
            .as_object()
            .is_some_and(|map| map.len() == 1 && map.contains_key("command"));
        if Some(name) == self.shell && only_command {
            if let Some(command) = arguments["command"].as_str().filter(|c| !c.contains('\n')) {
                return format!("$ {command}");
            }
        }
        format!(
            "```json\n{}\n```",
            serde_json::json!({"tool": name, "arguments": arguments})
        )
    }

    /// Split a reply into its remaining text and the calls it made. `next_id` names each call.
    pub fn parse_reply(
        &self,
        text: &str,
        mut next_id: impl FnMut() -> String,
    ) -> (String, Vec<ToolCall>) {
        let lines: Vec<&str> = text.lines().collect();
        let mut kept: Vec<&str> = Vec::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim_start();
            if let Some(fence) = trimmed.strip_prefix("```") {
                let language = fence.trim().to_ascii_lowercase();
                if let Some(end) = lines[i + 1..]
                    .iter()
                    .position(|l| l.trim_start().starts_with("```"))
                {
                    let body = &lines[i + 1..i + 1 + end];
                    let found = self.calls_in_fence(&language, body, &mut next_id);
                    if let Some(found) = found {
                        calls.extend(found);
                        i += end + 2;
                        continue;
                    }
                    kept.extend(&lines[i..i + end + 2]);
                    i += end + 2;
                    continue;
                }
            } else if let (Some(command), Some(shell)) = (trimmed.strip_prefix("$ "), self.shell) {
                let command = command.trim();
                if !command.is_empty() {
                    calls.push(shell_call(shell, command, next_id()));
                    i += 1;
                    continue;
                }
            }
            kept.push(line);
            i += 1;
        }
        (kept.join("\n").trim().to_string(), calls)
    }

    fn calls_in_fence(
        &self,
        language: &str,
        body: &[&str],
        next_id: &mut impl FnMut() -> String,
    ) -> Option<Vec<ToolCall>> {
        match language {
            "" | "json" | "jsonc" | "tool" | "tool_call" => {
                let value: Value = serde_json::from_str(&body.join("\n")).ok()?;
                let items = match value {
                    Value::Array(items) => items,
                    other => vec![other],
                };
                let mut found = Vec::new();
                for item in items {
                    let name = item
                        .get("tool")
                        .or_else(|| item.get("name"))
                        .and_then(Value::as_str)?;
                    if !self.tools.iter().any(|(known, _, _)| known == name) {
                        return None;
                    }
                    let arguments = item
                        .get("arguments")
                        .or_else(|| item.get("args"))
                        .or_else(|| item.get("input"))
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    if !arguments.is_object() {
                        return None;
                    }
                    found.push(ToolCall {
                        id: next_id(),
                        name: name.to_string(),
                        arguments_json: arguments.to_string(),
                    });
                }
                (!found.is_empty()).then_some(found)
            }
            "sh" | "bash" | "shell" | "zsh" | "console" | "powershell" => {
                let shell = self.shell?;
                let commands: Vec<&str> = body
                    .iter()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .map(|l| l.strip_prefix("$ "))
                    .collect::<Option<_>>()?;
                (!commands.is_empty()).then(|| {
                    commands
                        .into_iter()
                        .map(|command| shell_call(shell, command.trim(), next_id()))
                        .collect()
                })
            }
            _ => None,
        }
    }
}

fn shell_call(shell: &str, command: &str, id: String) -> ToolCall {
    ToolCall {
        id,
        name: shell.to_string(),
        arguments_json: serde_json::json!({"command": command}).to_string(),
    }
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
