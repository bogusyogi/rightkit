async fn cli_migrate(command: crate::cli::MigrateCommand) -> i32 {
    use crate::cli::MigrateCommand;

    match command {
        MigrateCommand::Status { db } => {
            let report = crate::daemon::migrate::migration_status(&db);
            println!("database: {}", report.db_path.display());
            match report.current_version {
                Some(version) => println!("schema version: {version}"),
                None => println!(
                    "schema version: unavailable (database does not exist or cannot be read)"
                ),
            }
            match report.pending_journal {
                Some(journal) => println!(
                    "pending migration journal: {} -> {} (versions: {:?}, pid: {})",
                    journal.from_version, journal.to_version, journal.pending_versions, journal.pid
                ),
                None => println!("pending migration journal: none"),
            }
            0
        }
        MigrateCommand::Apply { db } => match crate::daemon::migrate::apply_migrations(&db) {
            Ok(report) => {
                println!("database: {db}");
                println!("migrations applied: {}", report.applied_migrations);
                match report.resumed_from_crash {
                    Some(journal) => println!(
                        "resumed crash journal: {} -> {} (versions: {:?}, pid: {})",
                        journal.from_version,
                        journal.to_version,
                        journal.pending_versions,
                        journal.pid
                    ),
                    None => println!("resumed crash journal: no"),
                }
                0
            }
            Err(error) => {
                eprintln!("migration apply failed for {db}: {error}");
                1
            }
        },
    }
}

