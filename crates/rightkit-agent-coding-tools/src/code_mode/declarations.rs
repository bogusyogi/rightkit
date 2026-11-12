//! JSON-Schema to TypeScript declaration rendering for `run_program`.
//!
//! The generated `.d.ts` is emitted into the tool description straight from
//! the registered tools' `input_schema()`, so the typed API the model sees can
//! never drift from the tools it actually calls. Only the subset of JSON
//! Schema the tool registry uses is rendered; anything else becomes `unknown`.

use serde_json::Value;

const MAX_DEPTH: usize = 4;
const MAX_DOC_CHARS: usize = 160;

/// One callable tool as the declaration generator sees it.
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' || first == '$' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        }
        _ => false,
    }
}

fn one_line_doc(text: &str) -> String {
    let flat = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("*/", "* /");
    if flat.chars().count() > MAX_DOC_CHARS {
        let cut: String = flat.chars().take(MAX_DOC_CHARS).collect();
        format!("{cut}...")
    } else {
        flat
    }
}

fn literal(value: &Value) -> String {
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => value.to_string(),
        _ => "unknown".to_string(),
    }
}

/// Render one JSON Schema node as a TypeScript type expression.
pub fn schema_to_ts(schema: &Value, depth: usize) -> String {
    if depth > MAX_DEPTH {
        return "unknown".to_string();
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.is_empty() {
            return values.iter().map(literal).collect::<Vec<_>>().join(" | ");
        }
    }
    if let Some(constant) = schema.get("const") {
        return literal(constant);
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(options) = schema.get(key).and_then(Value::as_array) {
            if !options.is_empty() {
                return options
                    .iter()
                    .map(|option| schema_to_ts(option, depth + 1))
                    .collect::<Vec<_>>()
                    .join(" | ");
            }
        }
    }
    match schema.get("type") {
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .map(|kind| scalar_or_composite(schema, kind, depth))
            .collect::<Vec<_>>()
            .join(" | "),
        Some(Value::String(kind)) => scalar_or_composite(schema, kind, depth),
        _ if schema.get("properties").is_some() => scalar_or_composite(schema, "object", depth),
        _ => "unknown".to_string(),
    }
}

fn scalar_or_composite(schema: &Value, kind: &str, depth: usize) -> String {
    match kind {
        "string" => "string".to_string(),
        "integer" | "number" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => {
            let item = schema
                .get("items")
                .map(|items| schema_to_ts(items, depth + 1))
                .unwrap_or_else(|| "unknown".to_string());
            if item.contains(" | ") {
                format!("Array<{item}>")
            } else {
                format!("{item}[]")
            }
        }
        "object" => object_to_ts(schema, depth),
        _ => "unknown".to_string(),
    }
}

fn object_to_ts(schema: &Value, depth: usize) -> String {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return "Record<string, unknown>".to_string();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut members = Vec::new();
    for (key, value) in properties {
        let name = if is_identifier(key) {
            key.clone()
        } else {
            Value::String(key.clone()).to_string()
        };
        let optional = if required.contains(&key.as_str()) {
            ""
        } else {
            "?"
        };
        let doc = value
            .get("description")
            .and_then(Value::as_str)
            .map(one_line_doc)
            .filter(|doc| !doc.is_empty())
            .map(|doc| format!("/** {doc} */ "))
            .unwrap_or_default();
        members.push(format!(
            "{doc}{name}{optional}: {}",
            schema_to_ts(value, depth + 1)
        ));
    }
    if schema.get("additionalProperties") == Some(&Value::Bool(true)) {
        members.push("[key: string]: unknown".to_string());
    }
    if members.is_empty() {
        return "Record<string, never>".to_string();
    }
    format!("{{ {} }}", members.join("; "))
}

/// Render `declare const tools: { ... }` for the given read-only tools.
pub fn render_tools_namespace(tools: &[ToolDecl]) -> String {
    let mut out = String::from("declare const tools: {\n");
    for tool in tools {
        let doc = one_line_doc(&tool.description);
        if !doc.is_empty() {
            out.push_str(&format!("  /** {doc} */\n"));
        }
        let name = if is_identifier(&tool.name) {
            tool.name.clone()
        } else {
            Value::String(tool.name.clone()).to_string()
        };
        out.push_str(&format!(
            "  {name}(args: {}): Promise<string>;\n",
            schema_to_ts(&tool.schema, 0)
        ));
    }
    out.push_str("};\n");
    out
}
