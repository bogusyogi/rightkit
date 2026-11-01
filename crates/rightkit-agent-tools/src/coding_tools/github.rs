//! `github` — thin GitHub API tool for the agent.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

pub struct GitHubTool;

#[async_trait::async_trait]
impl Tool for GitHubTool {
    fn name(&self) -> &str {
        "github"
    }

    fn description(&self) -> &str {
        "GitHub API (forge-aware, GitHub App installation-token compatible): list_prs, get_file, add_comment, get_pr_files, get_pr_comments, get_pr_review_comments, get_repo, get_branch, list_branches, get_commit, list_commits, list_issues, get_issue, list_releases, get_pr, get_pr_diff, resolve_pr_for_branch, create_pr, update_pr, set_pr_draft, add_review_comment, submit_review, resolve_review_thread, merge_pr, get_combined_status, get_check_runs, list_issue_comments. Supports forge=github (default) and base_url override for GHES. Requires github_token config (PAT or ghs_ installation token). Mutating actions (create_pr, update_pr, set_pr_draft, add_comment, add_review_comment, submit_review, resolve_review_thread, merge_pr) require confirm=true and will not execute otherwise. merge_pr additionally requires an explicit merge_method. Fail: no token, unsupported forge, missing confirmation, or API error."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action", "owner", "repo"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list_prs", "get_file", "add_comment", "get_pr_files", "get_pr_comments", "get_pr_review_comments", "get_repo", "get_branch", "list_branches", "get_commit", "list_commits", "list_issues", "get_issue", "list_releases", "get_pr", "get_pr_diff", "resolve_pr_for_branch", "create_pr", "update_pr", "set_pr_draft", "add_review_comment", "submit_review", "resolve_review_thread", "merge_pr", "get_combined_status", "get_check_runs", "list_issue_comments"],
                    "description": "GitHub action to perform"
                },
                "owner": { "type": "string" },
                "repo": { "type": "string" },
                "forge": { "type": "string", "description": "Forge selector (github only, default github)" },
                "base_url": { "type": "string", "description": "Override API base URL for GHES (default https://api.github.com)" },
                "path": { "type": "string", "description": "File path (for get_file)" },
                "ref_": { "type": "string", "description": "Git ref (for get_file, get_commit, get_combined_status, get_check_runs)" },
                "sha": { "type": "string", "description": "Commit SHA (for get_commit)" },
                "branch": { "type": "string", "description": "Branch name (for get_branch, resolve_pr_for_branch, create_pr head branch)" },
                "issue_number": { "type": "integer", "description": "Issue number (for get_issue, list_issue_comments)" },
                "pr_number": { "type": "integer", "description": "PR number (for add_comment, get_pr_files, get_pr_comments, get_pr_review_comments, get_pr, get_pr_diff, update_pr, set_pr_draft, add_review_comment, submit_review, resolve_review_thread, merge_pr)" },
                "body": { "type": "string", "description": "Comment/PR/review body" },
                "state": { "type": "string", "description": "PR/issue list filter state: open|closed|all (default open)" },
                "author": { "type": "string", "description": "Filter list_prs by author login" },
                "label": { "type": "string", "description": "Filter list_prs by label (used with list_issues-style search)" },
                "max_bytes": { "type": "integer", "description": "Bound diff size in bytes for get_pr_diff (default 200000)" },
                "title": { "type": "string", "description": "PR title (for create_pr, update_pr)" },
                "head": { "type": "string", "description": "Head branch for create_pr, defaults to branch" },
                "base": { "type": "string", "description": "Base branch for create_pr/update_pr (default repo default branch)" },
                "draft": { "type": "boolean", "description": "Create as draft PR (create_pr) or target draft state (set_pr_draft)" },
                "ready": { "type": "boolean", "description": "For set_pr_draft: true marks ready-for-review, false marks draft" },
                "commit_id": { "type": "string", "description": "Commit SHA the review comment/review anchors to (add_review_comment, submit_review)" },
                "commit_path": { "type": "string", "description": "File path for add_review_comment" },
                "line": { "type": "integer", "description": "Line number for add_review_comment" },
                "event": { "type": "string", "description": "Review event for submit_review: APPROVE|REQUEST_CHANGES|COMMENT" },
                "thread_id": { "type": "string", "description": "Review thread node id for resolve_review_thread (GraphQL node id)" },
                "merge_method": { "type": "string", "description": "Required for merge_pr: merge|squash|rebase" },
                "confirm": { "type": "boolean", "description": "Required true to execute any mutating action" },
                "has_scoped_permission": { "type": "boolean", "description": "Caller-asserted flag that the connector/token holds scoped write permission for this write operation (required true for merge_pr, create_pr, submit_review with APPROVE/REQUEST_CHANGES). Assumption: no permission-scope registry exists yet in this crate, so callers must explicitly assert scope." }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let token = match &ctx.github_token {
            Some(t) => t.clone(),
            None => {
                return ToolResult::error(
                    "No GitHub credentials found. Sign in with GitHub in Agent, run `gh auth login`, or set GH_TOKEN.",
                )
            }
        };

        let action = match args["action"].as_str() {
            Some(a) => a,
            None => return ToolResult::error("Missing required argument: action"),
        };
        let owner = match args["owner"].as_str() {
            Some(o) => o,
            None => return ToolResult::error("Missing required argument: owner"),
        };
        let repo = match args["repo"].as_str() {
            Some(r) => r,
            None => return ToolResult::error("Missing required argument: repo"),
        };

        let base_url = match resolve_github_base_url(&args) {
            Ok(url) => url,
            Err(error) => return ToolResult::error(error),
        };
        let client = match crate::coding_tools::http_client::client_following(
            &base_url,
            crate::coding_tools::http_client::DEFAULT_TIMEOUT,
        ) {
            Ok(client) => client,
            Err(error) => {
                return ToolResult::error(format!("Could not create GitHub client: {error}"))
            }
        };

        match action {
            "list_prs" => {
                let url = format!(
                    "{}/repos/{}/{}/pulls?state=open&per_page=20",
                    base_url, owner, repo
                );
                github_get(&client, &url, &token).await
            }
            "get_file" => {
                let path = match args["path"].as_str() {
                    Some(p) => p,
                    None => return ToolResult::error("Missing argument: path"),
                };
                let ref_ = args["ref_"].as_str().unwrap_or("HEAD");
                let url = format!(
                    "{}/repos/{}/{}/contents/{}?ref={}",
                    base_url, owner, repo, path, ref_
                );
                let result = github_get(&client, &url, &token).await;
                if result.is_error {
                    return result;
                }
                // Decode base64 content from GitHub API response
                let v: Value = serde_json::from_str(&result.content).unwrap_or_default();
                if let Some(content_b64) = v["content"].as_str() {
                    let cleaned = content_b64.replace('\n', "");
                    match base64_decode(&cleaned) {
                        Ok(text) => ToolResult::ok(text),
                        Err(e) => ToolResult::error(format!("Failed to decode content: {}", e)),
                    }
                } else {
                    result
                }
            }
            "add_comment" => {
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let body = match args["body"].as_str() {
                    Some(b) => b,
                    None => return ToolResult::error("Missing argument: body"),
                };
                let url = format!(
                    "{}/repos/{}/{}/issues/{}/comments",
                    base_url, owner, repo, pr_number
                );
                github_post(&client, &url, &token, json!({"body": body})).await
            }
            "get_pr_files" => {
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let url = format!(
                    "{}/repos/{}/{}/pulls/{}/files",
                    base_url, owner, repo, pr_number
                );
                github_get(&client, &url, &token).await
            }
            "get_pr_comments" => {
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let url = github_pr_comments_url(&base_url, owner, repo, pr_number);
                github_get(&client, &url, &token).await
            }
            "get_pr_review_comments" => {
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let url = github_pr_review_comments_url(&base_url, owner, repo, pr_number);
                github_get(&client, &url, &token).await
            }
            "get_repo" => {
                let url = format!("{}/repos/{}/{}", base_url, owner, repo);
                github_get(&client, &url, &token).await
            }
            "get_branch" => {
                let branch = match args["branch"].as_str().or(args["ref_"].as_str()) {
                    Some(b) => b,
                    None => return ToolResult::error("Missing argument: branch (or ref_)"),
                };
                let url = format!("{}/repos/{}/{}/branches/{}", base_url, owner, repo, branch);
                github_get(&client, &url, &token).await
            }
            "list_branches" => {
                let url = format!("{}/repos/{}/{}/branches?per_page=50", base_url, owner, repo);
                github_get(&client, &url, &token).await
            }
            "get_commit" => {
                let sha = match args["sha"].as_str().or(args["ref_"].as_str()) {
                    Some(s) => s,
                    None => return ToolResult::error("Missing argument: sha (or ref_)"),
                };
                let url = format!("{}/repos/{}/{}/commits/{}", base_url, owner, repo, sha);
                github_get(&client, &url, &token).await
            }
            "list_commits" => {
                let sha = args["sha"].as_str().or(args["ref_"].as_str());
                let url = if let Some(s) = sha {
                    format!(
                        "{}/repos/{}/{}/commits?sha={}&per_page=20",
                        base_url, owner, repo, s
                    )
                } else {
                    format!("{}/repos/{}/{}/commits?per_page=20", base_url, owner, repo)
                };
                github_get(&client, &url, &token).await
            }
            "list_issues" => {
                let url = format!(
                    "{}/repos/{}/{}/issues?state=all&per_page=20",
                    base_url, owner, repo
                );
                github_get(&client, &url, &token).await
            }
            "get_issue" => {
                let issue_number =
                    match args["issue_number"].as_u64().or(args["pr_number"].as_u64()) {
                        Some(n) => n,
                        None => return ToolResult::error("Missing argument: issue_number"),
                    };
                let url = format!(
                    "{}/repos/{}/{}/issues/{}",
                    base_url, owner, repo, issue_number
                );
                github_get(&client, &url, &token).await
            }
            "list_releases" => {
                let url = format!("{}/repos/{}/{}/releases?per_page=20", base_url, owner, repo);
                github_get(&client, &url, &token).await
            }
            // ENG-57: PR list/detail/diff
            "get_pr" => {
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let url = format!("{}/repos/{}/{}/pulls/{}", base_url, owner, repo, pr_number);
                github_get(&client, &url, &token).await
            }
            "get_pr_diff" => {
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let max_bytes = args["max_bytes"].as_u64().unwrap_or(200_000) as usize;
                let url = format!("{}/repos/{}/{}/pulls/{}", base_url, owner, repo, pr_number);
                github_get_diff(&client, &url, &token, max_bytes).await
            }
            "resolve_pr_for_branch" => {
                let branch = match args["branch"].as_str() {
                    Some(b) => b,
                    None => return ToolResult::error("Missing argument: branch"),
                };
                let state = args["state"].as_str().unwrap_or("open");
                let url = format!(
                    "{}/repos/{}/{}/pulls?state={}&head={}:{}&per_page=1",
                    base_url, owner, repo, state, owner, branch
                );
                github_get(&client, &url, &token).await
            }
            // ENG-58: PR create/update/draft/ready
            "create_pr" => {
                if !confirmed(&args) {
                    return ToolResult::error(
                        "create_pr requires confirm=true to execute (explicit confirmation).",
                    );
                }
                let title = match args["title"].as_str() {
                    Some(t) => t,
                    None => return ToolResult::error("Missing argument: title"),
                };
                let head = match args["head"].as_str().or(args["branch"].as_str()) {
                    Some(h) => h,
                    None => return ToolResult::error("Missing argument: head (or branch)"),
                };
                let base = args["base"].as_str().unwrap_or("main");
                let body = args["body"].as_str().unwrap_or("");
                let draft = args["draft"].as_bool().unwrap_or(false);

                // Detect + reuse an existing open PR for this branch before creating a new one.
                let existing_url = format!(
                    "{}/repos/{}/{}/pulls?state=open&head={}:{}&per_page=1",
                    base_url, owner, repo, owner, head
                );
                let existing = github_get(&client, &existing_url, &token).await;
                if !existing.is_error {
                    if let Ok(Value::Array(prs)) = serde_json::from_str(&existing.content) {
                        if !prs.is_empty() {
                            return ToolResult::ok(existing.content);
                        }
                    }
                }

                let url = format!("{}/repos/{}/{}/pulls", base_url, owner, repo);
                github_post(
                    &client,
                    &url,
                    &token,
                    json!({"title": title, "head": head, "base": base, "body": body, "draft": draft}),
                )
                .await
            }
            "update_pr" => {
                if !confirmed(&args) {
                    return ToolResult::error(
                        "update_pr requires confirm=true to execute (explicit confirmation).",
                    );
                }
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let mut patch = serde_json::Map::new();
                if let Some(t) = args["title"].as_str() {
                    patch.insert("title".to_string(), json!(t));
                }
                if let Some(b) = args["body"].as_str() {
                    patch.insert("body".to_string(), json!(b));
                }
                if let Some(b) = args["base"].as_str() {
                    patch.insert("base".to_string(), json!(b));
                }
                if let Some(s) = args["state"].as_str() {
                    patch.insert("state".to_string(), json!(s));
                }
                if patch.is_empty() {
                    return ToolResult::error(
                        "update_pr requires at least one of: title, body, base, state",
                    );
                }
                let url = format!("{}/repos/{}/{}/pulls/{}", base_url, owner, repo, pr_number);
                github_patch(&client, &url, &token, Value::Object(patch)).await
            }
            "set_pr_draft" => {
                if !confirmed(&args) {
                    return ToolResult::error(
                        "set_pr_draft requires confirm=true to execute (explicit confirmation).",
                    );
                }
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let ready = args["ready"]
                    .as_bool()
                    .unwrap_or(!args["draft"].as_bool().unwrap_or(false));
                if ready {
                    // Mark ready for review (no direct REST field toggle on PATCH pulls; use dedicated endpoint).
                    let url = format!(
                        "{}/repos/{}/{}/pulls/{}/ready_for_review",
                        base_url, owner, repo, pr_number
                    );
                    github_post(&client, &url, &token, json!({})).await
                } else {
                    // Reverting to draft is not supported by REST; report explicitly.
                    ToolResult::error(
                        "Converting a PR back to draft is not supported by the GitHub REST API; use the GraphQL convertPullRequestToDraft mutation instead.",
                    )
                }
            }
            // ENG-59: PR review/comments/merge
            "add_review_comment" => {
                if !confirmed(&args) {
                    return ToolResult::error(
                        "add_review_comment requires confirm=true to execute (explicit confirmation).",
                    );
                }
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let body = match args["body"].as_str() {
                    Some(b) => b,
                    None => return ToolResult::error("Missing argument: body"),
                };
                let commit_id = match args["commit_id"].as_str() {
                    Some(c) => c,
                    None => return ToolResult::error("Missing argument: commit_id"),
                };
                let path = match args["commit_path"].as_str() {
                    Some(p) => p,
                    None => return ToolResult::error("Missing argument: commit_path"),
                };
                let line = match args["line"].as_u64() {
                    Some(l) => l,
                    None => return ToolResult::error("Missing argument: line"),
                };
                let url = format!(
                    "{}/repos/{}/{}/pulls/{}/comments",
                    base_url, owner, repo, pr_number
                );
                github_post(
                    &client,
                    &url,
                    &token,
                    json!({"body": body, "commit_id": commit_id, "path": path, "line": line}),
                )
                .await
            }
            "submit_review" => {
                if !confirmed(&args) {
                    return ToolResult::error(
                        "submit_review requires confirm=true to execute (explicit confirmation).",
                    );
                }
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let event = match args["event"].as_str() {
                    Some(e) => e.to_ascii_uppercase(),
                    None => {
                        return ToolResult::error(
                            "Missing argument: event (APPROVE|REQUEST_CHANGES|COMMENT)",
                        )
                    }
                };
                if event != "APPROVE" && event != "REQUEST_CHANGES" && event != "COMMENT" {
                    return ToolResult::error(
                        "event must be one of APPROVE, REQUEST_CHANGES, COMMENT",
                    );
                }
                if (event == "APPROVE" || event == "REQUEST_CHANGES")
                    && !has_scoped_permission(&args)
                {
                    return ToolResult::error(
                        "submit_review with APPROVE/REQUEST_CHANGES requires has_scoped_permission=true asserting the connector token holds scoped review-write permission.",
                    );
                }
                let body = args["body"].as_str().unwrap_or("");
                let mut payload = json!({"event": event, "body": body});
                if let Some(commit_id) = args["commit_id"].as_str() {
                    payload["commit_id"] = json!(commit_id);
                }
                let url = format!(
                    "{}/repos/{}/{}/pulls/{}/reviews",
                    base_url, owner, repo, pr_number
                );
                github_post(&client, &url, &token, payload).await
            }
            "resolve_review_thread" => {
                if !confirmed(&args) {
                    return ToolResult::error(
                        "resolve_review_thread requires confirm=true to execute (explicit confirmation).",
                    );
                }
                let thread_id = match args["thread_id"].as_str() {
                    Some(t) => t,
                    None => return ToolResult::error("Missing argument: thread_id"),
                };
                // Resolving review threads is GraphQL-only in the GitHub API.
                let graphql_url = format!("{}/graphql", graphql_base_url(&base_url));
                let query = "mutation($threadId:ID!){resolveReviewThread(input:{threadId:$threadId}){thread{id isResolved}}}";
                github_post(
                    &client,
                    &graphql_url,
                    &token,
                    json!({"query": query, "variables": {"threadId": thread_id}}),
                )
                .await
            }
            "merge_pr" => {
                if !confirmed(&args) {
                    return ToolResult::error(
                        "merge_pr requires confirm=true to execute (explicit approval).",
                    );
                }
                if !has_scoped_permission(&args) {
                    return ToolResult::error(
                        "merge_pr requires has_scoped_permission=true asserting the connector token holds scoped merge-write permission.",
                    );
                }
                let pr_number = match args["pr_number"].as_u64() {
                    Some(n) => n,
                    None => return ToolResult::error("Missing argument: pr_number"),
                };
                let merge_method = match args["merge_method"].as_str() {
                    Some(m) if m == "merge" || m == "squash" || m == "rebase" => m,
                    Some(other) => {
                        return ToolResult::error(format!(
                            "invalid merge_method '{other}'; must be merge, squash, or rebase"
                        ))
                    }
                    None => {
                        return ToolResult::error(
                            "merge_pr requires explicit merge_method: merge|squash|rebase",
                        )
                    }
                };
                let url = format!(
                    "{}/repos/{}/{}/pulls/{}/merge",
                    base_url, owner, repo, pr_number
                );
                github_put(&client, &url, &token, json!({"merge_method": merge_method})).await
            }
            // ENG-60: checks, statuses, issues, releases (read)
            "get_combined_status" => {
                let ref_ = match args["ref_"].as_str() {
                    Some(r) => r,
                    None => return ToolResult::error("Missing argument: ref_"),
                };
                let url = format!(
                    "{}/repos/{}/{}/commits/{}/status",
                    base_url, owner, repo, ref_
                );
                github_get(&client, &url, &token).await
            }
            "get_check_runs" => {
                let ref_ = match args["ref_"].as_str() {
                    Some(r) => r,
                    None => return ToolResult::error("Missing argument: ref_"),
                };
                let url = format!(
                    "{}/repos/{}/{}/commits/{}/check-runs",
                    base_url, owner, repo, ref_
                );
                github_get(&client, &url, &token).await
            }
            "list_issue_comments" => {
                let issue_number =
                    match args["issue_number"].as_u64().or(args["pr_number"].as_u64()) {
                        Some(n) => n,
                        None => return ToolResult::error("Missing argument: issue_number"),
                    };
                let url = format!(
                    "{}/repos/{}/{}/issues/{}/comments?per_page=50",
                    base_url, owner, repo, issue_number
                );
                github_get(&client, &url, &token).await
            }
            other => ToolResult::error(format!("Unknown action: {}", other)),
        }
    }
}

