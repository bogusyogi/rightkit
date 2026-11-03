//! Full-forge abstraction over GitHub App authority.
//!
//! GitHub remains the only concrete forge today, but every daemon/CLI surface
//! in this module is forge-typed so a second forge (GitLab, Bitbucket, Azure)
//! can be added without changing call sites that currently depend on GitHub
//! App contracts. The mapping is explicit: `ForgeOperation` carries the
//! underlying `GitHubOperation` where a GitHub permission exists, and
//! `Forge::capability` is the admission gate shared by tool dispatch, token
//! minting, and webhook dispatch.

use super::{
    AccessLevel, GitHubAppError, GitHubDenial, GitHubOperation, GitHubPermission,
    InstallationBinding, OperationRegistry, OperationScope, RepositoryScope,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Concrete forge selector. Only `GitHub` is wired today; the other variants
/// are reserved so CLI/daemon parsers reject typos with a typed error rather
/// than falling back to GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Forge {
    GitHub,
    GitLab,
    Bitbucket,
    AzureDevOps,
}

impl Forge {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
            Self::Bitbucket => "bitbucket",
            Self::AzureDevOps => "azure_devops",
        }
    }

    pub fn parse(input: &str) -> Result<Self, String> {
        match input.trim().to_ascii_lowercase().as_str() {
            "github" | "gh" => Ok(Self::GitHub),
            "gitlab" | "gl" => Ok(Self::GitLab),
            "bitbucket" | "bb" => Ok(Self::Bitbucket),
            "azure_devops" | "azure" | "ado" => Ok(Self::AzureDevOps),
            other => Err(format!(
                "unknown forge '{other}'; expected one of github, gitlab, bitbucket, azure_devops"
            )),
        }
    }

    pub const fn is_supported(self) -> bool {
        matches!(self, Self::GitHub)
    }
}

impl std::fmt::Display for Forge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for Forge {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Forge-level operation. The `Git*` variants map 1:1 to `GitHubOperation`
/// so permission + scope checks remain single-sourced in
/// `OperationRegistry`. Forge-only variants (e.g. `GitLabMergeRequestsRead`)
/// return `None` from `github_operation()` until a second forge backend is
/// wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgeOperation {
    // Repository-scoped, GitHub-backed.
    ContentsRead,
    ContentsWrite,
    BranchesRead,
    BranchesWrite,
    CommitsRead,
    CommitsWrite,
    ReleasesRead,
    ReleasesWrite,
    IssuesRead,
    IssuesWrite,
    PullRequestsRead,
    PullRequestsWrite,
    ReviewsRead,
    ReviewsWrite,
    IssueCommentsRead,
    IssueCommentsWrite,
    PullRequestCommentsRead,
    PullRequestCommentsWrite,
    ChecksRead,
    ChecksWrite,
    CommitStatusesRead,
    CommitStatusesWrite,
    ActionsRead,
    ActionsWrite,
    ActionsRunsRead,
    ActionsRunsWrite,
    ActionsArtifactsRead,
    ActionsArtifactsWrite,
    WorkflowsRead,
    WorkflowsWrite,
    DeploymentsRead,
    DeploymentsWrite,
    HooksRead,
    HooksWrite,
    DiscussionsRead,
    DiscussionsWrite,
    MetadataRead,

    // Installation/account-scoped, GitHub-backed.
    ProjectsRead,
    ProjectsWrite,
    NotificationsRead,
    NotificationsWrite,
    OrganizationHooksRead,
    OrganizationHooksWrite,
    WebhookDelivery,
}

