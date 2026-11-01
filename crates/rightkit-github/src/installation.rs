use super::{GitHubAppError, GitHubDenial};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// GitHub App permission families used by the host app operations.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn binding_with_permissions(
        permissions: BTreeMap<GitHubPermission, AccessLevel>,
    ) -> InstallationBinding {
        InstallationBinding::new(
            7,
            9,
            Some(11),
            "acme",
            RepositoryScope::Explicit(BTreeSet::from([42])),
            permissions,
            3,
        )
        .unwrap()
    }

    fn binding() -> InstallationBinding {
        binding_with_permissions(BTreeMap::from([
            (GitHubPermission::Contents, AccessLevel::Read),
            (GitHubPermission::Issues, AccessLevel::Write),
        ]))
    }

    #[test]
    fn registry_enforces_scope_and_permission() {
        let binding = binding();
        assert!(binding
            .authorize(GitHubOperation::IssuesWrite, Some(42))
            .is_ok());
        assert!(matches!(
            binding.authorize(GitHubOperation::IssuesWrite, Some(43)),
            Err(GitHubAppError::Denied(GitHubDenial::RepositoryOutOfScope {
                repository_id: 43
            }))
        ));
        assert!(matches!(
            binding.authorize(GitHubOperation::PullRequestsWrite, Some(42)),
            Err(GitHubAppError::Denied(GitHubDenial::PermissionNotGranted {
                operation: GitHubOperation::PullRequestsWrite
            }))
        ));
    }

    #[test]
    fn token_repository_selection_is_unique_and_sorted() {
        let binding = binding();
        assert_eq!(
            binding
                .repository_ids_for_token(GitHubOperation::ContentsRead, &[42, 42])
                .unwrap(),
            vec![42]
        );
    }

    #[test]
    fn every_operation_has_one_explicit_mapping_or_intentional_delivery_scope() {
        let operations = OperationRegistry::operations();
        assert_eq!(operations.len(), 44);
        for (index, operation) in operations.iter().enumerate() {
            assert!(
                !operations[..index].contains(operation),
                "operation appears more than once: {operation:?}"
            );
            if *operation == GitHubOperation::WebhookDelivery {
                assert_eq!(OperationRegistry::required(*operation), None);
                assert_eq!(
                    OperationRegistry::scope(*operation),
                    OperationScope::Installation
                );
            } else {
                assert!(
                    OperationRegistry::required(*operation).is_some(),
                    "operation has no permission mapping: {operation:?}"
                );
            }
        }
        assert_eq!(
            OperationRegistry::scope(GitHubOperation::ProjectsRead),
            OperationScope::Installation
        );
        assert_eq!(
            OperationRegistry::scope(GitHubOperation::NotificationsRead),
            OperationScope::Account
        );
        assert_eq!(
            OperationRegistry::scope(GitHubOperation::HooksRead),
            OperationScope::Repository
        );
    }

    #[test]
    fn every_operation_authorizes_only_at_declared_scope() {
        for operation in OperationRegistry::operations() {
            let mut permissions = BTreeMap::new();
            if let Some((permission, level)) = OperationRegistry::required(*operation) {
                permissions.insert(permission, level);
            }
            let binding = binding_with_permissions(permissions);
            match OperationRegistry::scope(*operation) {
                OperationScope::Repository => {
                    assert!(
                        binding.authorize(*operation, Some(42)).is_ok(),
                        "repository operation did not authorize: {operation:?}"
                    );
                    assert!(matches!(
                        binding.authorize(*operation, None),
                        Err(GitHubAppError::Denied(GitHubDenial::RepositoryRequired))
                    ));
                }
                OperationScope::Installation | OperationScope::Account => {
                    assert!(
                        binding.authorize(*operation, None).is_ok(),
                        "installation operation did not authorize: {operation:?}"
                    );
                    assert!(matches!(
                        binding.authorize(*operation, Some(42)),
                        Err(GitHubAppError::Denied(GitHubDenial::RepositoryNotAllowed {
                            operation: denied_operation
                        })) if denied_operation == *operation
                    ));
                }
            }
        }
    }

    #[test]
    fn forge_families_use_expected_permission_authority() {
        use AccessLevel::{Read, Write};
        use GitHubOperation::*;
        use GitHubPermission::*;

        for operation in [BranchesRead, CommitsRead, ReleasesRead] {
            assert_eq!(
                OperationRegistry::required(operation),
                Some((Contents, Read))
            );
        }
        for operation in [BranchesWrite, CommitsWrite, ReleasesWrite] {
            assert_eq!(
                OperationRegistry::required(operation),
                Some((Contents, Write))
            );
        }
        for operation in [ActionsRunsRead, ActionsArtifactsRead] {
            assert_eq!(
                OperationRegistry::required(operation),
                Some((Actions, Read))
            );
        }
        for operation in [ActionsRunsWrite, ActionsArtifactsWrite] {
            assert_eq!(
                OperationRegistry::required(operation),
                Some((Actions, Write))
            );
        }
        assert_eq!(
            OperationRegistry::required(PullRequestCommentsWrite),
            Some((PullRequests, Write))
        );
        assert_eq!(
            OperationRegistry::required(CommitStatusesWrite),
            Some((CommitStatuses, Write))
        );
        assert_eq!(
            OperationRegistry::required(OrganizationHooksRead),
            Some((OrganizationHooks, Read))
        );
    }
}
