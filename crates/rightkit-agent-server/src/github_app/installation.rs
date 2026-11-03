use super::{GitHubAppError, GitHubDenial};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// GitHub App permission families used by RightKit operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubPermission {
    Metadata,
    Contents,
    Issues,
    PullRequests,
    Checks,
    Actions,
    Workflows,
    Discussions,
    CommitStatuses,
    Deployments,
    RepositoryHooks,
    OrganizationHooks,
    OrganizationProjects,
    Notifications,
}

/// Permission level granted by an installation or requested for a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    None,
    Read,
    Write,
}

impl AccessLevel {
    pub const fn allows(self, required: Self) -> bool {
        matches!(
            (self, required),
            (Self::Write, Self::Read | Self::Write)
                | (Self::Read, Self::Read)
                | (Self::None, Self::None)
        )
    }
}

/// Operation registry key. Registry entries are the single permission mapping
/// used by both admission and token requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubOperation {
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
    ProjectsRead,
    ProjectsWrite,
    NotificationsRead,
    NotificationsWrite,
    HooksRead,
    HooksWrite,
    OrganizationHooksRead,
    OrganizationHooksWrite,
    DiscussionsRead,
    DiscussionsWrite,
    MetadataRead,
    WebhookDelivery,
}

/// Authority boundary for an operation's selector. Account- and
/// installation-scoped operations must not be narrowed with repository IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationScope {
    Repository,
    Installation,
    Account,
}

/// Compile-time operation-to-permission registry, kept explicit for audit and
/// future route adapters.
pub struct OperationRegistry;