async fn cli_schedule(command: ScheduleCommand) -> i32 {
    match command {
        ScheduleCommand::Create {
            payload,
            workspace_root,
        } => match read_json_file(&payload) {
            Ok(body) => {
                daemon_post_pretty_json(
                    &format!(
                        "/schedules{}",
                        workspace_root_query(workspace_root.as_deref())
                    ),
                    body,
                    "schedule create",
                )
                .await
            }
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        ScheduleCommand::Show { id, workspace_root } => match daemon_get_json_value(
            &format!(
                "/schedules{}",
                workspace_root_query(workspace_root.as_deref())
            ),
            "schedule show",
        )
        .await
        {
            Ok(value) => match value
                .get("schedules")
                .and_then(serde_json::Value::as_array)
                .and_then(|rows| {
                    rows.iter().find(|row| {
                        row.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())
                    })
                }) {
                Some(row) => {
                    print_json_value(row.clone());
                    0
                }
                None => {
                    eprintln!("No schedule named {id}.");
                    1
                }
            },
            Err(code) => code,
        },
        ScheduleCommand::Edit {
            id,
            payload,
            workspace_root,
        } => match read_json_file(&payload) {
            Ok(body) => {
                daemon_patch_pretty_json(
                    &format!(
                        "/schedules/{}{}",
                        url_encode_component(&id),
                        workspace_root_query(workspace_root.as_deref())
                    ),
                    body,
                    "schedule edit",
                )
                .await
            }
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        ScheduleCommand::Delete { id, workspace_root } => {
            daemon_delete_pretty_json(
                &format!(
                    "/schedules/{}{}",
                    url_encode_component(&id),
                    workspace_root_query(workspace_root.as_deref())
                ),
                "schedule delete",
            )
            .await
        }
        ScheduleCommand::List {
            workspace_root,
            json: _json,
        } => {
            daemon_get_pretty_json(
                &format!(
                    "/schedules{}",
                    workspace_root_query(workspace_root.as_deref())
                ),
                "schedules",
            )
            .await
        }
        ScheduleCommand::Runs { id, workspace_root } => {
            daemon_get_pretty_json(
                &format!(
                    "/schedules/{}/runs{}",
                    url_encode_component(&id),
                    workspace_root_query(workspace_root.as_deref())
                ),
                "schedule runs",
            )
            .await
        }
        ScheduleCommand::Audit { workspace_root } => {
            daemon_get_pretty_json(
                &format!(
                    "/schedules/audit{}",
                    workspace_root_query(workspace_root.as_deref())
                ),
                "schedule audit",
            )
            .await
        }
        ScheduleCommand::Pause { id, workspace_root } => {
            daemon_post_pretty_json(
                &format!(
                    "/schedules/{}/pause{}",
                    url_encode_component(&id),
                    workspace_root_query(workspace_root.as_deref())
                ),
                serde_json::json!({}),
                "schedule pause",
            )
            .await
        }
        ScheduleCommand::Resume { id, workspace_root } => {
            daemon_post_pretty_json(
                &format!(
                    "/schedules/{}/resume{}",
                    url_encode_component(&id),
                    workspace_root_query(workspace_root.as_deref())
                ),
                serde_json::json!({}),
                "schedule resume",
            )
            .await
        }
        ScheduleCommand::Run { id, workspace_root } => {
            daemon_post_pretty_json(
                &format!(
                    "/schedules/{}/run{}",
                    url_encode_component(&id),
                    workspace_root_query(workspace_root.as_deref())
                ),
                serde_json::json!({}),
                "schedule run",
            )
            .await
        }
    }
}

async fn daemon_patch_pretty_json(path: &str, body: serde_json::Value, label: &str) -> i32 {
    daemon_json_request(rightkit_http::Method::Patch, path, Some(body), label).await
}

async fn cli_browser(command: crate::cli::BrowserCommand) -> i32 {
    use crate::cli::BrowserCommand;
    let workspace = |session_id: &str| {
        format!(
            "/sessions/{}/browser/workspace",
            url_encode_component(session_id)
        )
    };
    match command {
        BrowserCommand::Status { session_id } => {
            daemon_get_pretty_json(&workspace(&session_id), "browser status").await
        }
        BrowserCommand::Open {
            session_id,
            url,
            headless: _,
            visible,
        } => {
            let opened = daemon_post_pretty_json(
                &format!("{}/open", workspace(&session_id)),
                serde_json::json!({"url": url}),
                "browser open",
            )
            .await;
            // The daemon's open contract creates one isolated headless
            // BrowserSession. Visible mode is a lease transition on that same
            // session truth, never a second WebView/browser/profile.
            if opened == 0 && visible {
                daemon_post_pretty_json(
                    &format!("{}/take-control", workspace(&session_id)),
                    serde_json::json!({}),
                    "browser open --visible",
                )
                .await
            } else {
                opened
            }
        }
        BrowserCommand::Inspect { session_id } => {
            daemon_get_pretty_json(&workspace(&session_id), "browser inspect").await
        }
        BrowserCommand::Act {
            session_id,
            browser_session_id,
            action,
            url,
        } => {
            let base = workspace(&session_id);
            match action.as_str() {
                "navigate" => {
                    match url {
                        Some(url) => daemon_post_pretty_json(
                            &format!("{base}/navigate"),
                            serde_json::json!({"browserSessionId": browser_session_id, "url": url}),
                            "browser act navigate",
                        )
                        .await,
                        None => {
                            eprintln!("browser act navigate requires --url");
                            2
                        }
                    }
                }
                "reload" | "back" | "forward" => {
                    daemon_post_pretty_json(
                        &format!("{base}/{action}"),
                        serde_json::json!({"sessionId": browser_session_id}),
                        "browser act",
                    )
                    .await
                }
                _ => {
                    eprintln!("Unsupported browser action '{action}'. Supported: navigate, reload, back, forward.");
                    2
                }
            }
        }
        BrowserCommand::Navigate {
            session_id,
            browser_session_id,
            url,
        } => {
            daemon_post_pretty_json(
                &format!("{}/navigate", workspace(&session_id)),
                serde_json::json!({"browserSessionId": browser_session_id, "url": url}),
                "browser navigate",
            )
            .await
        }
        BrowserCommand::Screenshot {
            session_id,
            browser_session_id,
        } => {
            daemon_post_pretty_json(
                &format!("{}/screenshot", workspace(&session_id)),
                serde_json::json!({"sessionId": browser_session_id}),
                "browser screenshot",
            )
            .await
        }
        BrowserCommand::Take { session_id } => {
            daemon_post_pretty_json(
                &format!("{}/take-control", workspace(&session_id)),
                serde_json::json!({}),
                "browser take control",
            )
            .await
        }
        BrowserCommand::Release { session_id } => {
            daemon_post_pretty_json(
                &format!("{}/release-control", workspace(&session_id)),
                serde_json::json!({}),
                "browser release control",
            )
            .await
        }
        BrowserCommand::Close {
            session_id,
            browser_session_id,
        } => {
            daemon_post_pretty_json(
                &format!("{}/close", workspace(&session_id)),
                serde_json::json!({"sessionId": browser_session_id}),
                "browser close",
            )
            .await
        }
    }
}

async fn cli_bot(command: crate::cli::BotCommand) -> i32 {
    use crate::cli::BotCommand;
    match command {
        BotCommand::List => daemon_get_pretty_json("/bots", "bots list").await,
        BotCommand::Show { id } => {
            daemon_get_pretty_json(&format!("/bots/{}", url_encode_component(&id)), "bot show")
                .await
        }
        BotCommand::Create { profile } => match read_json_file(&profile) {
            Ok(body) => daemon_post_pretty_json("/bots", body, "bot create").await,
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        BotCommand::Edit { id, profile } => match read_json_file(&profile) {
            Ok(body) => {
                daemon_patch_pretty_json(
                    &format!("/bots/{}", url_encode_component(&id)),
                    body,
                    "bot edit",
                )
                .await
            }
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        BotCommand::Import { package } => match read_json_file(&package) {
            Ok(body) => daemon_post_pretty_json("/bots/import", body, "bot import").await,
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        BotCommand::Export { id } => {
            daemon_get_pretty_json(
                &format!("/bots/{}/export", url_encode_component(&id)),
                "bot export",
            )
            .await
        }
        BotCommand::Run { id, workspace_root } => {
            daemon_post_pretty_json(
                &format!(
                    "/bots/{}/run{}",
                    url_encode_component(&id),
                    workspace_root_query(workspace_root.as_deref())
                ),
                serde_json::json!({}),
                "bot run",
            )
            .await
        }
        BotCommand::Pause { id } => set_bot_enabled(&id, false).await,
        BotCommand::Resume { id } => set_bot_enabled(&id, true).await,
    }
}

/// Preserve every profile field while only changing execution enablement. This
/// deliberately reads from daemon storage first rather than synthesizing a
/// partial profile or creating a surface-local pause store.
async fn set_bot_enabled(id: &str, enabled: bool) -> i32 {
    let path = format!("/bots/{}", url_encode_component(id));
    let mut profile = match daemon_get_json_value(&path, "bot profile").await {
        Ok(value) => value,
        Err(code) => return code,
    };
    let Some(object) = profile.as_object_mut() else {
        eprintln!("Daemon returned an invalid Bot profile for {id}.");
        return 1;
    };
    object.insert("enabled".to_string(), serde_json::Value::Bool(enabled));
    daemon_patch_pretty_json(
        &path,
        profile,
        if enabled { "bot resume" } else { "bot pause" },
    )
    .await
}

async fn cli_permission(command: PermissionCommand) -> i32 {
    match command {
        PermissionCommand::Mode { session_id, mode } => {
            daemon_post_pretty_json(
                &format!(
                    "/sessions/{}/permission-mode",
                    url_encode_component(&session_id)
                ),
                serde_json::json!({ "mode": mode }),
                "permission mode",
            )
            .await
        }
        PermissionCommand::Rules { session_id } => {
            daemon_get_pretty_json(
                &format!(
                    "/sessions/{}/permission-rules",
                    url_encode_component(&session_id)
                ),
                "permission rules",
            )
            .await
        }
        PermissionCommand::RuleAdd {
            session_id,
            effect,
            pattern,
        } => {
            daemon_post_pretty_json(
                &format!(
                    "/sessions/{}/permission-rules",
                    url_encode_component(&session_id)
                ),
                serde_json::json!({
                    "effect": effect,
                    "pattern": pattern,
                }),
                "permission rule add",
            )
            .await
        }
        PermissionCommand::RuleRemove { session_id, index } => {
            daemon_delete_pretty_json(
                &format!(
                    "/sessions/{}/permission-rules/{}",
                    url_encode_component(&session_id),
                    index
                ),
                "permission rule remove",
            )
            .await
        }
    }
}

/// `coderight provider ...`: every verb runs in-process through
/// `provider_setup` / `api_provider_auth` — the same functions the
/// `/providers` screen calls — so none of them needs a running daemon.
/// `set-visibility` writes the one shared `model-visibility.json`.
async fn cli_provider(command: ProviderCommand) -> i32 {
    use provider_setup::{screen_provider, vendor_facts};
    use rightkit_terminal_ui::nav::ProviderKind;
    let kind_of = |provider: &str| screen_provider(provider).map(|screen| screen.kind);
    match command {
        ProviderCommand::Status { provider } => {
            let status = match kind_of(&provider) {
                Some(ProviderKind::OAuth) => crate::api_provider_auth::terminal_status(&provider)
                    .await
                    .map_err(|error| format!("provider status failed: {error}")),
                Some(ProviderKind::VendorCli) => {
                    let facts = vendor_facts(&provider);
                    Ok(serde_json::json!({
                        "provider": provider,
                        "status": provider_setup::project_status(
                            ProviderKind::VendorCli, false, false, Some(&facts)),
                        "installed": facts.installed,
                        "version": facts.version,
                        "signedIn": facts.signed_in,
                    }))
                }
                _ => {
                    let key_set = provider_setup::key_is_set(&provider);
                    Ok(serde_json::json!({
                        "provider": provider,
                        "status": provider_setup::project_status(
                            ProviderKind::ApiKey, key_set, false, None),
                        "keySet": key_set,
                    }))
                }
            };
            match status {
                Ok(value) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&value).unwrap_or_default()
                    );
                    0
                }
                Err(error) => {
                    eprintln!("{error}");
                    1
                }
            }
        }
        ProviderCommand::Login { provider } => match kind_of(&provider) {
            Some(ProviderKind::OAuth) => {
                match crate::api_provider_auth::terminal_login(&provider).await {
                    Ok(()) => match crate::main_runtime::onboarding::ConfigPersister::persist_native_subscription_route(&provider) {
                        Ok(()) => {
                            println!("OpenAI subscription login succeeded; route saved.");
                            0
                        }
                        Err(error) => {
                            eprintln!("OpenAI login succeeded, but route was not saved: {error}");
                            1
                        }
                    },
                    Err(error) => {
                        eprintln!("OpenAI login failed: {error}");
                        1
                    }
                }
            }
            Some(ProviderKind::VendorCli) => {
                // The vendor CLI authenticates itself; we only say how.
                let (install, login) = provider_setup::vendor_hints(&provider).unwrap_or_default();
                if vendor_facts(&provider).installed {
                    println!("{login}");
                    0
                } else {
                    eprintln!("Not installed. {install}");
                    1
                }
            }
            _ => {
                eprintln!(
                    "{provider} uses an API key: run `coderight provider connect {provider}`."
                );
                2
            }
        },
        ProviderCommand::Logout { provider } => match kind_of(&provider) {
            Some(ProviderKind::OAuth) => {
                match crate::api_provider_auth::terminal_logout(&provider).await {
                    Ok(()) => {
                        println!("Signed out of {provider}.");
                        0
                    }
                    Err(error) => {
                        eprintln!("provider logout failed: {error}");
                        1
                    }
                }
            }
            Some(ProviderKind::VendorCli) => {
                eprintln!("{provider} signs in and out through its own CLI, not RightKit.");
                2
            }
            _ => cli_provider_key_remove(&provider),
        },
        ProviderCommand::Connect { provider } | ProviderCommand::Rotate { provider } => {
            // The store holds one key per provider, so writing the
            // replacement is the rotation (never delete-then-write).
            cli_provider_key_write(provider).await
        }
        ProviderCommand::Disconnect { provider } => cli_provider_key_remove(&provider),
        ProviderCommand::Test { provider } => {
            match provider_setup::test_connection(&provider).await {
                Ok(message) => {
                    println!("{message}");
                    0
                }
                Err(error) => {
                    eprintln!("provider test failed: {error}");
                    1
                }
            }
        }
        ProviderCommand::Models { provider } => match kind_of(&provider) {
            Some(ProviderKind::VendorCli) => {
                let facts = vendor_facts(&provider);
                if !facts.installed {
                    let (install, _) = provider_setup::vendor_hints(&provider).unwrap_or_default();
                    eprintln!("Not installed. {install}");
                    return 1;
                }
                for (id, display) in provider_setup::vendor_models(&provider) {
                    println!("{id}\t{display}");
                }
                0
            }
            _ => interactive_session::cli_models(Some(provider)).await,
        },
        ProviderCommand::SetVisibility {
            provider,
            model,
            visible,
        } => {
            // The same function `/providers` calls: one store,
            // ~/.coderight/state/model-visibility.json.
            match set_model_hidden(&provider, &model, !visible) {
                Ok(()) => {
                    println!(
                        "{provider}/{model} is now {} in /model.",
                        if visible { "shown" } else { "hidden" }
                    );
                    0
                }
                Err(error) => {
                    eprintln!("provider set-visibility failed: {error}");
                    1
                }
            }
        }
    }
}

fn cli_provider_key_remove(provider: &str) -> i32 {
    match provider_setup::remove_key(provider) {
        Ok(()) => {
            println!("Key removed for {provider}.");
            0
        }
        Err(error) => {
            eprintln!("provider disconnect failed: {error}");
            1
        }
    }
}

async fn cli_provider_key_write(provider: String) -> i32 {
    if matches!(
        provider_setup::screen_provider(&provider).map(|screen| screen.kind),
        Some(
            rightkit_terminal_ui::nav::ProviderKind::OAuth
                | rightkit_terminal_ui::nav::ProviderKind::VendorCli
        )
    ) {
        eprintln!("{provider} does not use an API key; use `coderight provider login {provider}`.");
        return 2;
    }
    let secret = match read_secret_line("Provider API key") {
        Ok(secret) => secret,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    match provider_setup::store_key(&provider, &secret) {
        Ok(()) => {
            println!("Key saved for {provider}.");
            0
        }
        Err(error) => {
            eprintln!("provider connect failed: {error}");
            1
        }
    }
}

async fn daemon_put_pretty_json(path: &str, body: serde_json::Value, label: &str) -> i32 {
    daemon_json_request(rightkit_http::Method::Put, path, Some(body), label).await
}

async fn cli_local_model(command: LocalModelCommand) -> i32 {
    match command {
        LocalModelCommand::Hardware => {
            daemon_get_pretty_json("/local-models/hardware", "local model hardware").await
        }
        LocalModelCommand::Catalog => {
            daemon_get_pretty_json("/local-models/catalog", "local model catalog").await
        }
    }
}

async fn cli_hooks(command: HooksCommand) -> i32 {
    match command {
        HooksCommand::Status => daemon_get_pretty_json("/hooks/status", "hooks status").await,
        HooksCommand::List => daemon_get_pretty_json("/hooks/list", "hooks list").await,
        HooksCommand::Test { event, matcher } => {
            let body = serde_json::json!({
                "event": event,
                "matcher": matcher,
            });
            daemon_post_pretty_json("/hooks/test", body, "hooks test").await
        }
        HooksCommand::Revoke { id, restore } => {
            let body = serde_json::json!({
                "id": id,
                "restore": restore,
            });
            daemon_post_pretty_json("/hooks/revoke", body, "hooks revoke").await
        }
    }
}

async fn cli_toolset(command: ToolsetCommand) -> i32 {
    match command {
        ToolsetCommand::List { json: _json } => {
            daemon_get_pretty_json("/toolsets", "toolsets").await
        }
        ToolsetCommand::Gateways { json: _json } => {
            daemon_get_pretty_json("/toolsets/gateways", "toolset gateways").await
        }
    }
}

async fn cli_execution(command: ExecutionCommand) -> i32 {
    match command {
        ExecutionCommand::Backends {
            workspace_root,
            json: _json,
        } => {
            daemon_get_pretty_json(
                &format!(
                    "/execution/backends{}",
                    workspace_root_query(workspace_root.as_deref())
                ),
                "execution backends",
            )
            .await
        }
        ExecutionCommand::Doctor {
            backend_id,
            workspace_root,
            json: _json,
        } => {
            daemon_post_pretty_json(
                &format!(
                    "/execution/backends/{}/doctor{}",
                    url_encode_component(&backend_id),
                    workspace_root_query(workspace_root.as_deref())
                ),
                serde_json::json!({}),
                "execution doctor",
            )
            .await
        }
    }
}

async fn cli_runtime(command: CliRuntimeCommand) -> i32 {
    match command {
        CliRuntimeCommand::MatrixCell {
            provider,
            scenario,
            workspace,
            child_provider,
            json: _,
        } => {
            let (base_url, token) = match crate::cli_presence::resolve_daemon_or_autostart().await {
                Some(value) => value,
                None => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "provider": provider, "scenario": scenario, "passed": false,
                            "diagnostic": "no running daemon; matrix cells require installed daemon runtime"
                        })
                    );
                    return 1;
                }
            };
            let response = rightkit_http::http_client::daemon_call(
                rightkit_http::Method::Post,
                &format!("{base_url}/cli-runtime/matrix-cell"),
                &token,
                Some(&serde_json::json!({
                    "provider": provider, "scenario": scenario, "workspace": workspace,
                    "childProvider": child_provider,
                })),
            )
            .await;
            match response {
                Ok(response) => {
                    let success = rightkit_http::http_client::is_success(response.status);
                    let value = response.json::<serde_json::Value>().unwrap_or_else(|_| {
                        serde_json::json!({
                            "provider": provider, "scenario": scenario, "passed": false,
                            "diagnostic": "daemon returned non-JSON matrix receipt"
                        })
                    });
                    println!("{}", value);
                    if success
                        && value.get("passed").and_then(serde_json::Value::as_bool) == Some(true)
                    {
                        0
                    } else {
                        1
                    }
                }
                Err(error) => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "provider": provider, "scenario": scenario, "passed": false,
                            "diagnostic": format!("daemon matrix request failed: {error}")
                        })
                    );
                    1
                }
            }
        }
    }
}

