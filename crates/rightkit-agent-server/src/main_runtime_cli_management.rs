async fn cli_info(json: bool) -> i32 {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let mut body = serde_json::json!({
        "schema_version": crate::host_identity::schema("cli.info.v1"),
        "version": crate::host_identity::version(),
        "cwd": cwd,
        "daemon": {
            "running": false,
        },
    });

    if let Some((base_url, token)) = resolve_daemon() {
        body["daemon"] = serde_json::json!({
            "detected": true,
            "running": false,
            "baseUrl": base_url,
        });
        match rightkit_agent_daemon::http_client::daemon_get(&format!("{base_url}/health"), &token)
            .await
        {
            Ok(resp) => {
                let status = resp.status;
                body["daemon"]["healthStatus"] = serde_json::json!(status);
                body["daemon"]["running"] =
                    serde_json::json!(rightkit_agent_daemon::http_client::is_success(status));
                if rightkit_agent_daemon::http_client::is_success(status) {
                    if let Ok(health) = resp.json::<serde_json::Value>() {
                        body["daemon"]["health"] = health;
                    }
                }
            }
            Err(e) => {
                body["daemon"]["error"] = serde_json::json!(e);
            }
        }
    }

    if json {
        print_json_value(body);
    } else {
        println!(
            "{} {}",
            rightkit_agent_config::app_name(),
            crate::host_identity::version()
        );
        println!("cwd: {}", body["cwd"].as_str().unwrap_or("."));
        if body["daemon"]["running"].as_bool().unwrap_or(false) {
            println!(
                "daemon: running at {}",
                body["daemon"]["baseUrl"].as_str().unwrap_or("unknown")
            );
            if let Some(status) = body["daemon"]["healthStatus"].as_u64() {
                println!("health: HTTP {status}");
            }
        } else {
            println!("daemon: not running");
        }
    }
    0
}

// ── Pure helpers (extracted for testability) ──────────────────────────────────