impl OperationRegistry {
    /// Every supported operation appears exactly once in this audit list.
    pub const ALL: &'static [GitHubOperation] = &[
        GitHubOperation::ContentsRead,
        GitHubOperation::ContentsWrite,
        GitHubOperation::BranchesRead,
        GitHubOperation::BranchesWrite,
        GitHubOperation::CommitsRead,
        GitHubOperation::CommitsWrite,
        GitHubOperation::ReleasesRead,
        GitHubOperation::ReleasesWrite,
        GitHubOperation::IssuesRead,
        GitHubOperation::IssuesWrite,
        GitHubOperation::PullRequestsRead,
        GitHubOperation::PullRequestsWrite,
        GitHubOperation::ReviewsRead,
        GitHubOperation::ReviewsWrite,
        GitHubOperation::IssueCommentsRead,
        GitHubOperation::IssueCommentsWrite,
        GitHubOperation::PullRequestCommentsRead,
        GitHubOperation::PullRequestCommentsWrite,
        GitHubOperation::ChecksRead,
        GitHubOperation::ChecksWrite,
        GitHubOperation::CommitStatusesRead,
        GitHubOperation::CommitStatusesWrite,
        GitHubOperation::ActionsRead,
        GitHubOperation::ActionsWrite,
        GitHubOperation::ActionsRunsRead,
        GitHubOperation::ActionsRunsWrite,
        GitHubOperation::ActionsArtifactsRead,
        GitHubOperation::ActionsArtifactsWrite,
        GitHubOperation::WorkflowsRead,
        GitHubOperation::WorkflowsWrite,
        GitHubOperation::DeploymentsRead,
        GitHubOperation::DeploymentsWrite,
        GitHubOperation::ProjectsRead,
        GitHubOperation::ProjectsWrite,
        GitHubOperation::NotificationsRead,
        GitHubOperation::NotificationsWrite,
        GitHubOperation::HooksRead,
        GitHubOperation::HooksWrite,
        GitHubOperation::OrganizationHooksRead,
        GitHubOperation::OrganizationHooksWrite,
        GitHubOperation::DiscussionsRead,
        GitHubOperation::DiscussionsWrite,
        GitHubOperation::MetadataRead,
        GitHubOperation::WebhookDelivery,
    ];

    pub const fn operations() -> &'static [GitHubOperation] {
        Self::ALL
    }

    pub const fn required(operation: GitHubOperation) -> Option<(GitHubPermission, AccessLevel)> {
        use AccessLevel::{Read, Write};
        use GitHubOperation::*;
        use GitHubPermission::*;
        match operation {
            ContentsRead => Some((Contents, Read)),
            ContentsWrite => Some((Contents, Write)),
            BranchesRead | CommitsRead | ReleasesRead => Some((Contents, Read)),
            BranchesWrite | CommitsWrite | ReleasesWrite => Some((Contents, Write)),
            IssuesRead | IssueCommentsRead => Some((Issues, Read)),
            IssuesWrite | IssueCommentsWrite => Some((Issues, Write)),
            PullRequestsRead | ReviewsRead | PullRequestCommentsRead => Some((PullRequests, Read)),
            PullRequestsWrite | ReviewsWrite | PullRequestCommentsWrite => {
                Some((PullRequests, Write))
            }
            ChecksRead => Some((Checks, Read)),
            ChecksWrite => Some((Checks, Write)),
            CommitStatusesRead => Some((CommitStatuses, Read)),
            CommitStatusesWrite => Some((CommitStatuses, Write)),
            ActionsRead | ActionsRunsRead | ActionsArtifactsRead => Some((Actions, Read)),
            ActionsWrite | ActionsRunsWrite | ActionsArtifactsWrite => Some((Actions, Write)),
            WorkflowsRead => Some((Workflows, Read)),
            WorkflowsWrite => Some((Workflows, Write)),
            DeploymentsRead => Some((Deployments, Read)),
            DeploymentsWrite => Some((Deployments, Write)),
            ProjectsRead => Some((OrganizationProjects, Read)),
            ProjectsWrite => Some((OrganizationProjects, Write)),
            NotificationsRead => Some((Notifications, Read)),
            NotificationsWrite => Some((Notifications, Write)),
            HooksRead => Some((RepositoryHooks, Read)),
            HooksWrite => Some((RepositoryHooks, Write)),
            OrganizationHooksRead => Some((OrganizationHooks, Read)),
            OrganizationHooksWrite => Some((OrganizationHooks, Write)),
            DiscussionsRead => Some((Discussions, Read)),
            DiscussionsWrite => Some((Discussions, Write)),
            MetadataRead => Some((Metadata, Read)),
            WebhookDelivery => None,
        }
    }

    pub const fn scope(operation: GitHubOperation) -> OperationScope {
        use GitHubOperation::*;
        match operation {
            ProjectsRead
            | ProjectsWrite
            | OrganizationHooksRead
            | OrganizationHooksWrite
            | WebhookDelivery => OperationScope::Installation,
            NotificationsRead | NotificationsWrite => OperationScope::Account,
            ContentsRead
            | ContentsWrite
            | BranchesRead
            | BranchesWrite
            | CommitsRead
            | CommitsWrite
            | ReleasesRead
            | ReleasesWrite
            | IssuesRead
            | IssuesWrite
            | PullRequestsRead
            | PullRequestsWrite
            | ReviewsRead
            | ReviewsWrite
            | IssueCommentsRead
            | IssueCommentsWrite
            | PullRequestCommentsRead
            | PullRequestCommentsWrite
            | ChecksRead
            | ChecksWrite
            | CommitStatusesRead
            | CommitStatusesWrite
            | ActionsRead
            | ActionsWrite
            | ActionsRunsRead
            | ActionsRunsWrite
            | ActionsArtifactsRead
            | ActionsArtifactsWrite
            | WorkflowsRead
            | WorkflowsWrite
            | DeploymentsRead
            | DeploymentsWrite
            | HooksRead
            | HooksWrite
            | DiscussionsRead
            | DiscussionsWrite
            | MetadataRead => OperationScope::Repository,
        }
    }

    pub const fn needs_repository(operation: GitHubOperation) -> bool {
        matches!(Self::scope(operation), OperationScope::Repository)
    }
}

/// Repository scope granted to one App installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "repository_ids")]
pub enum RepositoryScope {
    AllGranted,
    Explicit(BTreeSet<u64>),
}

impl RepositoryScope {
    pub fn contains(&self, repository_id: u64) -> bool {
        match self {
            Self::AllGranted => true,
            Self::Explicit(ids) => ids.contains(&repository_id),
        }
    }
}

