//! FM-165 — forge-routed CI status + review/comment posting.
//!
//! `refactor0-closure-ledger.md` FM-165 ("Desktop PR/CI lifecycle"): §14
//! outcome D — retain desktop delivery authority; port Gito forge-routing and
//! report children (donor: Gito — multi-forge CI/review routing). Scope is
//! exactly the two report-child operations Gito routes across forges: actual
//! CI/check status and review/comment posting, beyond the `Forge` enum that
//! already exists in `forge.rs`. GitHub App installation, tokens, and
//! permission-gated forge operations are `GH-001`, not this atom.
//!
//! This module stays adapter-free like the rest of `github_app`: it builds a
//! typed, forge-specific `HttpRequestSpec` (method, URL, headers, body) per
//! operation and lets a caller execute it with whatever HTTP client is wired
//! at the daemon boundary. That keeps routing, URL/method/auth-header shape,
//! and unsupported-operation handling unit-testable without live network.

use super::Forge;
use serde::{Deserialize, Serialize};

/// One CI/check status report as returned by a forge's status API, normalized
/// to the shape callers need regardless of forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiStatusRequest {
    pub owner: String,
    pub repo: String,
    /// Commit SHA (GitHub/Bitbucket) or ref usable as a commit SHA (GitLab).
    pub commit_sha: String,
}

/// A review or PR/MR comment to post back to the forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewPostRequest {
    pub owner: String,
    pub repo: String,
    /// PR number (GitHub/Bitbucket) or merge request IID (GitLab).
    pub pull_request_id: u64,
    pub body: String,
}

/// Forge-routable report-child operations. Named exactly to the FM-165 scope:
/// actual CI status and review/comment posting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgeReportOperation {
    CiStatus,
    PostReview,
}

/// A fully-constructed, not-yet-executed HTTP request. Deliberately opaque to
/// any client type so routing/shape can be asserted in tests without a live
/// network call; the daemon boundary executes it with `rightkit-http`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequestSpec {
    pub method: HttpMethod,
    pub url: String,
    /// `(header name, header value)` pairs, in construction order.
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

impl HttpMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// Typed failure for an operation a forge client cannot (yet) perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeClientError {
    UnsupportedOperation {
        forge: Forge,
        operation: ForgeReportOperation,
    },
    MissingCredential {
        forge: Forge,
    },
    InvalidRequest {
        forge: Forge,
        reason: String,
    },
}

impl std::fmt::Display for ForgeClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedOperation { forge, operation } => write!(
                formatter,
                "forge {forge} does not support report operation {operation:?}"
            ),
            Self::MissingCredential { forge } => {
                write!(formatter, "forge {forge} client is missing a credential")
            }
            Self::InvalidRequest { forge, reason } => {
                write!(formatter, "forge {forge} request is invalid: {reason}")
            }
        }
    }
}

impl std::error::Error for ForgeClientError {}

/// Per-forge credential. Callers hold the concrete secret; this module never
/// stores or logs it (`HttpRequestSpec::headers` are constructed once and
/// handed straight to the caller's HTTP layer).
#[derive(Clone)]
pub struct ForgeCredential(pub String);

impl std::fmt::Debug for ForgeCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ForgeCredential")
            .field(&"[REDACTED]")
            .finish()
    }
}

/// Routing seam: builds the request for a forge + report operation. The
/// GitHub arm delegates to the same REST shape the existing
/// `providers::github::GitHubClient` uses (`Authorization: Bearer`,
/// `Accept: application/vnd.github.v3+json`); GitLab and Bitbucket arms are
/// new, built against their public REST APIs.
pub trait ForgeClient {
    fn forge(&self) -> Forge;

    fn ci_status_request(
        &self,
        request: &CiStatusRequest,
    ) -> Result<HttpRequestSpec, ForgeClientError>;

    fn post_review_request(
        &self,
        request: &ReviewPostRequest,
    ) -> Result<HttpRequestSpec, ForgeClientError>;
}

/// GitHub arm. Delegates URL/header shape to the same conventions as
/// `engine/crates/providers/src/github.rs::GitHubClient` (Bearer token,
/// `application/vnd.github.v3+json`, explicit `User-Agent`), so the daemon
/// can route through this seam without diverging from the existing
/// GitHub-only path.
pub struct GitHubForgeClient {
    pub credential: ForgeCredential,
    pub base_url: String,
}

