//! ENG-44: on-device clone of a selected HTTPS repository into the app-owned
//! workspace, with progress, cancellation, auth, & cleanup after failure.
//! Uses the same guarded pattern as `git_write`: typed error strings, no
//! shell interpolation, workspace containment, non-force defaults.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use git2::{build::RepoBuilder, FetchOptions, RemoteCallbacks};
use rightkit_agent_policy::{
    CapabilityClass, PermissionDecision, PermissionEngine, PermissionMode,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct GitCloneTool;

#[async_trait::async_trait]
impl Tool for GitCloneTool {
    fn name(&self) -> &str {
        "git_clone"
    }

    fn description(&self) -> &str {
        "Clone an exact HTTPS repository URL into an empty app-owned workspace directory, with progress, cancellation, and cleanup of the partial clone on failure. Force and non-HTTPS remotes are refused."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["remote_url"],
            "properties": {
                "remote_url": { "type": "string", "description": "Exact HTTPS clone URL" },
                "repo_path": { "type": "string", "description": "Destination directory inside the workspace (defaults to workspace root)" },
                "branch": { "type": "string", "description": "Optional exact branch to check out after clone" }
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::GitWrite)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if ctx.permission_mode != PermissionMode::Bypass && !ctx.durable_write_approved {
            return ToolResult::error(
                "git_clone requires approval bound to this exact tool call.".to_string(),
            );
        }
        let verdict = PermissionEngine::new().evaluate(
            CapabilityClass::GitWrite,
            ctx.permission_mode,
            ctx.workspace_trust,
            None,
            false,
        );
        if verdict.decision == PermissionDecision::Deny {
            return ToolResult::error(format!(
                "Permission denied: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }
        if verdict.decision == PermissionDecision::Ask && !ctx.durable_write_approved {
            return ToolResult::error(format!(
                "Git clone requires approval: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }

        let cancel_rx = ctx.cancel_rx.clone();
        match clone(&args, ctx, cancel_rx) {
            Ok(effect) => ToolResult::ok(
                json!({
                    "operation": "clone",
                    "approval": {
                        "permissionMode": format!("{:?}", ctx.permission_mode),
                        "explicit": ctx.durable_write_approved,
                        "policySource": verdict.policy_source
                    },
                    "effect": effect
                })
                .to_string(),
            ),
            Err(error) => ToolResult::error(error),
        }
    }
}

/// Minimal, dependency-free HTTPS-URL validation: the `tools` crate does not
/// depend on the `url` crate, so this checks exactly the invariants ENG-44
/// needs (scheme, no embedded credentials, non-empty host) without pulling in
/// a full parser. Query and fragment are stripped so a caller cannot smuggle
/// tokens through them.
fn validate_https_url(raw: &str) -> Result<String, String> {
    let rest = raw
        .strip_prefix("https://")
        .ok_or_else(|| "git_clone accepts HTTPS remotes only.".to_string())?;
    let authority_and_path = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, _) = authority_and_path
        .split_once('/')
        .unwrap_or((authority_and_path, ""));
    if authority.contains('@') {
        return Err("Remote URL must not contain credentials.".to_string());
    }
    if authority.is_empty() {
        return Err("Remote URL must name a host.".to_string());
    }
    Ok(format!("https://{authority_and_path}"))
}

fn callbacks(
    remote_url: &str,
    github_token: Option<&str>,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
) -> RemoteCallbacks<'static> {
    let github_token = github_token
        .filter(|_| {
            remote_url.starts_with("https://github.com/")
                || remote_url.starts_with("https://api.github.com/")
        })
        .map(str::to_owned);
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, _username, allowed| {
        if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            if let Some(token) = github_token.as_deref() {
                return git2::Cred::userpass_plaintext("x-access-token", token);
            }
        }
        git2::Cred::default()
    });
    let cancel_flag: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    if let Some(mut rx) = cancel_rx {
        let flag = cancel_flag.clone();
        tokio::spawn(async move {
            if rx.changed().await.is_ok() && *rx.borrow() {
                flag.store(true, Ordering::SeqCst);
            }
        });
    }
    callbacks.transfer_progress(move |_progress| !cancel_flag.load(Ordering::SeqCst));
    callbacks
}

fn clone(
    args: &Value,
    ctx: &ToolContext,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<Value, String> {
    let raw_url = args["remote_url"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "git_clone requires exact 'remote_url'.".to_string())?;
    let remote_url = validate_https_url(raw_url)?;
    let requested = args["repo_path"].as_str().unwrap_or(".");
    let destination = crate::read_file::resolve_in_workspace(requested, &ctx.workspace_root)?;
    let canonical_root = ctx
        .workspace_root
        .canonicalize()
        .unwrap_or_else(|_| ctx.workspace_root.clone());

    if destination.exists() {
        let has_entries = destination
            .read_dir()
            .map_err(|error| format!("Read clone destination: {error}"))?
            .next()
            .is_some();
        if has_entries {
            return Err(format!(
                "Clone destination '{}' is not empty.",
                destination.display()
            ));
        }
    } else {
        std::fs::create_dir_all(&destination)
            .map_err(|error| format!("Create clone destination: {error}"))?;
    }
    let canonical_destination = destination
        .canonicalize()
        .map_err(|error| format!("Resolve clone destination: {error}"))?;
    if !canonical_destination.starts_with(&canonical_root) {
        return Err(format!(
            "Clone destination '{}' escapes workspace root '{}'.",
            canonical_destination.display(),
            canonical_root.display()
        ));
    }

    let mut fetch = FetchOptions::new();
    fetch.remote_callbacks(callbacks(
        &remote_url,
        ctx.github_token.as_deref(),
        cancel_rx,
    ));
    let mut builder = RepoBuilder::new();
    builder.fetch_options(fetch);
    if let Some(branch) = args["branch"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        builder.branch(branch);
    }
    let repo = match builder.clone(&remote_url, &destination) {
        Ok(repo) => repo,
        Err(error) => {
            // Cleanup after failure: never leave a half-populated directory
            // behind for the caller to mistake for a usable clone.
            let _ = std::fs::remove_dir_all(&destination);
            return Err(format!("Clone failed: {error}"));
        }
    };
    Ok(json!({
        "repository": crate::git_read::repository_identity_path(&repo),
        "remoteUrl": crate::output_compaction::mask_secrets(&remote_url),
        "headBranch": repo.head().ok().and_then(|head| head.shorthand().ok().map(str::to_string))
    }))
}