/// Immutable identity and authority snapshot for one installed App.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationBinding {
    pub app_id: u64,
    pub installation_id: u64,
    pub account_id: Option<u64>,
    pub account_login: String,
    pub repository_scope: RepositoryScope,
    pub permissions: BTreeMap<GitHubPermission, AccessLevel>,
    pub generation: u64,
    pub enabled: bool,
}

impl InstallationBinding {
    pub fn new(
        app_id: u64,
        installation_id: u64,
        account_id: Option<u64>,
        account_login: impl Into<String>,
        repository_scope: RepositoryScope,
        permissions: BTreeMap<GitHubPermission, AccessLevel>,
        generation: u64,
    ) -> Result<Self, GitHubAppError> {
        let account_login = account_login.into();
        if app_id == 0 || installation_id == 0 {
            return Err(GitHubAppError::InvalidBinding(
                "app_id and installation_id must be non-zero".to_string(),
            ));
        }
        if account_login.trim().is_empty() {
            return Err(GitHubAppError::InvalidBinding(
                "account_login must not be empty".to_string(),
            ));
        }
        if let RepositoryScope::Explicit(ids) = &repository_scope {
            if ids.iter().any(|id| *id == 0) {
                return Err(GitHubAppError::InvalidBinding(
                    "repository IDs must be non-zero".to_string(),
                ));
            }
        }
        Ok(Self {
            app_id,
            installation_id,
            account_id,
            account_login,
            repository_scope,
            permissions,
            generation,
            enabled: true,
        })
    }

    pub fn authorize(
        &self,
        operation: GitHubOperation,
        repository_id: Option<u64>,
    ) -> Result<(), GitHubAppError> {
        if !self.enabled {
            return Err(GitHubAppError::denied(GitHubDenial::AppDisabled));
        }
        match OperationRegistry::scope(operation) {
            OperationScope::Repository => {
                let repository_id = repository_id
                    .ok_or_else(|| GitHubAppError::denied(GitHubDenial::RepositoryRequired))?;
                if repository_id == 0 || !self.repository_scope.contains(repository_id) {
                    return Err(GitHubAppError::denied(GitHubDenial::RepositoryOutOfScope {
                        repository_id,
                    }));
                }
            }
            OperationScope::Installation | OperationScope::Account => {
                if repository_id.is_some() {
                    return Err(GitHubAppError::denied(GitHubDenial::RepositoryNotAllowed {
                        operation,
                    }));
                }
            }
        }
        let Some((permission, required)) = OperationRegistry::required(operation) else {
            return Ok(());
        };
        let granted = self
            .permissions
            .get(&permission)
            .copied()
            .unwrap_or(AccessLevel::None);
        if !granted.allows(required) {
            return Err(GitHubAppError::denied(GitHubDenial::PermissionNotGranted {
                operation,
            }));
        }
        Ok(())
    }

    pub fn repository_ids_for_token(
        &self,
        operation: GitHubOperation,
        repository_ids: &[u64],
    ) -> Result<Vec<u64>, GitHubAppError> {
        if !OperationRegistry::needs_repository(operation) {
            return Err(GitHubAppError::denied(GitHubDenial::UnsupportedOperation {
                operation,
            }));
        }
        let mut unique = BTreeSet::new();
        for repository_id in repository_ids {
            self.authorize(operation, Some(*repository_id))?;
            unique.insert(*repository_id);
        }
        if unique.is_empty() {
            return Err(GitHubAppError::denied(GitHubDenial::RepositoryRequired));
        }
        Ok(unique.into_iter().collect())
    }

    pub fn permission_for(
        &self,
        operation: GitHubOperation,
    ) -> Result<(GitHubPermission, AccessLevel), GitHubAppError> {
        OperationRegistry::required(operation)
            .ok_or_else(|| GitHubAppError::denied(GitHubDenial::UnsupportedOperation { operation }))
    }

    pub fn token_request(
        &self,
        operation: GitHubOperation,
        repository_ids: &[u64],
    ) -> Result<super::token::TokenRequest, GitHubAppError> {
        let repository_ids = self.repository_ids_for_token(operation, repository_ids)?;
        let (permission, level) = self.permission_for(operation)?;
        let permissions = BTreeMap::from([(permission, level)]);
        super::token::TokenRequest::new(
            self.app_id,
            self.installation_id,
            operation,
            repository_ids,
            permissions,
        )
    }
}