impl ForgeOperation {
    pub const fn github_operation(self) -> Option<GitHubOperation> {
        match self {
            Self::ContentsRead => Some(GitHubOperation::ContentsRead),
            Self::ContentsWrite => Some(GitHubOperation::ContentsWrite),
            Self::BranchesRead => Some(GitHubOperation::BranchesRead),
            Self::BranchesWrite => Some(GitHubOperation::BranchesWrite),
            Self::CommitsRead => Some(GitHubOperation::CommitsRead),
            Self::CommitsWrite => Some(GitHubOperation::CommitsWrite),
            Self::ReleasesRead => Some(GitHubOperation::ReleasesRead),
            Self::ReleasesWrite => Some(GitHubOperation::ReleasesWrite),
            Self::IssuesRead => Some(GitHubOperation::IssuesRead),
            Self::IssuesWrite => Some(GitHubOperation::IssuesWrite),
            Self::PullRequestsRead => Some(GitHubOperation::PullRequestsRead),
            Self::PullRequestsWrite => Some(GitHubOperation::PullRequestsWrite),
            Self::ReviewsRead => Some(GitHubOperation::ReviewsRead),
            Self::ReviewsWrite => Some(GitHubOperation::ReviewsWrite),
            Self::IssueCommentsRead => Some(GitHubOperation::IssueCommentsRead),
            Self::IssueCommentsWrite => Some(GitHubOperation::IssueCommentsWrite),
            Self::PullRequestCommentsRead => Some(GitHubOperation::PullRequestCommentsRead),
            Self::PullRequestCommentsWrite => Some(GitHubOperation::PullRequestCommentsWrite),
            Self::ChecksRead => Some(GitHubOperation::ChecksRead),
            Self::ChecksWrite => Some(GitHubOperation::ChecksWrite),
            Self::CommitStatusesRead => Some(GitHubOperation::CommitStatusesRead),
            Self::CommitStatusesWrite => Some(GitHubOperation::CommitStatusesWrite),
            Self::ActionsRead => Some(GitHubOperation::ActionsRead),
            Self::ActionsWrite => Some(GitHubOperation::ActionsWrite),
            Self::ActionsRunsRead => Some(GitHubOperation::ActionsRunsRead),
            Self::ActionsRunsWrite => Some(GitHubOperation::ActionsRunsWrite),
            Self::ActionsArtifactsRead => Some(GitHubOperation::ActionsArtifactsRead),
            Self::ActionsArtifactsWrite => Some(GitHubOperation::ActionsArtifactsWrite),
            Self::WorkflowsRead => Some(GitHubOperation::WorkflowsRead),
            Self::WorkflowsWrite => Some(GitHubOperation::WorkflowsWrite),
            Self::DeploymentsRead => Some(GitHubOperation::DeploymentsRead),
            Self::DeploymentsWrite => Some(GitHubOperation::DeploymentsWrite),
            Self::HooksRead => Some(GitHubOperation::HooksRead),
            Self::HooksWrite => Some(GitHubOperation::HooksWrite),
            Self::DiscussionsRead => Some(GitHubOperation::DiscussionsRead),
            Self::DiscussionsWrite => Some(GitHubOperation::DiscussionsWrite),
            Self::MetadataRead => Some(GitHubOperation::MetadataRead),
            Self::ProjectsRead => Some(GitHubOperation::ProjectsRead),
            Self::ProjectsWrite => Some(GitHubOperation::ProjectsWrite),
            Self::NotificationsRead => Some(GitHubOperation::NotificationsRead),
            Self::NotificationsWrite => Some(GitHubOperation::NotificationsWrite),
            Self::OrganizationHooksRead => Some(GitHubOperation::OrganizationHooksRead),
            Self::OrganizationHooksWrite => Some(GitHubOperation::OrganizationHooksWrite),
            Self::WebhookDelivery => Some(GitHubOperation::WebhookDelivery),
        }
    }

    pub const fn scope(self) -> OperationScope {
        match self.github_operation() {
            Some(operation) => OperationRegistry::scope(operation),
            None => OperationScope::Repository,
        }
    }