fn resolve_github_base_url(args: &Value) -> Result<String, String> {
    if let Some(forge) = args["forge"].as_str() {
        let normalized = forge.trim().to_ascii_lowercase();
        if normalized != "github" && normalized != "gh" && !normalized.is_empty() {
            return Err(format!(
                "unsupported forge '{forge}'; only github is wired on this lane (use forge=github)"
            ));
        }
    }
    if let Some(base_url) = args["base_url"].as_str() {
        let trimmed = base_url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err("base_url must not be empty".to_string());
        }
        if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
            return Err("base_url must start with https:// or http://".to_string());
        }
        return Ok(trimmed.to_string());
    }
    Ok("https://api.github.com".to_string())
}

/// Mutating actions must pass confirm=true explicitly; absence or false blocks execution.
fn confirmed(args: &Value) -> bool {
    args["confirm"].as_bool().unwrap_or(false)
}

/// Assumption: this crate has no scoped-connector-permission registry yet, so write
/// operations that need elevated GitHub scopes (merge, approve/request-changes) require
/// the caller to explicitly assert it holds that scope via has_scoped_permission=true.
fn has_scoped_permission(args: &Value) -> bool {
    args["has_scoped_permission"].as_bool().unwrap_or(false)
}

/// Best-effort GraphQL endpoint derivation from a REST base_url (api.github.com or GHES).
fn graphql_base_url(rest_base_url: &str) -> String {
    if rest_base_url.trim_end_matches('/') == "https://api.github.com" {
        "https://api.github.com".to_string()
    } else {
        rest_base_url.trim_end_matches('/').to_string()
    }
}