fn workspace_root_query(workspace_root: Option<&str>) -> String {
    let root = workspace_root.map(str::to_string).unwrap_or_else(|| {
        std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });
    format!("?workspaceRoot={}", url_encode_component(&root))
}

async fn daemon_mcp_get(name: &str, workspace_root: Option<&str>) -> i32 {
    let path = format!("/mcp{}", mcp_management_query(None, workspace_root));
    let value = match daemon_get_json_value(&path, "mcp servers").await {
        Ok(value) => value,
        Err(code) => return code,
    };
    let matches = value["servers"]
        .as_array()
        .map(|servers| {
            servers
                .iter()
                .filter(|server| server["name"].as_str() == Some(name))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if matches.is_empty() {
        eprintln!("No MCP server named {name}.");
        return 1;
    }
    if matches.len() == 1 {
        print_json_value(
            matches
                .into_iter()
                .next()
                .unwrap_or(serde_json::Value::Null),
        );
    } else {
        print_json_value(serde_json::json!({ "servers": matches }));
    }
    0
}

fn mcp_management_query(scope: Option<&str>, workspace_root: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(scope) = scope.filter(|s| !s.trim().is_empty()) {
        parts.push(format!("scope={}", url_encode_component(scope)));
    }
    if let Some(workspace_root) = workspace_root.filter(|s| !s.trim().is_empty()) {
        parts.push(format!(
            "workspaceRoot={}",
            url_encode_component(workspace_root)
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// Renders `/help` (and CLI `coderight help`) from [`crate::cli::command_registry`].
/// This is the one place help text is generated — grouping is Common vs
/// Advanced (`--all` includes Advanced) exactly as the registry declares it,
/// so a command added to the registry appears here with no second edit.
pub(crate) fn render_registry_help(show_all: bool) -> String {
    use crate::cli::command_registry::{entries_in_group, is_valid_mid_turn, HelpGroup};

    // `*` marks a command `is_valid_mid_turn` for the *current* registered
    // name (aliases share the same entry, so this reads the canonical name,
    // not the alias a user might type). This is a real consumer of the
    // enforcement check the registry owns -- the same predicate that gates
    // dispatch mid-turn also drives what help tells the user is safe to run
    // while a turn is in flight, so the two can never drift apart.
    let mut out = String::new();
    out.push_str("Common commands (* = usable mid-turn):\n");
    for entry in entries_in_group(HelpGroup::Common) {
        let marker = if is_valid_mid_turn(entry.name) {
            "*"
        } else {
            " "
        };
        out.push_str(&format!(
            "  {}{:<27} {}\n",
            marker, entry.name, entry.summary
        ));
    }
    if show_all {
        out.push_str("\nAdvanced commands:\n");
        for entry in entries_in_group(HelpGroup::Advanced) {
            let marker = if is_valid_mid_turn(entry.name) {
                "*"
            } else {
                " "
            };
            out.push_str(&format!(
                "  {}{:<27} {}\n",
                marker, entry.name, entry.summary
            ));
        }
    }
    out
}
