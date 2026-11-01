use super::installation::{AccessLevel, GitHubOperation, GitHubPermission, OperationRegistry};
use super::{GitHubAppError, GitHubDenial};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Request passed to an adapter that mints one installation access token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRequest {
    pub app_id: u64,
    pub installation_id: u64,
    pub operation: GitHubOperation,
    pub repository_ids: Vec<u64>,
    pub permissions: BTreeMap<GitHubPermission, AccessLevel>,
}

impl TokenRequest {
    pub fn new(
        app_id: u64,
        installation_id: u64,
        operation: GitHubOperation,
        repository_ids: Vec<u64>,
        permissions: BTreeMap<GitHubPermission, AccessLevel>,
    ) -> Result<Self, GitHubAppError> {
        if app_id == 0 || installation_id == 0 {
            return Err(GitHubAppError::InvalidToken(
                "token request identity must be non-zero".to_string(),
            ));
        }
        if !OperationRegistry::needs_repository(operation) {
            return Err(GitHubAppError::denied(GitHubDenial::UnsupportedOperation {
                operation,
            }));
        }
        if repository_ids.is_empty() {
            return Err(GitHubAppError::denied(GitHubDenial::RepositoryRequired));
        }
        if repository_ids.contains(&0) {
            return Err(GitHubAppError::InvalidToken(
                "token repository IDs must be non-zero".to_string(),
            ));
        }
        if permissions.is_empty() {
            return Err(GitHubAppError::InvalidToken(
                "token request must carry explicit permissions".to_string(),
            ));
        }
        validate_permission_shape(operation, &permissions)?;
        let mut repository_ids = repository_ids;
        repository_ids.sort_unstable();
        repository_ids.dedup();
        Ok(Self {
            app_id,
            installation_id,
            operation,
            repository_ids,
            permissions,
        })
    }
}

/// Short-lived GitHub installation access token. Token bytes never serialize
/// and its Debug output is deliberately redacted.
#[derive(Clone)]
pub struct InstallationAccessToken {
    token: String,
    pub app_id: u64,
    pub installation_id: u64,
    pub operation: GitHubOperation,
    pub issued_at: u64,
    pub expires_at: u64,
    pub repository_ids: Vec<u64>,
    pub permissions: BTreeMap<GitHubPermission, AccessLevel>,
}

impl std::fmt::Debug for InstallationAccessToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstallationAccessToken")
            .field("token", &"[REDACTED]")
            .field("app_id", &self.app_id)
            .field("installation_id", &self.installation_id)
            .field("operation", &self.operation)
            .field("issued_at", &self.issued_at)
            .field("expires_at", &self.expires_at)
            .field("repository_ids", &self.repository_ids)
            .field("permissions", &self.permissions)
            .finish()
    }
}