fn github_pr_comments_url(base_url: &str, owner: &str, repo: &str, pr_number: u64) -> String {
    format!("{base_url}/repos/{owner}/{repo}/issues/{pr_number}/comments")
}

fn github_pr_review_comments_url(
    base_url: &str,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> String {
    format!("{base_url}/repos/{owner}/{repo}/pulls/{pr_number}/comments")
}

/// One authenticated GitHub API call. `accept` selects the media type; the body,
/// when present, is sent as JSON. Returns the status and response text.
async fn github_call(
    client: &rightkit_http::AsyncClient,
    method: rightkit_http::Method,
    url: &str,
    token: &str,
    accept: &str,
    body: Option<&Value>,
) -> Result<(u16, String), String> {
    let mut request = rightkit_http::Request::new(method, url)
        .bearer(token)
        .header("User-Agent", "rightkit-agent/1.0")
        .header("Accept", accept)
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(body) = body {
        request = request.json(body).map_err(|e| e.to_string())?;
    }
    let response = crate::coding_tools::http_client::send_any(client, &request).await?;
    Ok((response.status, response.text()))
}

/// Map a GitHub call outcome to a tool result; `ok` shapes a 2xx body.
fn github_result(
    outcome: Result<(u16, String), String>,
    ok: impl FnOnce(String) -> String,
) -> ToolResult {
    match outcome {
        Err(e) => ToolResult::error(format!("GitHub request failed: {}", e)),
        Ok((status, body)) if (200..300).contains(&status) => ToolResult::ok(ok(body)),
        Ok((status, body)) => ToolResult::error(format!("GitHub API error {}: {}", status, body)),
    }
}

async fn github_get(client: &rightkit_http::AsyncClient, url: &str, token: &str) -> ToolResult {
    github_result(
        github_call(
            client,
            rightkit_http::Method::Get,
            url,
            token,
            "application/vnd.github+json",
            None,
        )
        .await,
        |body| body,
    )
}

async fn github_post(
    client: &rightkit_http::AsyncClient,
    url: &str,
    token: &str,
    body: Value,
) -> ToolResult {
    github_result(
        github_call(
            client,
            rightkit_http::Method::Post,
            url,
            token,
            "application/vnd.github+json",
            Some(&body),
        )
        .await,
        |body| body,
    )
}

/// Fetch a PR's diff (via the `.diff` media type) with a bounded, explicit size cap so
/// large diffs never flood the caller. Truncates with a marker rather than erroring.
async fn github_get_diff(
    client: &rightkit_http::AsyncClient,
    pr_url: &str,
    token: &str,
    max_bytes: usize,
) -> ToolResult {
    github_result(
        github_call(
            client,
            rightkit_http::Method::Get,
            pr_url,
            token,
            "application/vnd.github.v3.diff",
            None,
        )
        .await,
        |body| bound_diff(&body, max_bytes),
    )
}

/// Truncate a diff body to at most `max_bytes` (on a UTF-8-safe boundary), appending a
/// clear truncation marker so callers never mistake a partial diff for a complete one.
fn bound_diff(diff: &str, max_bytes: usize) -> String {
    if diff.len() <= max_bytes {
        return diff.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !diff.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[... diff truncated at {} bytes (full diff is {} bytes) ...]",
        &diff[..end],
        max_bytes,
        diff.len()
    )
}

async fn github_patch(
    client: &rightkit_http::AsyncClient,
    url: &str,
    token: &str,
    body: Value,
) -> ToolResult {
    github_result(
        github_call(
            client,
            rightkit_http::Method::Patch,
            url,
            token,
            "application/vnd.github+json",
            Some(&body),
        )
        .await,
        |body| body,
    )
}

async fn github_put(
    client: &rightkit_http::AsyncClient,
    url: &str,
    token: &str,
    body: Value,
) -> ToolResult {
    github_result(
        github_call(
            client,
            rightkit_http::Method::Put,
            url,
            token,
            "application/vnd.github+json",
            Some(&body),
        )
        .await,
        |body| body,
    )
}

fn base64_decode(s: &str) -> Result<String, String> {
    // Manual base64 decode (avoid extra dep)
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut buf = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        let get = |b: u8| -> Option<u8> {
            if b == b'=' {
                Some(0)
            } else {
                CHARS.iter().position(|&c| c == b).map(|p| p as u8)
            }
        };
        let (a, b, c, d) = match (
            get(bytes[i]),
            get(bytes[i + 1]),
            get(bytes[i + 2]),
            get(bytes[i + 3]),
        ) {
            (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
            _ => return Err("invalid base64".to_string()),
        };
        buf.push((a << 2) | (b >> 4));
        if bytes[i + 2] != b'=' {
            buf.push((b << 4) | (c >> 2));
        }
        if bytes[i + 3] != b'=' {
            buf.push((c << 6) | d);
        }
        i += 4;
    }
    String::from_utf8(buf).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use tempfile::tempdir;

    fn ctx_no_token() -> ToolContext {
        let dir = tempdir().unwrap();
        ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
    }

    #[tokio::test]
    async fn github_no_token_returns_error() {
        let tool = GitHubTool;
        let result = tool
            .execute(
                json!({"action": "list_prs", "owner": "foo", "repo": "bar"}),
                &ctx_no_token(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("No GitHub credentials found"));
    }

    #[tokio::test]
    async fn github_missing_action_returns_error() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_github_token("tok");
        let tool = GitHubTool;
        let result = tool
            .execute(json!({"owner": "foo", "repo": "bar"}), &ctx)
            .await;
        assert!(result.is_error);
    }

    #[test]
    fn base64_decode_basic() {
        // "Hello" in base64 = "SGVsbG8="
        assert_eq!(base64_decode("SGVsbG8=").unwrap(), "Hello");
    }

    #[test]
    fn github_schema_advertises_pr_comment_fetch_actions() {
        let schema = GitHubTool.input_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert!(actions.iter().any(|action| action == "get_pr_comments"));
        assert!(actions
            .iter()
            .any(|action| action == "get_pr_review_comments"));
        assert!(GitHubTool.description().contains("get_pr_comments"));
        assert!(GitHubTool.description().contains("get_pr_review_comments"));
    }

    #[test]
    fn github_pr_comment_urls_target_issue_and_review_threads() {
        assert_eq!(
            github_pr_comments_url("https://api.github.com", "octo", "repo", 42),
            "https://api.github.com/repos/octo/repo/issues/42/comments"
        );
        assert_eq!(
            github_pr_review_comments_url("https://api.github.com", "octo", "repo", 42),
            "https://api.github.com/repos/octo/repo/pulls/42/comments"
        );
    }

    fn ctx_with_token() -> ToolContext {
        let dir = tempdir().unwrap();
        ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_github_token("tok")
    }

    // --- merge requires explicit strategy + approval ---

    #[tokio::test]
    async fn merge_pr_without_confirm_is_blocked() {
        let tool = GitHubTool;
        let result = tool
            .execute(
                json!({
                    "action": "merge_pr", "owner": "o", "repo": "r", "pr_number": 1,
                    "merge_method": "squash", "has_scoped_permission": true
                }),
                &ctx_with_token(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("confirm"));
    }

    #[tokio::test]
    async fn merge_pr_without_merge_method_is_blocked() {
        let tool = GitHubTool;
        let result = tool
            .execute(
                json!({
                    "action": "merge_pr", "owner": "o", "repo": "r", "pr_number": 1,
                    "confirm": true, "has_scoped_permission": true
                }),
                &ctx_with_token(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("merge_method"));
    }

    #[tokio::test]
    async fn merge_pr_with_invalid_merge_method_is_blocked() {
        let tool = GitHubTool;
        let result = tool
            .execute(
                json!({
                    "action": "merge_pr", "owner": "o", "repo": "r", "pr_number": 1,
                    "confirm": true, "has_scoped_permission": true, "merge_method": "yolo"
                }),
                &ctx_with_token(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("invalid merge_method"));
    }

    #[tokio::test]
    async fn merge_pr_without_scoped_permission_is_blocked() {
        let tool = GitHubTool;
        let result = tool
            .execute(
                json!({
                    "action": "merge_pr", "owner": "o", "repo": "r", "pr_number": 1,
                    "confirm": true, "merge_method": "merge"
                }),
                &ctx_with_token(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("has_scoped_permission"));
    }

    // --- create-or-reuse and other mutation confirmation gates ---

    #[tokio::test]
    async fn create_pr_without_confirm_is_blocked() {
        let tool = GitHubTool;
        let result = tool
            .execute(
                json!({
                    "action": "create_pr", "owner": "o", "repo": "r",
                    "title": "t", "head": "feature", "base": "main"
                }),
                &ctx_with_token(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("confirm"));
    }

    #[tokio::test]
    async fn update_pr_without_confirm_is_blocked() {
        let tool = GitHubTool;
        let result = tool
            .execute(
                json!({"action": "update_pr", "owner": "o", "repo": "r", "pr_number": 1, "title": "x"}),
                &ctx_with_token(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("confirm"));
    }

    #[tokio::test]
    async fn submit_review_approve_requires_scoped_permission() {
        let tool = GitHubTool;
        let result = tool
            .execute(
                json!({
                    "action": "submit_review", "owner": "o", "repo": "r", "pr_number": 1,
                    "confirm": true, "event": "APPROVE"
                }),
                &ctx_with_token(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("has_scoped_permission"));
    }

    #[tokio::test]
    async fn submit_review_rejects_invalid_event() {
        let tool = GitHubTool;
        let result = tool
            .execute(
                json!({
                    "action": "submit_review", "owner": "o", "repo": "r", "pr_number": 1,
                    "confirm": true, "event": "NONSENSE"
                }),
                &ctx_with_token(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("event must be"));
    }

    // --- bounded diff truncation ---

    #[test]
    fn bound_diff_passes_through_small_diff() {
        let diff = "diff --git a/x b/x\n+hello\n";
        assert_eq!(bound_diff(diff, 1_000), diff);
    }

    #[test]
    fn bound_diff_truncates_large_diff_with_marker() {
        let diff = "a".repeat(1_000);
        let truncated = bound_diff(&diff, 100);
        assert!(truncated.len() < diff.len());
        assert!(truncated.contains("truncated at 100 bytes"));
        assert!(truncated.contains("full diff is 1000 bytes"));
    }

    #[test]
    fn bound_diff_respects_utf8_boundaries() {
        // multi-byte chars around the cut point must not panic and must stay valid utf8.
        let diff = "é".repeat(200); // each 'é' is 2 bytes
        let truncated = bound_diff(&diff, 101);
        assert!(truncated.contains("truncated"));
    }

    // --- schema coverage for new actions ---

    #[test]
    fn github_schema_advertises_pr_lifecycle_actions() {
        let schema = GitHubTool.input_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        for expected in [
            "get_pr",
            "get_pr_diff",
            "resolve_pr_for_branch",
            "create_pr",
            "update_pr",
            "set_pr_draft",
            "add_review_comment",
            "submit_review",
            "resolve_review_thread",
            "merge_pr",
            "get_combined_status",
            "get_check_runs",
            "list_issue_comments",
        ] {
            assert!(
                actions.iter().any(|a| a == expected),
                "missing action {expected}"
            );
        }
    }

    #[test]
    fn confirmed_and_scoped_permission_default_false() {
        assert!(!confirmed(&json!({})));
        assert!(confirmed(&json!({"confirm": true})));
        assert!(!has_scoped_permission(&json!({})));
        assert!(has_scoped_permission(
            &json!({"has_scoped_permission": true})
        ));
    }
}
