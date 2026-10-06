//! Claude-style `.mcp.json` parsing with `${VAR}` / `${VAR:-default}` expansion.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::client::ServerSpec;
use crate::error::{McpError, Result};
use crate::pool::ServerEntry;
use crate::trust::ServerSource;

fn expand(s: &str, env: &HashMap<String, String>) -> Result<String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let end = after.find('}').ok_or_else(|| McpError::Config(format!("unterminated ${{ in '{s}'")))?;
        let inner = &after[..end];
        let (name, default) = match inner.split_once(":-") {
            Some((n, d)) => (n, Some(d)),
            None => (inner, None),
        };
        match (env.get(name), default) {
            (Some(v), _) => out.push_str(v),
            (None, Some(d)) => out.push_str(d),
            (None, None) => return Err(McpError::Config(format!("variable '{name}' is not set"))),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

pub fn parse_mcp_json(text: &str, source: ServerSource, env: &HashMap<String, String>) -> Result<Vec<ServerEntry>> {
    let v: Value = serde_json::from_str(text).map_err(|e| McpError::Config(e.to_string()))?;
    let servers = v.get("mcpServers").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for (name, s) in servers {
        let spec = if let Some(url) = s.get("url").and_then(Value::as_str) {
            if s.get("type").and_then(Value::as_str) == Some("sse") {
                return Err(McpError::Config(format!("server '{name}': legacy SSE transport is not supported")));
            }
            ServerSpec::Http { url: expand(url, env)? }
        } else if let Some(cmd) = s.get("command").and_then(Value::as_str) {
            let args = s
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(|x| expand(x, env)).collect::<Result<Vec<_>>>())
                .transpose()?
                .unwrap_or_default();
            let mut envm = BTreeMap::new();
            if let Some(o) = s.get("env").and_then(Value::as_object) {
                for (k, val) in o {
                    envm.insert(k.clone(), expand(val.as_str().unwrap_or(""), env)?);
                }
            }
            ServerSpec::Stdio {
                command: expand(cmd, env)?,
                args,
                env: envm,
                cwd: s.get("cwd").and_then(Value::as_str).map(Into::into),
            }
        } else {
            return Err(McpError::Config(format!("server '{name}' has neither command nor url")));
        };
        out.push(ServerEntry { name, spec, source: source.clone() });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}