    pub const fn needs_repository(self) -> bool {
        matches!(self.scope(), OperationScope::Repository)
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::ContentsRead,
            Self::ContentsWrite,
            Self::BranchesRead,
            Self::BranchesWrite,
            Self::CommitsRead,
            Self::CommitsWrite,
            Self::ReleasesRead,
            Self::ReleasesWrite,
            Self::IssuesRead,
            Self::IssuesWrite,
            Self::PullRequestsRead,
            Self::PullRequestsWrite,
            Self::ReviewsRead,
            Self::ReviewsWrite,
            Self::IssueCommentsRead,
            Self::IssueCommentsWrite,
            Self::PullRequestCommentsRead,
            Self::PullRequestCommentsWrite,
            Self::ChecksRead,
            Self::ChecksWrite,
            Self::CommitStatusesRead,
            Self::CommitStatusesWrite,
            Self::ActionsRead,
            Self::ActionsWrite,
            Self::ActionsRunsRead,
            Self::ActionsRunsWrite,
            Self::ActionsArtifactsRead,
            Self::ActionsArtifactsWrite,
            Self::WorkflowsRead,
            Self::WorkflowsWrite,
            Self::DeploymentsRead,
            Self::DeploymentsWrite,
            Self::HooksRead,
            Self::HooksWrite,
            Self::DiscussionsRead,
            Self::DiscussionsWrite,
            Self::MetadataRead,
            Self::ProjectsRead,
            Self::ProjectsWrite,
            Self::NotificationsRead,
            Self::NotificationsWrite,
            Self::OrganizationHooksRead,
            Self::OrganizationHooksWrite,
            Self::WebhookDelivery,
        ]
    }
}

/// Forge-bound installation identity. GitHub installations carry the concrete
/// `InstallationBinding`; other forges are reserved for future backends and
/// fail closed with `UnsupportedForge`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "forge")]
pub enum ForgeBinding {
    GitHub(InstallationBinding),
    Unsupported {
        #[serde(rename = "forge_kind")]
        forge: Forge,
        reason: String,
    },
}

impl ForgeBinding {
    pub fn forge(&self) -> Forge {
        match self {
            Self::GitHub(_) => Forge::GitHub,
            Self::Unsupported { forge, .. } => *forge,
        }
    }

    pub fn as_github(&self) -> Result<&InstallationBinding, GitHubAppError> {
        match self {
            Self::GitHub(binding) => Ok(binding),
            Self::Unsupported { forge, reason } => Err(GitHubAppError::Adapter(format!(
                "forge {forge} is not supported: {reason}"
            ))),
        }
    }

    pub fn authorize(
        &self,
        operation: ForgeOperation,
        repository_id: Option<u64>,
    ) -> Result<(), GitHubAppError> {
        let github_operation = operation.github_operation().ok_or_else(|| {
            GitHubAppError::denied(GitHubDenial::UnsupportedOperation {
                operation: GitHubOperation::WebhookDelivery,
            })
        })?;
        self.as_github()?.authorize(github_operation, repository_id)
    }
}

/// Registry helper that preserves the single permission mapping while exposing
/// a forge-aware capability check.
pub struct ForgeRegistry;

impl ForgeRegistry {
    pub fn authorize(
        binding: &InstallationBinding,
        forge: Forge,
        operation: ForgeOperation,
        repository_id: Option<u64>,
    ) -> Result<(), GitHubAppError> {
        if forge != Forge::GitHub {
            return Err(GitHubAppError::Adapter(format!(
                "forge {forge} is not yet wired; only github is supported"
            )));
        }
        let github_operation = operation.github_operation().ok_or_else(|| {
            GitHubAppError::denied(GitHubDenial::UnsupportedOperation {
                operation: GitHubOperation::WebhookDelivery,
            })
        })?;
        binding.authorize(github_operation, repository_id)
    }

    pub fn permission_for(
        operation: ForgeOperation,
    ) -> Result<(GitHubPermission, AccessLevel), GitHubAppError> {
        let github_operation = operation.github_operation().ok_or_else(|| {
            GitHubAppError::denied(GitHubDenial::UnsupportedOperation {
                operation: GitHubOperation::WebhookDelivery,
            })
        })?;
        OperationRegistry::required(github_operation).ok_or_else(|| {
            GitHubAppError::denied(GitHubDenial::UnsupportedOperation {
                operation: github_operation,
            })
        })
    }

