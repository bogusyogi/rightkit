// Curated MCP server catalog for `coderight mcp catalog` / `coderight mcp install`.
// Static, in-code list (no network fetch, no signing) — mirrors the plugin
// marketplace's Install/InstallMarketplace pattern but scoped down to what the
// #224 gap actually needs: a curated list + one-command resolve-and-add.

/// One curated, installable MCP server entry.
struct McpCatalogEntry {
    name: &'static str,
    description: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    /// Extra stdio env as (name, value); values may use `${VAR}` placeholders
    /// that the MCP config loader expands at spawn time.
    env: &'static [(&'static str, &'static str)],
}

const MCP_CATALOG: &[McpCatalogEntry] = &[
    McpCatalogEntry {
        name: "context7",
        description: "Up-to-date library/framework documentation lookup.",
        command: "npx",
        args: &["-y", "@upstash/context7-mcp"],
        env: &[],
    },
    McpCatalogEntry {
        name: "filesystem",
        description: "Read/write access to a local directory tree.",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-filesystem"],
        env: &[],
    },
    McpCatalogEntry {
        name: "github",
        description: "GitHub's official github/github-mcp-server (stdio mode); requires github-mcp-server on PATH. The GitHub token is passed at spawn through GITHUB_PERSONAL_ACCESS_TOKEN.",
        command: "github-mcp-server",
        args: &["stdio"],
        // Expanded from the daemon environment when the server is spawned.
        env: &[("GITHUB_PERSONAL_ACCESS_TOKEN", "${GITHUB_PERSONAL_ACCESS_TOKEN}")],
    },
    McpCatalogEntry {
        name: "memory",
        description: "Persistent knowledge-graph memory across sessions.",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-memory"],
        env: &[],
    },
    McpCatalogEntry {
        name: "sequential-thinking",
        description: "Structured step-by-step reasoning tool.",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-sequential-thinking"],
        env: &[],
    },
    McpCatalogEntry {
        name: "scraperight",
        description: "ScrapeRight engine (OCR, transcription, scraping) via its MCP server mode; requires scraperight-engine on PATH.",
        command: "scraperight-engine",
        args: &["--mcp"],
        env: &[],
    },
];

fn mcp_entry_env(entry: &McpCatalogEntry) -> serde_json::Value {
    serde_json::Value::Object(
        entry
            .env
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_string(),
                    serde_json::Value::String((*v).to_string()),
                )
            })
            .collect(),
    )
}

fn mcp_catalog_entry_json(entry: &McpCatalogEntry) -> serde_json::Value {
    serde_json::json!({
        "name": entry.name,
        "description": entry.description,
        "config": {
            "type": "stdio",
            "command": entry.command,
            "args": entry.args,
            "env": mcp_entry_env(entry),
        },
    })
}

fn mcp_catalog_json() -> serde_json::Value {
    serde_json::json!({
        "catalog": MCP_CATALOG.iter().map(mcp_catalog_entry_json).collect::<Vec<_>>(),
    })
}

fn find_mcp_catalog_entry(name: &str) -> Option<&'static McpCatalogEntry> {
    MCP_CATALOG.iter().find(|e| e.name == name)
}

/// Build the same request body shape `mcp add-json` sends to `POST /mcp/servers`.
fn mcp_install_save_body(
    entry: &McpCatalogEntry,
    scope: &str,
    workspace_root: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "scope": scope,
        "workspaceRoot": workspace_root.unwrap_or(""),
        "name": entry.name,
        "config": {
            "type": "stdio",
            "command": entry.command,
            "args": entry.args,
            "env": mcp_entry_env(entry),
        },
    })
}

fn cli_mcp_catalog(json: bool) -> i32 {
    let value = mcp_catalog_json();
    if json {
        print_json_value(value);
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    }
    0
}

async fn cli_mcp_install(name: &str, scope: &str, workspace_root: Option<&str>) -> i32 {
    match find_mcp_catalog_entry(name) {
        Some(entry) => {
            let body = mcp_install_save_body(entry, scope, workspace_root);
            daemon_post_pretty_json("/mcp/servers", body, "mcp install").await
        }
        None => {
            let known: Vec<&str> = MCP_CATALOG.iter().map(|e| e.name).collect();
            eprintln!(
                "Unknown catalog entry '{name}'. Known entries: {}",
                known.join(", ")
            );
            1
        }
    }
}