fn mcp_test_probe_format(name: &str, value: &serde_json::Value) -> (String, i32) {
    let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let tool_count = value.get("toolCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let latency = value.get("latencyMs").and_then(|v| v.as_i64()).unwrap_or(0);
    if ok {
        (
            format!("PASS | {name} | {tool_count} tool(s) | {latency}ms"),
            0,
        )
    } else {
        let error = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        (format!("FAIL | {name} | {error}"), 1)
    }
}

fn mcp_login_result(value: &serde_json::Value) -> (String, i32) {
    let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let message = value.get("message").and_then(|v| v.as_str()).unwrap_or("");
    if ok {
        (message.to_string(), 0)
    } else {
        let error = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        (error.to_string(), 1)
    }
}

async fn cli_app(command: AppCommand) -> i32 {
    match command {
        AppCommand::Handoff { session_id } => daemon_handoff(&session_id).await,
        AppCommand::Open { session_id } => daemon_app_open(&session_id).await,
    }
}

async fn cli_mcp(command: McpCommand) -> i32 {
    match command {
        McpCommand::List {
            workspace_root,
            json: _json,
        } => {
            let path = format!(
                "/mcp{}",
                mcp_management_query(None, workspace_root.as_deref())
            );
            daemon_get_pretty_json(&path, "mcp servers").await
        }
        McpCommand::Status => {
            // `/mcp/server/status` exists only where the host runs a stand-alone MCP server.
            if !crate::host_options::mcp_public_server() {
                eprintln!(
                    "{}: `mcp status` is not available: this build runs no stand-alone MCP server",
                    crate::host_identity::app()
                );
                return 2;
            }
            daemon_get_pretty_json("/mcp/server/status", "mcp status").await
        }
        McpCommand::Get {
            name,
            workspace_root,
        } => daemon_mcp_get(&name, workspace_root.as_deref()).await,
        McpCommand::AddJson { path } => match read_json_file(&path) {
            Ok(body) => daemon_post_pretty_json("/mcp/servers", body, "mcp add").await,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        McpCommand::Import {
            path,
            scope,
            workspace_root,
        } => cli_mcp_import(&path, &scope, workspace_root.as_deref()).await,
        McpCommand::Remove {
            name,
            scope,
            workspace_root,
        } => {
            let query = mcp_management_query(Some(&scope), workspace_root.as_deref());
            daemon_delete_pretty_json(
                &format!("/mcp/servers/{}{}", url_encode_component(&name), query),
                "mcp remove",
            )
            .await
        }
        McpCommand::Probe {
            name,
            scope,
            workspace_root,
        } => {
            let query = mcp_management_query(Some(&scope), workspace_root.as_deref());
            daemon_post_pretty_json(
                &format!("/mcp/servers/{}/probe{query}", url_encode_component(&name)),
                serde_json::json!({}),
                "mcp probe",
            )
            .await
        }
        McpCommand::Test {
            name,
            scope,
            workspace_root,
            json,
        } => {
            let query = mcp_management_query(Some(&scope), workspace_root.as_deref());
            let result = daemon_post_raw(
                &format!("/mcp/servers/{}/probe{query}", url_encode_component(&name)),
                serde_json::json!({}),
            )
            .await;
            match result {
                Ok(value) if json => {
                    print_json_value(value);
                    0
                }
                Ok(value) => {
                    let (line, code) = mcp_test_probe_format(&name, &value);
                    println!("{line}");
                    code
                }
                Err(e) => daemon_error_exit(json, &e),
            }
        }
        McpCommand::Login {
            name,
            scope,
            workspace_root,
        } => {
            let query = mcp_management_query(Some(&scope), workspace_root.as_deref());
            let result = daemon_post_raw(
                &format!("/mcp/servers/{}/login{query}", url_encode_component(&name)),
                serde_json::json!({}),
            )
            .await;
            match result {
                Ok(value) => {
                    let (msg, code) = mcp_login_result(&value);
                    if code == 0 {
                        println!("{msg}");
                    } else {
                        eprintln!("{msg}");
                    }
                    code
                }
                Err(e) => daemon_error_exit(false, &e),
            }
        }
        McpCommand::Approvals { workspace_root } => {
            let query = mcp_management_query(None, Some(&workspace_root));
            daemon_get_pretty_json(&format!("/mcp/approvals{query}"), "mcp approvals").await
        }
        McpCommand::Approve {
            name,
            workspace_root,
        } => {
            let query = mcp_management_query(None, Some(&workspace_root));
            daemon_post_pretty_json(
                &format!(
                    "/mcp/servers/{}/approve{query}",
                    url_encode_component(&name)
                ),
                serde_json::json!({}),
                "mcp approve",
            )
            .await
        }
        McpCommand::Catalog { json } => cli_mcp_catalog(json),
        McpCommand::Install {
            name,
            scope,
            workspace_root,
        } => cli_mcp_install(&name, &scope, workspace_root.as_deref()).await,
    }
}

async fn cli_plugin(command: PluginCommand) -> i32 {
    match command {
        PluginCommand::List { json: _json } => daemon_get_pretty_json("/plugins", "plugins").await,
        PluginCommand::Marketplace => {
            daemon_get_pretty_json("/plugins/marketplace", "plugin marketplace").await
        }
        PluginCommand::InstallMarketplace {
            id,
            expected_sha256,
            overwrite,
            allow_file_urls,
        } => {
            let body = serde_json::json!({
                "trustAccepted": true,
                "expectedSha256": expected_sha256,
                "overwrite": overwrite,
                "allowFileUrls": allow_file_urls,
                "fileUrlTrustAccepted": allow_file_urls,
            });
            daemon_post_pretty_json(
                &format!("/plugins/marketplace/{}/install", url_encode_component(&id)),
                body,
                "plugin marketplace install",
            )
            .await
        }
        PluginCommand::MarketplaceSync {
            catalog_url,
            expected_sha256,
            allow_unsigned,
            allow_file_urls,
        } => {
            let body = serde_json::json!({
                "catalogUrl": catalog_url,
                "expectedSha256": expected_sha256,
                "trustAccepted": true,
                "requireSigned": !allow_unsigned,
                "allowFileUrls": allow_file_urls,
                "fileUrlTrustAccepted": allow_file_urls,
            });
            daemon_post_pretty_json("/plugins/marketplace/sync", body, "plugin marketplace sync")
                .await
        }
        PluginCommand::Reload => {
            daemon_post_pretty_json("/plugins/reload", serde_json::json!({}), "plugin reload").await
        }
        PluginCommand::Install {
            source_path,
            overwrite,
        } => {
            // A git source is cloned (shallow) to a scratch folder under the
            // state root, installed through the same trusted path as a local
            // folder, and the clone removed (mcp D-parity-2).
            // An `npm:<spec>` source is packed with `npm pack --ignore-scripts`
            // (no install scripts run) and unpacked the same way.
            let fetched = if let Some(spec) = source_path.strip_prefix("npm:") {
                Some(fetch_plugin_npm_source(spec))
            } else {
                plugin_git_source(&source_path)
                    .map(|(url, git_ref)| clone_plugin_git_source(&url, git_ref.as_deref()))
            };
            let clone = match fetched {
                Some(Ok(dir)) => Some(dir),
                Some(Err(message)) => {
                    eprintln!("error: plugin install: {message}");
                    return 1;
                }
                None => None,
            };
            let source = clone
                .as_ref()
                .map(|(_, tree)| tree.display().to_string())
                .unwrap_or(source_path);
            let body = serde_json::json!({
                "sourcePath": source,
                "trustAccepted": true,
                "overwrite": overwrite,
            });
            let code = daemon_post_pretty_json("/plugins/install", body, "plugin install").await;
            if let Some((dir, _)) = clone {
                let _ = std::fs::remove_dir_all(dir);
            }
            code
        }
        PluginCommand::Remove { id } => {
            daemon_delete_pretty_json(
                &format!("/plugins/{}", url_encode_component(&id)),
                "plugin remove",
            )
            .await
        }
        PluginCommand::Enable { id } => {
            daemon_post_pretty_json(
                &format!("/plugins/{}/enable", url_encode_component(&id)),
                serde_json::json!({}),
                "plugin enable",
            )
            .await
        }
        PluginCommand::Disable { id } => {
            daemon_post_pretty_json(
                &format!("/plugins/{}/disable", url_encode_component(&id)),
                serde_json::json!({}),
                "plugin disable",
            )
            .await
        }
    }
}

/// A git plugin source: `git:<url>`, `github:owner/repo`, `git@host:path`, or
/// an `https://` / `ssh://` URL ending in `.git`, each with an optional
/// `#ref` (branch or tag). Anything else is a local folder.
fn plugin_git_source(spec: &str) -> Option<(String, Option<String>)> {
    let (base, git_ref) = match spec.rsplit_once('#') {
        Some((base, git_ref)) if !git_ref.is_empty() => (base, Some(git_ref.to_string())),
        _ => (spec, None),
    };
    let url = if let Some(url) = base.strip_prefix("git:") {
        url.to_string()
    } else if let Some(repo) = base.strip_prefix("github:") {
        format!("https://github.com/{repo}.git")
    } else if base.starts_with("git@")
        || ((base.starts_with("https://") || base.starts_with("ssh://")) && base.ends_with(".git"))
    {
        base.to_string()
    } else {
        return None;
    };
    (!url.is_empty()).then_some((url, git_ref))
}

/// A fresh scratch folder under `<state root>/plugin-sources/`.
fn plugin_scratch_dir() -> Result<std::path::PathBuf, String> {
    let root = rightkit_agent_config::state_root()
        .ok_or_else(|| "no state root to fetch into".to_string())?
        .join("plugin-sources");
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("create {}: {error}", root.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    Ok(root.join(format!("fetch-{}-{stamp}", std::process::id())))
}

/// Pack an npm plugin package (a registry name with an optional version, or
/// any spec `npm pack` takes) without running its scripts, and unpack it.
/// Returns (scratch folder to remove, plugin tree).
fn fetch_plugin_npm_source(spec: &str) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    if spec.is_empty() {
        return Err("npm: needs a package spec, for example npm:my-plugin@1.2.0".to_string());
    }
    let dir = plugin_scratch_dir()?;
    std::fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let fail = |message: String| {
        let _ = std::fs::remove_dir_all(&dir);
        Err(message)
    };
    let output = match rightkit_child_process::command_std("npm")
        .args(["pack", "--silent", "--ignore-scripts", "--pack-destination"])
        .arg(&dir)
        .arg("--")
        .arg(spec)
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(error) => return fail(format!("npm is not available: {error}")),
    };
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return fail(format!("npm pack failed: {}", detail.trim()));
    }
    let Some(tarball) = String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.ends_with(".tgz"))
        .map(|name| dir.join(name))
    else {
        return fail("npm pack printed no tarball name".to_string());
    };
    let unpack = rightkit_child_process::command_std("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&dir)
        .stdin(std::process::Stdio::null())
        .output();
    match unpack {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let detail = String::from_utf8_lossy(&out.stderr);
            return fail(format!(
                "unpacking the npm package failed: {}",
                detail.trim()
            ));
        }
        Err(error) => return fail(format!("tar is not available: {error}")),
    }
    let tree = dir.join("package");
    if !tree.is_dir() {
        return fail("the npm package has no package/ folder".to_string());
    }
    Ok((dir, tree))
}