    pub fn forge_for_github_operation(operation: GitHubOperation) -> ForgeOperation {
        match operation {
            GitHubOperation::ContentsRead => ForgeOperation::ContentsRead,
            GitHubOperation::ContentsWrite => ForgeOperation::ContentsWrite,
            GitHubOperation::BranchesRead => ForgeOperation::BranchesRead,
            GitHubOperation::BranchesWrite => ForgeOperation::BranchesWrite,
            GitHubOperation::CommitsRead => ForgeOperation::CommitsRead,
            GitHubOperation::CommitsWrite => ForgeOperation::CommitsWrite,
            GitHubOperation::ReleasesRead => ForgeOperation::ReleasesRead,
            GitHubOperation::ReleasesWrite => ForgeOperation::ReleasesWrite,
            GitHubOperation::IssuesRead => ForgeOperation::IssuesRead,
            GitHubOperation::IssuesWrite => ForgeOperation::IssuesWrite,
            GitHubOperation::PullRequestsRead => ForgeOperation::PullRequestsRead,
            GitHubOperation::PullRequestsWrite => ForgeOperation::PullRequestsWrite,
            GitHubOperation::ReviewsRead => ForgeOperation::ReviewsRead,
            GitHubOperation::ReviewsWrite => ForgeOperation::ReviewsWrite,
            GitHubOperation::IssueCommentsRead => ForgeOperation::IssueCommentsRead,
            GitHubOperation::IssueCommentsWrite => ForgeOperation::IssueCommentsWrite,
            GitHubOperation::PullRequestCommentsRead => ForgeOperation::PullRequestCommentsRead,
            GitHubOperation::PullRequestCommentsWrite => ForgeOperation::PullRequestCommentsWrite,
            GitHubOperation::ChecksRead => ForgeOperation::ChecksRead,
            GitHubOperation::ChecksWrite => ForgeOperation::ChecksWrite,
            GitHubOperation::CommitStatusesRead => ForgeOperation::CommitStatusesRead,
            GitHubOperation::CommitStatusesWrite => ForgeOperation::CommitStatusesWrite,
            GitHubOperation::ActionsRead => ForgeOperation::ActionsRead,
            GitHubOperation::ActionsWrite => ForgeOperation::ActionsWrite,
            GitHubOperation::ActionsRunsRead => ForgeOperation::ActionsRunsRead,
            GitHubOperation::ActionsRunsWrite => ForgeOperation::ActionsRunsWrite,
            GitHubOperation::ActionsArtifactsRead => ForgeOperation::ActionsArtifactsRead,
            GitHubOperation::ActionsArtifactsWrite => ForgeOperation::ActionsArtifactsWrite,
            GitHubOperation::WorkflowsRead => ForgeOperation::WorkflowsRead,
            GitHubOperation::WorkflowsWrite => ForgeOperation::WorkflowsWrite,
            GitHubOperation::DeploymentsRead => ForgeOperation::DeploymentsRead,
            GitHubOperation::DeploymentsWrite => ForgeOperation::DeploymentsWrite,
            GitHubOperation::ProjectsRead => ForgeOperation::ProjectsRead,
            GitHubOperation::ProjectsWrite => ForgeOperation::ProjectsWrite,
            GitHubOperation::NotificationsRead => ForgeOperation::NotificationsRead,
            GitHubOperation::NotificationsWrite => ForgeOperation::NotificationsWrite,
            GitHubOperation::HooksRead => ForgeOperation::HooksRead,
            GitHubOperation::HooksWrite => ForgeOperation::HooksWrite,
            GitHubOperation::OrganizationHooksRead => ForgeOperation::OrganizationHooksRead,
            GitHubOperation::OrganizationHooksWrite => ForgeOperation::OrganizationHooksWrite,
            GitHubOperation::DiscussionsRead => ForgeOperation::DiscussionsRead,
            GitHubOperation::DiscussionsWrite => ForgeOperation::DiscussionsWrite,
            GitHubOperation::MetadataRead => ForgeOperation::MetadataRead,
            GitHubOperation::WebhookDelivery => ForgeOperation::WebhookDelivery,
        }
    }
}

