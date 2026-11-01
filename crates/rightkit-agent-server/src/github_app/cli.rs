//! CLI integration for the GitHub App / full-forge lane.
//!
//! The daemon remains the authority for installation state and token minting;
//! this module provides the `coderight github` command shapes and the
//! client-side validation that runs before any daemon call. No secret material
//! is accepted as a CLI argument — secrets are resolved from named environment
//! variables at daemon startup, matching `api_triggers.rs`.

use super::{
    forge::{Forge, ForgeOperation},
    GitHubAppError, GitHubDenial, GitHubOperation,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level `coderight github` subcommand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitHubCliCommand {
    App(GitHubAppCommand),
    Forge(ForgeCommand),
}

/// `coderight github app ...` — installation + token + webhook helpers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitHubAppCommand {
    /// Install or replace a GitHub App installation binding.
    Install(InstallArgs),
    /// Replace an existing installation (generation must advance).
    Replace(InstallArgs),
    /// Revoke (disable) an installation.
    Revoke(RevokeArgs),
    /// Check whether an operation is authorized for a repository.
    Authorize(AuthorizeArgs),
    /// Issue an installation token for a forge operation.
    IssueToken(IssueTokenArgs),
    /// Revoke a cached installation token.
    RevokeToken(RevokeTokenArgs),
    /// Verify a webhook payload against the configured secret (offline).
    VerifyWebhook(VerifyWebhookArgs),
    /// Show installation state from the daemon.
    Status(StatusArgs),
}