/// A `#ref` that names a commit: 7 to 40 hexadecimal digits. A branch or tag
/// that is spelled like that cannot be installed by name; pin its commit.
fn is_commit_sha_ref(git_ref: &str) -> bool {
    (7..=40).contains(&git_ref.len()) && git_ref.chars().all(|c| c.is_ascii_hexdigit())
}

/// Clone a plugin source without prompting for credentials: shallow for a
/// branch, a tag or the default branch; a full clone checked out at the commit
/// for a commit sha (`#<sha>`), after which HEAD is verified to be that commit.
/// Returns (scratch folder to remove, plugin tree).
fn clone_plugin_git_source(
    url: &str,
    git_ref: Option<&str>,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let dir = plugin_scratch_dir()?;
    let pinned = git_ref.filter(|git_ref| is_commit_sha_ref(git_ref));
    let mut command = rightkit_child_process::command_std("git");
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["clone", "--quiet"]);
    match (pinned, git_ref) {
        // A commit is not a branch: take the history, then check the commit out.
        (Some(_), _) => {}
        (None, Some(git_ref)) => {
            command.args(["--depth", "1", "--branch", git_ref]);
        }
        (None, None) => {
            command.args(["--depth", "1"]);
        }
    }
    let output = command
        .arg("--")
        .arg(url)
        .arg(&dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("git is not available: {error}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git clone failed: {}", detail.trim()));
    }
    if let Some(sha) = pinned {
        let git = |args: &[&str]| {
            rightkit_child_process::command_std("git")
                .env("GIT_TERMINAL_PROMPT", "0")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .stdin(std::process::Stdio::null())
                .output()
        };
        let checked_out = git(&["checkout", "--quiet", "--detach", sha])
            .map_err(|error| format!("git is not available: {error}"))?;
        let head = git(&["rev-parse", "HEAD"])
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .to_ascii_lowercase()
            });
        let verified = checked_out.status.success()
            && head
                .as_deref()
                .is_some_and(|head| head.starts_with(&sha.to_ascii_lowercase()));
        if !verified {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!(
                "commit {sha} is not in {url}; nothing was installed"
            ));
        }
        eprintln!(
            "[plugin] pinned to commit {}",
            head.unwrap_or_else(|| sha.to_string())
        );
    }
    let _ = std::fs::remove_dir_all(dir.join(".git"));
    Ok((dir.clone(), dir))
}