impl GitHubForgeClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            credential: ForgeCredential(token.into()),
            base_url: "https://api.github.com".to_string(),
        }
    }

    fn auth_header(&self) -> (String, String) {
        (
            "Authorization".to_string(),
            format!("Bearer {}", self.credential.0),
        )
    }
}

impl ForgeClient for GitHubForgeClient {
    fn forge(&self) -> Forge {
        Forge::GitHub
    }

    fn ci_status_request(
        &self,
        request: &CiStatusRequest,
    ) -> Result<HttpRequestSpec, ForgeClientError> {
        if self.credential.0.trim().is_empty() {
            return Err(ForgeClientError::MissingCredential {
                forge: Forge::GitHub,
            });
        }
        Ok(HttpRequestSpec {
            method: HttpMethod::Get,
            url: format!(
                "{}/repos/{}/{}/commits/{}/status",
                self.base_url, request.owner, request.repo, request.commit_sha
            ),
            headers: vec![
                self.auth_header(),
                (
                    "Accept".to_string(),
                    "application/vnd.github.v3+json".to_string(),
                ),
                (
                    "User-Agent".to_string(),
                    "rightkit-github-forge-client/0.1".to_string(),
                ),
            ],
            body: None,
        })
    }

    fn post_review_request(
        &self,
        request: &ReviewPostRequest,
    ) -> Result<HttpRequestSpec, ForgeClientError> {
        if self.credential.0.trim().is_empty() {
            return Err(ForgeClientError::MissingCredential {
                forge: Forge::GitHub,
            });
        }
        Ok(HttpRequestSpec {
            method: HttpMethod::Post,
            url: format!(
                "{}/repos/{}/{}/issues/{}/comments",
                self.base_url, request.owner, request.repo, request.pull_request_id
            ),
            headers: vec![
                self.auth_header(),
                (
                    "Accept".to_string(),
                    "application/vnd.github.v3+json".to_string(),
                ),
                (
                    "User-Agent".to_string(),
                    "rightkit-github-forge-client/0.1".to_string(),
                ),
            ],
            body: Some(serde_json::json!({ "body": request.body }).to_string()),
        })
    }
}

/// Branch-level PR/CI lookups for the TUI status band (#136 A197). Inherent
/// to the GitHub arm: GitLab/Bitbucket keep the trait surface only.
impl GitHubForgeClient {
    fn read_headers(&self) -> Result<Vec<(String, String)>, ForgeClientError> {
        if self.credential.0.trim().is_empty() {
            return Err(ForgeClientError::MissingCredential {
                forge: Forge::GitHub,
            });
        }
        Ok(vec![
            self.auth_header(),
            (
                "Accept".to_string(),
                "application/vnd.github.v3+json".to_string(),
            ),
            (
                "User-Agent".to_string(),
                "rightkit-github-forge-client/0.1".to_string(),
            ),
        ])
    }

