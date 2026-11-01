#[derive(Debug)]
struct Target {
    path: PathBuf,
    line_index: usize,
    character: usize,
}

fn target_from_args(args: &Value, ctx: &ToolContext) -> Result<Target, String> {
    let path = path_arg(args, ctx)?;
    let line = args["line"]
        .as_u64()
        .ok_or_else(|| "Missing required argument: line".to_string())?;
    let character = args["character"]
        .as_u64()
        .ok_or_else(|| "Missing required argument: character".to_string())?;
    Ok(Target {
        path,
        line_index: (line as usize).saturating_sub(1),
        character: character as usize,
    })
}

fn path_arg(args: &Value, ctx: &ToolContext) -> Result<PathBuf, String> {
    let path = args["path"]
        .as_str()
        .ok_or_else(|| "Missing required argument: path".to_string())?;
    crate::coding_tools::read_file::resolve_in_workspace(path, &ctx.workspace_root)
}

fn server_config_from_args_or_path(
    args: &Value,
    path: &Path,
    ctx: &ToolContext,
) -> Option<LspServerConfig> {
    let requested_path = args["server_path"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| {
            configured_server_for_path(path, ctx).map(|server| PathBuf::from(server.server_path))
        })
        .or_else(|| discover_server(path));
    let server_path = resolve_server_executable(&requested_path?, &ctx.workspace_root)?;
    let configured_args = configured_server_for_path(path, ctx)
        .map(|server| server.server_args)
        .unwrap_or_else(|| default_server_args(path));
    let server_args = args["server_args"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or(configured_args);
    Some(LspServerConfig {
        language_id: language_id(path).unwrap_or("plaintext").to_string(),
        server_path: server_path.to_string_lossy().to_string(),
        server_args,
        workspace_root: file_uri(&ctx.workspace_root),
    })
}

fn render_status(workspace_root: &Path) -> String {
    let status = lsp_status(workspace_root);
    let mut lines = vec![
        format!("workspace: {}", status.workspace_root),
        format!("config: {}", status.config_path),
    ];
    for server in status.servers {
        let path = server
            .effective_path
            .unwrap_or_else(|| "not found".to_string());
        lines.push(format!("{}: {} ({})", server.language, path, server.source));
    }
    lines.push(format!(
        "fallback: local symbol index {}",
        if status.fallback_enabled {
            "enabled"
        } else {
            "disabled"
        }
    ));
    lines.join("\n")
}

pub fn lsp_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".rightkit").join("lsp.json")
}

pub fn load_lsp_workspace_config(workspace_root: &Path) -> LspWorkspaceConfig {
    let path = lsp_config_path(workspace_root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return LspWorkspaceConfig::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn write_lsp_workspace_config(
    workspace_root: &Path,
    config: &LspWorkspaceConfig,
) -> Result<(), String> {
    let path = lsp_config_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create '{}': {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|e| format!("failed to serialize LSP config: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("failed to write '{}': {e}", path.display()))
}

pub fn lsp_status(workspace_root: &Path) -> LspStatusView {
    let config = load_lsp_workspace_config(workspace_root);
    let mut rows = known_lsp_servers()
        .iter()
        .map(|(language, executable, extensions)| {
            lsp_server_status(language, executable, extensions, &config, workspace_root)
        })
        .collect::<Vec<_>>();

    for configured in &config.servers {
        if rows.iter().any(|row| row.language == configured.language) {
            continue;
        }
        rows.push(lsp_server_status(
            &configured.language,
            "",
            &configured
                .extensions
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            &config,
            workspace_root,
        ));
    }

    LspStatusView {
        workspace_root: workspace_root.display().to_string(),
        config_path: lsp_config_path(workspace_root).display().to_string(),
        fallback_enabled: true,
        servers: rows,
    }
}

fn lsp_server_status(
    language: &str,
    executable: &str,
    extensions: &[&str],
    config: &LspWorkspaceConfig,
    workspace_root: &Path,
) -> LspServerStatus {
    let configured = config
        .servers
        .iter()
        .find(|server| server.language == language)
        .cloned();
    let discovered = if executable.is_empty() {
        None
    } else {
        find_executable(executable)
            .or_else(|| find_executable(&format!("{executable}.cmd")))
            .map(|p| p.display().to_string())
    };
    let effective = configured
        .as_ref()
        .map(|server| resolve_server_executable(Path::new(&server.server_path), workspace_root))
        .unwrap_or_else(|| discovered.as_ref().map(PathBuf::from))
        .map(|path| path.display().to_string());
    let source = if configured.is_some() {
        "workspace-config"
    } else if discovered.is_some() {
        "path"
    } else {
        "fallback"
    };
    LspServerStatus {
        language: language.to_string(),
        executable: executable.to_string(),
        extensions: configured
            .as_ref()
            .map(|server| server.extensions.clone())
            .unwrap_or_else(|| extensions.iter().map(|ext| (*ext).to_string()).collect()),
        configured_path: configured.as_ref().map(|server| server.server_path.clone()),
        discovered_path: discovered,
        effective_path: effective.clone(),
        server_args: configured
            .as_ref()
            .map(|server| server.server_args.clone())
            .unwrap_or_else(|| match language {
                "python" | "typescript" | "javascript" => vec!["--stdio".to_string()],
                _ => Vec::new(),
            }),
        source: source.to_string(),
        available: effective.is_some(),
    }
}

fn configured_server_for_path(path: &Path, ctx: &ToolContext) -> Option<LspWorkspaceServer> {
    configured_server_for_workspace_path(path, &ctx.workspace_root)
}

fn configured_server_for_workspace_path(
    path: &Path,
    workspace_root: &Path,
) -> Option<LspWorkspaceServer> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let language = language_id(path).unwrap_or("");
    load_lsp_workspace_config(workspace_root)
        .servers
        .into_iter()
        .find(|server| {
            (!language.is_empty() && server.language == language)
                || server.extensions.iter().any(|candidate| candidate == ext)
        })
}

fn known_lsp_servers() -> [(&'static str, &'static str, &'static [&'static str]); 3] {
    [
        ("rust", "rust-analyzer", &["rs"]),
        (
            "typescript",
            "typescript-language-server",
            &["ts", "tsx", "js", "jsx"],
        ),
        ("python", "pyright-langserver", &["py"]),
    ]
}

fn discover_server(path: &Path) -> Option<PathBuf> {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => find_executable("rust-analyzer"),
        "ts" | "tsx" | "js" | "jsx" => find_executable("typescript-language-server")
            .or_else(|| find_executable("typescript-language-server.cmd")),
        "py" => find_executable("pyright-langserver")
            .or_else(|| find_executable("pyright-langserver.cmd")),
        _ => None,
    }
}

fn default_server_args(path: &Path) -> Vec<String> {
    match language_id(path) {
        Some("python" | "typescript" | "javascript") => vec!["--stdio".to_string()],
        _ => Vec::new(),
    }
}

fn resolve_server_executable(path: &Path, workspace_root: &Path) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }
    if path.components().count() > 1 || path.is_absolute() {
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace_root.join(path)
        };
        return resolved.is_file().then_some(resolved);
    }
    let name = path.to_str()?;
    find_executable(name).or_else(|| find_executable(&format!("{name}.cmd")))
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

pub(super) fn language_id(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str())? {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "py" => Some("python"),
        _ => None,
    }
}