/// Biggest MCP config file `mcp import` reads.
const MCP_IMPORT_MAX_BYTES: u64 = 1024 * 1024;

/// `{app} mcp import FILE [--scope S] [--workspace-root DIR]`: save each server of an
/// `mcpServers` file through the daemon's save route (the one `mcp add-json` uses), so the
/// trust gate sees an imported workspace server exactly as it sees a typed one. Only server
/// names and kinds are printed; a server's env and headers can hold credentials.
async fn cli_mcp_import(path: &str, scope: &str, workspace_root: Option<&str>) -> i32 {
    if !matches!(scope, "user" | "workspace") {
        eprintln!("--scope is `user` or `workspace`");
        return 2;
    }
    if scope == "workspace" && workspace_root.is_none_or(|root| root.trim().is_empty()) {
        eprintln!("--scope workspace needs --workspace-root");
        return 2;
    }
    let text = match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MCP_IMPORT_MAX_BYTES => {
            eprintln!("{path} is larger than {MCP_IMPORT_MAX_BYTES} bytes");
            return 1;
        }
        Ok(_) => match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("cannot read {path}: {error}");
                return 1;
            }
        },
        Err(error) => {
            eprintln!("cannot read {path}: {error}");
            return 1;
        }
    };
    let (file, skipped) = match rightkit_mcp::agent_mcp::McpConfigFile::from_json_lenient(&text) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{path} is not an MCP server file: {error}");
            return 1;
        }
    };
    for message in &skipped {
        eprintln!("skipped: {message}");
    }
    let mut servers: Vec<_> = file.mcp_servers.into_iter().collect();
    servers.sort_by(|a, b| a.0.cmp(&b.0));
    if servers.is_empty() {
        eprintln!("{path} holds no MCP server to import");
        return 1;
    }
    let mut failed = false;
    for (name, config) in servers {
        let kind = match &config {
            rightkit_mcp::agent_mcp::McpServerConfig::Http(_) => "http",
            rightkit_mcp::agent_mcp::McpServerConfig::Sse(_) => "sse",
            rightkit_mcp::agent_mcp::McpServerConfig::Stdio(_) => "stdio",
        };
        let body = serde_json::json!({
            "scope": scope,
            "workspaceRoot": workspace_root.unwrap_or(""),
            "name": name,
            "config": config,
        });
        match daemon_post_raw("/mcp/servers", body).await {
            Ok(_) => println!("imported {name} ({kind}, {scope} scope)"),
            Err(error) if error == no_daemon_message() => return host_unreachable_exit(false),
            Err(error) => {
                eprintln!("could not import {name}: {error}");
                failed = true;
            }
        }
    }
    i32::from(failed)
}
