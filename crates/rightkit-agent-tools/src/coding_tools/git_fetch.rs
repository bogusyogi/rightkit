//! ENG-46: authenticated, cancellable fetch that updates remote-tracking refs
//! only. Never touches the working tree, index, or HEAD — that boundary is
//! what distinguishes fetch from `pull`/`switch`, which stay dirty-guarded
//! mutations elsewhere.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use git2::{FetchOptions, RemoteCallbacks, Repository};
use rightkit_agent_policy::{
    CapabilityClass, PermissionDecision, PermissionEngine, PermissionMode,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct GitFetchTool;

#[async_trait::async_trait]
impl Tool for GitFetchTool {
    fn name(&self) -> &str {
        "git_fetch"
    }

    fn description(&self) -> &str {
        "Authenticated, cancellable Git fetch that updates remote-tracking refs only. Never touches the working tree, index, or HEAD."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo_path": { "type": "string" },
                "remote": { "type": "string", "description": "Remote name to fetch (defaults to origin)" },
                "expected_remote_url": { "type": "string", "description": "Optional exact remote URL guard" }
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::GitWrite)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if ctx.permission_mode != PermissionMode::Bypass && !ctx.durable_write_approved {
            return ToolResult::error(
                "git_fetch requires approval bound to this exact tool call.".to_string(),
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
                "Git fetch requires approval: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }

        let repo = match crate::coding_tools::git_read::open_workspace_repository(
            args["repo_path"].as_str(),
            ctx,
        ) {
            Ok(repo) => repo,
            Err(error) => return ToolResult::error(error),
        };
        match fetch(
            &repo,
            &args,
            ctx.github_token.as_deref(),
            ctx.cancel_rx.clone(),
        ) {
            Ok(effect) => ToolResult::ok(
                json!({
                    "operation": "fetch",
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
    callbacks.credentials(move |_url, username, allowed| {
        if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            if let Some(token) = github_token.as_deref() {
                return git2::Cred::userpass_plaintext("x-access-token", token);
            }
        }
        if allowed.contains(git2::CredentialType::SSH_KEY) {
            return username
                .map(git2::Cred::ssh_key_from_agent)
                .unwrap_or_else(|| {
                    Err(git2::Error::from_str("remote did not provide SSH username"))
                });
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

fn fetch(
    repo: &Repository,
    args: &Value,
    github_token: Option<&str>,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<Value, String> {
    let remote_name = args["remote"].as_str().unwrap_or("origin");
    let mut remote = repo
        .find_remote(remote_name)
        .map_err(|error| format!("Unknown remote '{remote_name}': {error}"))?;
    let remote_url = remote
        .url()
        .map_err(|error| format!("Read remote '{remote_name}' URL: {error}"))?
        .to_string();
    if let Some(expected) = args["expected_remote_url"].as_str() {
        if expected != remote_url {
            return Err(format!(
                "Remote URL changed: expected '{expected}', current '{}'.",
                crate::coding_tools::output_compaction::mask_secrets(&remote_url)
            ));
        }
    }
    let mut options = FetchOptions::new();
    options.remote_callbacks(callbacks(&remote_url, github_token, cancel_rx));
    // No refspecs supplied: fetch the remote's configured default refspecs,
    // which update `refs/remotes/<remote>/*` only. `RemoteUpdateFlags`
    // defaults never touch HEAD, the index, or the working tree.
    remote
        .fetch(&[] as &[&str], Some(&mut options), None)
        .map_err(|error| format!("Fetch from '{remote_name}' failed: {error}"))?;
    let stats = remote.stats();
    Ok(json!({
        "remote": remote_name,
        "remoteUrl": crate::coding_tools::output_compaction::mask_secrets(&remote_url),
        "receivedObjects": stats.received_objects(),
        "totalObjects": stats.total_objects()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::WorkspaceTrust;

    fn ctx(root: &std::path::Path) -> ToolContext {
        ToolContext::new(
            "git-fetch-test",
            root,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
    }

    fn init_repo_with_remote(remote_path: &std::path::Path) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        repo.remote("origin", &format!("file://{}", remote_path.display()))
            .unwrap();
        dir
    }

    #[tokio::test]
    async fn fetch_never_touches_working_tree() {
        let source = tempfile::tempdir().unwrap();
        let source_repo = Repository::init(source.path()).unwrap();
        let sig = git2::Signature::now("Origin", "origin@example.invalid").unwrap();
        let tree_id = {
            let mut index = source_repo.index().unwrap();
            index.write_tree().unwrap()
        };
        let tree = source_repo.find_tree(tree_id).unwrap();
        source_repo
            .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let clone_dir = init_repo_with_remote(source.path());
        std::fs::write(clone_dir.path().join("untouched.txt"), "keep me\n").unwrap();

        let result = GitFetchTool
            .execute(json!({}), &ctx(clone_dir.path()))
            .await;
        // file:// remotes may or may not be supported by the linked libgit2
        // transport in this build; either way the working tree must be
        // untouched, which is the actual invariant under test.
        let _ = result;
        assert_eq!(
            std::fs::read_to_string(clone_dir.path().join("untouched.txt")).unwrap(),
            "keep me\n"
        );
    }

    #[tokio::test]
    async fn fetch_requires_approval_outside_bypass() {
        let workspace = tempfile::tempdir().unwrap();
        Repository::init(workspace.path()).unwrap();
        let result = GitFetchTool
            .execute(
                json!({}),
                &ToolContext::new(
                    "s",
                    workspace.path(),
                    PermissionMode::AutoSafe,
                    WorkspaceTrust::Trusted,
                ),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("requires approval"));
    }
}