    /// Open PRs whose head is `owner:branch`.
    pub fn open_pr_for_branch_request(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<HttpRequestSpec, ForgeClientError> {
        Ok(HttpRequestSpec {
            method: HttpMethod::Get,
            url: format!(
                "{}/repos/{owner}/{repo}/pulls?state=open&head={owner}:{}",
                self.base_url,
                branch.replace('/', "%2F")
            ),
            headers: self.read_headers()?,
            body: None,
        })
    }

    /// Check runs for a commit.
    pub fn check_runs_request(
        &self,
        owner: &str,
        repo: &str,
        commit_sha: &str,
    ) -> Result<HttpRequestSpec, ForgeClientError> {
        Ok(HttpRequestSpec {
            method: HttpMethod::Get,
            url: format!(
                "{}/repos/{owner}/{repo}/commits/{commit_sha}/check-runs",
                self.base_url
            ),
            headers: self.read_headers()?,
            body: None,
        })
    }
}

/// Execute a GET spec with `rightkit-http`'s blocking client and return the body.
/// The spec's host is the one host allowed to resolve to a non-public address (an
/// operator-configured Enterprise server); redirects are not followed.
pub fn execute_get_blocking(spec: &HttpRequestSpec) -> Result<String, String> {
    if spec.method != HttpMethod::Get {
        return Err("only GET specs are supported".to_string());
    }
    let mut policy = rightkit_http::AddressPolicy::public_web();
    if let Some(host) = crate::forge_client::host_of(&spec.url) {
        policy = policy.allow_if(vec![host], |_| true);
    }
    let client = rightkit_http::Client::new(rightkit_http::ClientConfig {
        request_timeout: std::time::Duration::from_secs(10),
        response_timeout: Some(std::time::Duration::from_secs(10)),
        retry: rightkit_http::RetryPolicy::none(),
        address_policy: policy,
        proxy: rightkit_http::ProxyMode::Environment,
        ..rightkit_http::ClientConfig::default()
    });
    let mut request = rightkit_http::Request::get(spec.url.clone());
    for (name, value) in &spec.headers {
        // Credentials stay out of error text.
        request = if name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("private-token")
        {
            request.secret_header(name.as_str(), value.as_str())
        } else {
            request.header(name.as_str(), value.as_str())
        };
    }
    let response = client
        .send_any(&request)
        .map_err(|error| error.to_string())?;
    if !(200..300).contains(&response.status) {
        return Err(format!("HTTP {}", response.status));
    }
    Ok(response.text())
}

/// GitLab arm. Builds requests against the GitLab REST API v4: commit
/// statuses (`GET /projects/:id/repository/commits/:sha/statuses`) and merge
/// request notes (`POST /projects/:id/merge_requests/:iid/notes`), using
/// `PRIVATE-TOKEN` auth per GitLab's documented convention.
pub struct GitLabForgeClient {
    pub credential: ForgeCredential,
    pub base_url: String,
}

impl GitLabForgeClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            credential: ForgeCredential(token.into()),
            base_url: "https://gitlab.com/api/v4".to_string(),
        }
    }

    fn auth_header(&self) -> (String, String) {
        ("PRIVATE-TOKEN".to_string(), self.credential.0.clone())
    }

    fn project_path(owner: &str, repo: &str) -> String {
        urlencoding_project(owner, repo)
    }
}

impl ForgeClient for GitLabForgeClient {
    fn forge(&self) -> Forge {
        Forge::GitLab
    }

    fn ci_status_request(
        &self,
        request: &CiStatusRequest,
    ) -> Result<HttpRequestSpec, ForgeClientError> {
        if self.credential.0.trim().is_empty() {
            return Err(ForgeClientError::MissingCredential {
                forge: Forge::GitLab,
            });
        }
        Ok(HttpRequestSpec {
            method: HttpMethod::Get,
            url: format!(
                "{}/projects/{}/repository/commits/{}/statuses",
                self.base_url,
                Self::project_path(&request.owner, &request.repo),
                request.commit_sha
            ),
            headers: vec![self.auth_header()],
            body: None,
        })
    }

    fn post_review_request(
        &self,
        request: &ReviewPostRequest,
    ) -> Result<HttpRequestSpec, ForgeClientError> {
        if self.credential.0.trim().is_empty() {
            return Err(ForgeClientError::MissingCredential {
                forge: Forge::GitLab,
            });
        }
        Ok(HttpRequestSpec {
            method: HttpMethod::Post,
            url: format!(
                "{}/projects/{}/merge_requests/{}/notes",
                self.base_url,
                Self::project_path(&request.owner, &request.repo),
                request.pull_request_id
            ),
            headers: vec![
                self.auth_header(),
                ("Content-Type".to_string(), "application/json".to_string()),
            ],
            body: Some(serde_json::json!({ "body": request.body }).to_string()),
        })
    }
}

/// Bitbucket arm. Builds requests against the Bitbucket Cloud REST API 2.0:
/// commit build statuses
/// (`GET /repositories/{workspace}/{repo}/commit/{sha}/statuses`) and PR
/// comments (`POST /repositories/{workspace}/{repo}/pullrequests/{id}/comments`),
/// using `Authorization: Bearer` (app password / access token) auth.
pub struct BitbucketForgeClient {
    pub credential: ForgeCredential,
    pub base_url: String,
}

impl BitbucketForgeClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            credential: ForgeCredential(token.into()),
            base_url: "https://api.bitbucket.org/2.0".to_string(),
        }
    }

    fn auth_header(&self) -> (String, String) {
        (
            "Authorization".to_string(),
            format!("Bearer {}", self.credential.0),
        )
    }
}

impl ForgeClient for BitbucketForgeClient {
    fn forge(&self) -> Forge {
        Forge::Bitbucket
    }

    fn ci_status_request(
        &self,
        request: &CiStatusRequest,
    ) -> Result<HttpRequestSpec, ForgeClientError> {
        if self.credential.0.trim().is_empty() {
            return Err(ForgeClientError::MissingCredential {
                forge: Forge::Bitbucket,
            });
        }
        Ok(HttpRequestSpec {
            method: HttpMethod::Get,
            url: format!(
                "{}/repositories/{}/{}/commit/{}/statuses",
                self.base_url, request.owner, request.repo, request.commit_sha
            ),
            headers: vec![self.auth_header()],
            body: None,
        })
    }