/// `coderight github forge ...` — forge-aware helpers that wrap the same
/// authority but expose the `forge` selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForgeCommand {
    /// List supported forges and their wire status.
    List,
    /// Check whether a forge operation is supported.
    Check(ForgeCheckArgs),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallArgs {
    pub app_id: u64,
    pub installation_id: u64,
    pub account_login: String,
    pub account_id: Option<u64>,
    pub repository_ids: Vec<u64>,
    pub permissions: Vec<PermissionArg>,
    pub generation: u64,
    pub state_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeArgs {
    pub app_id: u64,
    pub installation_id: u64,
    pub expected_generation: Option<u64>,
    pub state_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizeArgs {
    pub app_id: u64,
    pub installation_id: u64,
    pub forge: Option<String>,
    pub operation: String,
    pub repository_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueTokenArgs {
    pub app_id: u64,
    pub installation_id: u64,
    pub forge: Option<String>,
    pub operation: String,
    pub repository_ids: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeTokenArgs {
    pub app_id: u64,
    pub installation_id: u64,
    pub operation: String,
    pub repository_ids: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyWebhookArgs {
    pub app_id: u64,
    pub secret_env: String,
    pub delivery_id: String,
    pub event: String,
    pub signature_256: String,
    pub body_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusArgs {
    pub app_id: u64,
    pub installation_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeCheckArgs {
    pub forge: String,
    pub operation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionArg {
    pub permission: String,
    pub level: String,
}

impl PermissionArg {
    pub fn parse(&self) -> Result<(super::GitHubPermission, super::AccessLevel), String> {
        let permission = match self.permission.trim().to_ascii_lowercase().as_str() {
            "metadata" => super::GitHubPermission::Metadata,
            "contents" => super::GitHubPermission::Contents,
            "issues" => super::GitHubPermission::Issues,
            "pull_requests" | "pull-requests" | "pullrequests" => {
                super::GitHubPermission::PullRequests
            }
            "checks" => super::GitHubPermission::Checks,
            "actions" => super::GitHubPermission::Actions,
            "workflows" => super::GitHubPermission::Workflows,
            "discussions" => super::GitHubPermission::Discussions,
            "commit_statuses" | "commit-statuses" | "statuses" => {
                super::GitHubPermission::CommitStatuses
            }
            "deployments" => super::GitHubPermission::Deployments,
            "repository_hooks" | "hooks" => super::GitHubPermission::RepositoryHooks,
            "organization_hooks" => super::GitHubPermission::OrganizationHooks,
            "organization_projects" | "projects" => {
                super::GitHubPermission::OrganizationProjects
            }
            "notifications" => super::GitHubPermission::Notifications,
            other => {
                return Err(format!(
                    "unknown permission '{other}'; expected contents, issues, pull_requests, checks, actions, workflows, metadata, etc."
                ))
            }
        };
        let level = match self.level.trim().to_ascii_lowercase().as_str() {
            "read" => super::AccessLevel::Read,
            "write" => super::AccessLevel::Write,
            "none" => super::AccessLevel::None,
            other => {
                return Err(format!(
                    "unknown access level '{other}'; expected read, write, or none"
                ))
            }
        };
        Ok((permission, level))
    }
}

/// Parse a user-supplied operation string (CLI or API) into a typed
/// `ForgeOperation`. Accepts both `snake_case` and `kebab-case`.
pub fn parse_forge_operation(input: &str) -> Result<ForgeOperation, String> {
    let normalized = input.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "contents_read" => Ok(ForgeOperation::ContentsRead),
        "contents_write" => Ok(ForgeOperation::ContentsWrite),
        "branches_read" => Ok(ForgeOperation::BranchesRead),
        "branches_write" => Ok(ForgeOperation::BranchesWrite),
        "commits_read" => Ok(ForgeOperation::CommitsRead),
        "commits_write" => Ok(ForgeOperation::CommitsWrite),
        "releases_read" => Ok(ForgeOperation::ReleasesRead),
        "releases_write" => Ok(ForgeOperation::ReleasesWrite),
        "issues_read" => Ok(ForgeOperation::IssuesRead),
        "issues_write" => Ok(ForgeOperation::IssuesWrite),
        "pull_requests_read" | "pullrequests_read" => Ok(ForgeOperation::PullRequestsRead),
        "pull_requests_write" | "pullrequests_write" => Ok(ForgeOperation::PullRequestsWrite),
        "reviews_read" => Ok(ForgeOperation::ReviewsRead),
        "reviews_write" => Ok(ForgeOperation::ReviewsWrite),
        "issue_comments_read" => Ok(ForgeOperation::IssueCommentsRead),
        "issue_comments_write" => Ok(ForgeOperation::IssueCommentsWrite),
        "pull_request_comments_read" | "pr_comments_read" => {
            Ok(ForgeOperation::PullRequestCommentsRead)
        }
        "pull_request_comments_write" | "pr_comments_write" => {
            Ok(ForgeOperation::PullRequestCommentsWrite)
        }
        "checks_read" => Ok(ForgeOperation::ChecksRead),
        "checks_write" => Ok(ForgeOperation::ChecksWrite),
        "commit_statuses_read" | "statuses_read" => Ok(ForgeOperation::CommitStatusesRead),
        "commit_statuses_write" | "statuses_write" => Ok(ForgeOperation::CommitStatusesWrite),
        "actions_read" => Ok(ForgeOperation::ActionsRead),
        "actions_write" => Ok(ForgeOperation::ActionsWrite),
        "actions_runs_read" => Ok(ForgeOperation::ActionsRunsRead),
        "actions_runs_write" => Ok(ForgeOperation::ActionsRunsWrite),
        "actions_artifacts_read" => Ok(ForgeOperation::ActionsArtifactsRead),
        "actions_artifacts_write" => Ok(ForgeOperation::ActionsArtifactsWrite),
        "workflows_read" => Ok(ForgeOperation::WorkflowsRead),
        "workflows_write" => Ok(ForgeOperation::WorkflowsWrite),
        "deployments_read" => Ok(ForgeOperation::DeploymentsRead),
        "deployments_write" => Ok(ForgeOperation::DeploymentsWrite),
        "projects_read" => Ok(ForgeOperation::ProjectsRead),
        "projects_write" => Ok(ForgeOperation::ProjectsWrite),
        "notifications_read" => Ok(ForgeOperation::NotificationsRead),
        "notifications_write" => Ok(ForgeOperation::NotificationsWrite),
        "hooks_read" => Ok(ForgeOperation::HooksRead),
        "hooks_write" => Ok(ForgeOperation::HooksWrite),
        "organization_hooks_read" | "org_hooks_read" => {
            Ok(ForgeOperation::OrganizationHooksRead)
        }
        "organization_hooks_write" | "org_hooks_write" => {
            Ok(ForgeOperation::OrganizationHooksWrite)
        }
        "discussions_read" => Ok(ForgeOperation::DiscussionsRead),
        "discussions_write" => Ok(ForgeOperation::DiscussionsWrite),
        "metadata_read" => Ok(ForgeOperation::MetadataRead),
        "webhook_delivery" => Ok(ForgeOperation::WebhookDelivery),
        other => Err(format!(
            "unknown operation '{other}'; expected e.g. contents_read, issues_write, pull_requests_read"
        )),
    }
}

/// Parse a `Forge` selector from an optional CLI string. `None` means
/// GitHub (the default for every existing caller).
pub fn parse_forge(input: Option<&str>) -> Result<Forge, String> {
    match input {
        None | Some("") => Ok(Forge::GitHub),
        Some(value) => Forge::parse(value),
    }
}

/// Render a `GitHubAppError` / `GitHubDenial` for CLI output without leaking
/// secret material. The denial's JSON shape is stable and already redacted.
pub fn format_cli_error(error: &GitHubAppError) -> String {
    match error {
        GitHubAppError::Denied(denial) => {
            let json = serde_json::to_string(denial).unwrap_or_else(|_| format!("{denial:?}"));
            format!("denied: {json}")
        }
        GitHubAppError::InvalidBinding(reason) => format!("invalid binding: {reason}"),
        GitHubAppError::InvalidToken(reason) => format!("invalid token: {reason}"),
        GitHubAppError::Adapter(reason) => format!("adapter failure: {reason}"),
    }
}

/// One-line help for `coderight github --help`.
pub fn help_text() -> &'static str {
    "coderight github — GitHub App and full-forge integration\n\
     \n\
     USAGE:\n  \
     coderight github app install --app-id <id> --installation-id <id> --account-login <login> --generation <n> [--repository-ids <ids>] [--permissions <perm:level>...]\n  \
     coderight github app revoke --app-id <id> --installation-id <id> [--expected-generation <n>]\n  \
     coderight github app authorize --app-id <id> --installation-id <id> --operation <op> [--repository-id <id>] [--forge github]\n  \
     coderight github app issue-token --app-id <id> --installation-id <id> --operation <op> --repository-ids <ids> [--forge github]\n  \
     coderight github app revoke-token --app-id <id> --installation-id <id> --operation <op> --repository-ids <ids>\n  \
     coderight github app verify-webhook --app-id <id> --secret-env <ENV> --delivery-id <id> --event <event> --signature-256 <sig> --body-path <path>\n  \
     coderight github forge list\n  \
     coderight github forge check --forge <forge> --operation <op>\n\
     \n\
     OPERATIONS:\n  \
     contents_read/write, branches_read/write, issues_read/write,\n  \
     pull_requests_read/write, checks_read/write, actions_read/write, etc.\n\
     \n\
     FORGES:\n  \
     github (supported), gitlab, bitbucket, azure_devops (reserved)\n\
     \n\
     NOTES:\n  \
     Webhook secrets are never accepted as CLI arguments; set the named\n  \
     environment variable before starting the daemon and pass --secret-env.\n"
}

/// Machine-readable status for `coderight github forge list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeListEntry {
    pub forge: Forge,
    pub supported: bool,
    pub api_base: String,
}

pub fn forge_list() -> Vec<ForgeListEntry> {
    [
        Forge::GitHub,
        Forge::GitLab,
        Forge::Bitbucket,
        Forge::AzureDevOps,
    ]
    .into_iter()
    .map(|forge| ForgeListEntry {
        forge,
        supported: forge.is_supported(),
        api_base: super::daemon::forge_api_base(forge).to_string(),
    })
    .collect()
}

/// Validate that a forge + operation pair is supported on this lane.
pub fn check_forge_operation(
    forge: Forge,
    operation: ForgeOperation,
) -> Result<(), GitHubAppError> {
    if !forge.is_supported() {
        return Err(GitHubAppError::Adapter(format!(
            "forge {forge} is reserved; only github is wired on this lane"
        )));
    }
    // Ensure the operation has a GitHub mapping (all current forge operations
    // do, but this keeps the check explicit for future-only operations).
    operation.github_operation().ok_or_else(|| {
        GitHubAppError::denied(GitHubDenial::UnsupportedOperation {
            operation: GitHubOperation::WebhookDelivery,
        })
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_parse_accepts_kebab_and_snake() {
        assert_eq!(
            parse_forge_operation("contents_read").unwrap(),
            ForgeOperation::ContentsRead
        );
        assert_eq!(
            parse_forge_operation("contents-read").unwrap(),
            ForgeOperation::ContentsRead
        );
        assert_eq!(
            parse_forge_operation("pull-requests-read").unwrap(),
            ForgeOperation::PullRequestsRead
        );
    }

    #[test]
    fn permission_arg_parses_levels() {
        let arg = PermissionArg {
            permission: "contents".to_string(),
            level: "read".to_string(),
        };
        let (permission, level) = arg.parse().unwrap();
        assert_eq!(permission, super::super::GitHubPermission::Contents);
        assert_eq!(level, super::super::AccessLevel::Read);
    }

    #[test]
    fn forge_list_marks_only_github_supported() {
        let entries = forge_list();
        assert_eq!(entries.len(), 4);
        assert!(
            entries
                .iter()
                .find(|entry| entry.forge == Forge::GitHub)
                .unwrap()
                .supported
        );
        assert!(
            !entries
                .iter()
                .find(|entry| entry.forge == Forge::GitLab)
                .unwrap()
                .supported
        );
    }

    #[test]
    fn check_forge_operation_rejects_reserved_forges() {
        assert!(check_forge_operation(Forge::GitHub, ForgeOperation::ContentsRead).is_ok());
        assert!(matches!(
            check_forge_operation(Forge::GitLab, ForgeOperation::ContentsRead),
            Err(GitHubAppError::Adapter(_))
        ));
    }

    #[test]
    fn error_formatting_does_not_leak_token() {
        let error = GitHubAppError::Denied(GitHubDenial::InvalidSignature);
        let rendered = format_cli_error(&error);
        assert!(rendered.contains("invalid_signature"));
        assert!(!rendered.contains("ghs_"));
    }
}
