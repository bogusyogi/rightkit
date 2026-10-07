//! Tool-name namespacing: `mcp__{server}__{tool}` (ported from CodeRight's mcp crate).

fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_us = false;
    for ch in s.chars() {
        let m = if ch.is_ascii_alphanumeric() || ch == '_' {
            ch.to_ascii_lowercase()
        } else {
            '_'
        };
        if m == '_' {
            if last_us {
                continue;
            }
            last_us = true;
        } else {
            last_us = false;
        }
        out.push(m);
    }
    out
}

pub fn sanitize_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", clean(server), clean(tool))
}

pub fn parse_qualified_name(qualified: &str) -> Option<(String, String)> {
    let rest = qualified.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}