    fn post_review_request(
        &self,
        request: &ReviewPostRequest,
    ) -> Result<HttpRequestSpec, ForgeClientError> {
        if self.credential.0.trim().is_empty() {
            return Err(ForgeClientError::MissingCredential {
                forge: Forge::Bitbucket,
            });
        }
        Ok(HttpRequestSpec {
            method: HttpMethod::Post,
            url: format!(
                "{}/repositories/{}/{}/pullrequests/{}/comments",
                self.base_url, request.owner, request.repo, request.pull_request_id
            ),
            headers: vec![
                self.auth_header(),
                ("Content-Type".to_string(), "application/json".to_string()),
            ],
            body: Some(serde_json::json!({ "content": { "raw": request.body } }).to_string()),
        })
    }
}

/// Azure DevOps has no report-child client yet; every operation is a typed
/// `UnsupportedOperation` rather than a silent GitHub fallback.
pub struct AzureDevOpsForgeClient;

impl ForgeClient for AzureDevOpsForgeClient {
    fn forge(&self) -> Forge {
        Forge::AzureDevOps
    }

    fn ci_status_request(
        &self,
        _request: &CiStatusRequest,
    ) -> Result<HttpRequestSpec, ForgeClientError> {
        Err(ForgeClientError::UnsupportedOperation {
            forge: Forge::AzureDevOps,
            operation: ForgeReportOperation::CiStatus,
        })
    }

    fn post_review_request(
        &self,
        _request: &ReviewPostRequest,
    ) -> Result<HttpRequestSpec, ForgeClientError> {
        Err(ForgeClientError::UnsupportedOperation {
            forge: Forge::AzureDevOps,
            operation: ForgeReportOperation::PostReview,
        })
    }
}

/// Routes a forge selector to its concrete `ForgeClient`. This is the seam
/// callers (daemon routes, CLI) use instead of matching on `Forge` directly.
pub fn client_for(forge: Forge, credential: ForgeCredential) -> Box<dyn ForgeClient> {
    match forge {
        Forge::GitHub => Box::new(GitHubForgeClient {
            credential,
            base_url: "https://api.github.com".to_string(),
        }),
        Forge::GitLab => Box::new(GitLabForgeClient {
            credential,
            base_url: "https://gitlab.com/api/v4".to_string(),
        }),
        Forge::Bitbucket => Box::new(BitbucketForgeClient {
            credential,
            base_url: "https://api.bitbucket.org/2.0".to_string(),
        }),
        Forge::AzureDevOps => Box::new(AzureDevOpsForgeClient),
    }
}

/// Builds a request for a named report operation, routed by forge. This is
/// the single dispatch point exercised by tests and by the daemon boundary.
pub fn build_report_request(
    forge: Forge,
    credential: ForgeCredential,
    operation: ForgeReportOperation,
    ci_status: Option<&CiStatusRequest>,
    review: Option<&ReviewPostRequest>,
) -> Result<HttpRequestSpec, ForgeClientError> {
    let client = client_for(forge, credential);
    match operation {
        ForgeReportOperation::CiStatus => {
            let request = ci_status.ok_or_else(|| ForgeClientError::InvalidRequest {
                forge,
                reason: "ci_status operation requires a CiStatusRequest".to_string(),
            })?;
            client.ci_status_request(request)
        }
        ForgeReportOperation::PostReview => {
            let request = review.ok_or_else(|| ForgeClientError::InvalidRequest {
                forge,
                reason: "post_review operation requires a ReviewPostRequest".to_string(),
            })?;
            client.post_review_request(request)
        }
    }
}

/// Minimal path-segment percent-encoding sufficient for GitLab's
/// `namespace%2Fproject` project-path convention. Avoids adding a
/// `urlencoding`/`percent-encoding` dependency for the handful of reserved
/// characters GitLab project paths can realistically contain.
fn urlencoding_project(owner: &str, repo: &str) -> String {
    fn encode(segment: &str) -> String {
        let mut out = String::with_capacity(segment.len());
        for byte in segment.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(byte as char);
                }
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        out
    }
    format!("{}%2F{}", encode(owner), encode(repo))
}

/// Host name of `url`, if it parses.
pub fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
}