/// Helpers for narrowing repository scope exactly like the GitHub store does,
/// but through the forge-typed operation.
pub fn repository_ids_for_forge_operation(
    binding: &InstallationBinding,
    operation: ForgeOperation,
    repository_ids: &[u64],
) -> Result<Vec<u64>, GitHubAppError> {
    let github_operation = operation.github_operation().ok_or_else(|| {
        GitHubAppError::denied(GitHubDenial::UnsupportedOperation {
            operation: GitHubOperation::WebhookDelivery,
        })
    })?;
    binding.repository_ids_for_token(github_operation, repository_ids)
}

/// Convenience for constructing an explicit repository scope from raw IDs.
pub fn repository_scope_from_ids(ids: &[u64]) -> Result<RepositoryScope, GitHubAppError> {
    if ids.is_empty() {
        return Ok(RepositoryScope::AllGranted);
    }
    let mut unique = BTreeSet::new();
    for id in ids {
        if *id == 0 {
            return Err(GitHubAppError::InvalidBinding(
                "repository IDs must be non-zero".to_string(),
            ));
        }
        unique.insert(*id);
    }
    Ok(RepositoryScope::Explicit(unique))
}

/// Default permission map for a single forge operation (one entry, same shape
/// the token path expects).
pub fn permissions_for_forge_operation(
    operation: ForgeOperation,
) -> Result<BTreeMap<GitHubPermission, AccessLevel>, GitHubAppError> {
    let (permission, level) = ForgeRegistry::permission_for(operation)?;
    Ok(BTreeMap::from([(permission, level)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_app::{AccessLevel, GitHubPermission, RepositoryScope};
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    fn binding() -> InstallationBinding {
        InstallationBinding::new(
            7,
            9,
            Some(11),
            "acme",
            RepositoryScope::Explicit(BTreeSet::from([42])),
            BTreeMap::from([(GitHubPermission::Contents, AccessLevel::Read)]),
            1,
        )
        .unwrap()
    }

    #[test]
    fn forge_parse_accepts_aliases_and_rejects_unknown() {
        assert_eq!(Forge::parse("github").unwrap(), Forge::GitHub);
        assert_eq!(Forge::parse("GH").unwrap(), Forge::GitHub);
        assert_eq!(Forge::parse("gitlab").unwrap(), Forge::GitLab);
        assert!(Forge::parse("unknown").is_err());
    }

    #[test]
    fn only_github_is_supported_on_this_lane() {
        assert!(Forge::GitHub.is_supported());
        assert!(!Forge::GitLab.is_supported());
    }

    #[test]
    fn every_forge_operation_maps_to_a_github_operation() {
        for operation in ForgeOperation::all() {
            assert!(
                operation.github_operation().is_some(),
                "forge operation has no github mapping: {operation:?}"
            );
        }
    }

    #[test]
    fn forge_authorize_mirrors_github_binding() {
        let binding = binding();
        let forge_binding = ForgeBinding::GitHub(binding);
        assert!(forge_binding
            .authorize(ForgeOperation::ContentsRead, Some(42))
            .is_ok());
        assert!(matches!(
            forge_binding.authorize(ForgeOperation::ContentsRead, Some(43)),
            Err(GitHubAppError::Denied(GitHubDenial::RepositoryOutOfScope {
                repository_id: 43
            }))
        ));
        assert!(matches!(
            ForgeRegistry::authorize(
                forge_binding.as_github().unwrap(),
                Forge::GitLab,
                ForgeOperation::ContentsRead,
                Some(42)
            ),
            Err(GitHubAppError::Adapter(_))
        ));
    }

    #[test]
    fn repository_scope_helpers_deduplicate_and_validate() {
        assert_eq!(
            repository_ids_for_forge_operation(&binding(), ForgeOperation::ContentsRead, &[42, 42])
                .unwrap(),
            vec![42]
        );
        assert!(matches!(
            repository_scope_from_ids(&[0]),
            Err(GitHubAppError::InvalidBinding(_))
        ));
    }
}