impl InstallationAccessToken {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        token: impl Into<String>,
        app_id: u64,
        installation_id: u64,
        operation: GitHubOperation,
        issued_at: u64,
        expires_at: u64,
        repository_ids: Vec<u64>,
        permissions: BTreeMap<GitHubPermission, AccessLevel>,
    ) -> Result<Self, GitHubAppError> {
        let token = token.into();
        if token.trim().is_empty() {
            return Err(GitHubAppError::denied(GitHubDenial::TokenMissing));
        }
        if app_id == 0 || installation_id == 0 || expires_at <= issued_at {
            return Err(GitHubAppError::InvalidToken(
                "token identity or expiry is invalid".to_string(),
            ));
        }
        if repository_ids.is_empty() || repository_ids.contains(&0) {
            return Err(GitHubAppError::InvalidToken(
                "token repository scope is invalid".to_string(),
            ));
        }
        if permissions.is_empty() {
            return Err(GitHubAppError::InvalidToken(
                "token permissions must be explicit".to_string(),
            ));
        }
        validate_permission_shape(operation, &permissions)?;
        let mut repository_ids = repository_ids;
        repository_ids.sort_unstable();
        repository_ids.dedup();
        Ok(Self {
            token,
            app_id,
            installation_id,
            operation,
            issued_at,
            expires_at,
            repository_ids,
            permissions,
        })
    }

    pub fn is_expired(&self, now_unix: u64) -> bool {
        now_unix >= self.expires_at
    }

    pub fn needs_refresh(&self, now_unix: u64, refresh_skew_seconds: u64) -> bool {
        self.is_expired(now_unix)
            || self.expires_at.saturating_sub(now_unix) <= refresh_skew_seconds
    }

    pub fn matches_request(&self, request: &TokenRequest) -> bool {
        self.app_id == request.app_id
            && self.installation_id == request.installation_id
            && self.operation == request.operation
            && self.repository_ids == request.repository_ids
            && self.permissions == request.permissions
    }

    pub fn authorization_header_for(
        &self,
        request: &TokenRequest,
        now_unix: u64,
    ) -> Result<String, GitHubAppError> {
        if !self.matches_request(request) {
            return Err(GitHubAppError::denied(GitHubDenial::TokenBindingMismatch));
        }
        if self.is_expired(now_unix) {
            return Err(GitHubAppError::denied(GitHubDenial::TokenExpired));
        }
        Ok(format!("Bearer {}", self.token))
    }
}

fn validate_permission_shape(
    operation: GitHubOperation,
    permissions: &BTreeMap<GitHubPermission, AccessLevel>,
) -> Result<(), GitHubAppError> {
    let Some((required_permission, required_level)) = OperationRegistry::required(operation) else {
        return Err(GitHubAppError::denied(GitHubDenial::UnsupportedOperation {
            operation,
        }));
    };
    if permissions.len() != 1
        || permissions.get(&required_permission).copied() != Some(required_level)
    {
        return Err(GitHubAppError::denied(GitHubDenial::PermissionNotGranted {
            operation,
        }));
    }
    Ok(())
}

/// Adapter seam for GitHub's JWT → installation-token → revoke lifecycle.
/// Every implementation must preserve request scope and return a fresh token
/// on refresh; no unsafe default mutates or reuses an old token.
pub trait InstallationTokenLifecycle {
    fn issue(&self, request: &TokenRequest) -> Result<InstallationAccessToken, GitHubAppError>;

    fn refresh(
        &self,
        current: &InstallationAccessToken,
        request: &TokenRequest,
        now_unix: u64,
    ) -> Result<InstallationAccessToken, GitHubAppError>;

    fn revoke(&self, token: &InstallationAccessToken) -> Result<(), GitHubAppError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permissions() -> BTreeMap<GitHubPermission, AccessLevel> {
        BTreeMap::from([(GitHubPermission::Contents, AccessLevel::Read)])
    }

    #[test]
    fn token_expiry_and_refresh_skew_are_fail_closed() {
        let token = InstallationAccessToken::new(
            "ghs_secret",
            1,
            2,
            GitHubOperation::ContentsRead,
            100,
            200,
            vec![3],
            permissions(),
        )
        .unwrap();
        assert!(!token.is_expired(199));
        assert!(token.needs_refresh(199, 1));
        assert!(token.is_expired(200));
        assert!(format!("{token:?}").contains("REDACTED"));
        assert!(!format!("{token:?}").contains("ghs_secret"));
        let request =
            TokenRequest::new(1, 2, GitHubOperation::ContentsRead, vec![3], permissions()).unwrap();
        assert_eq!(
            token.authorization_header_for(&request, 199).unwrap(),
            "Bearer ghs_secret"
        );
        assert!(matches!(
            token.authorization_header_for(&request, 200),
            Err(GitHubAppError::Denied(GitHubDenial::TokenExpired))
        ));
    }

    #[test]
    fn invalid_token_requests_do_not_admit_empty_scope() {
        assert!(matches!(
            TokenRequest::new(1, 2, GitHubOperation::ContentsRead, vec![], permissions()),
            Err(GitHubAppError::Denied(GitHubDenial::RepositoryRequired))
        ));
    }
}
