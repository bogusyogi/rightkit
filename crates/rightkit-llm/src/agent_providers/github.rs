#![allow(clippy::result_large_err)]

//! Thin GitHub REST API client.
//!
//! Opt-in via configuration. Provides methods for common PR/file/branch/comment
//! operations used by the host app's code-review workflows.

use crate::agent_providers::github_guard::{capped_error_text, TargetGuard};
use crate::agent_providers::transport::ProviderError;
use rightkit_http::{AddressPolicy, AsyncClient, ClientConfig, Request};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Forge selector for delivery/PR-CI routing.
///
/// This is deliberately separate from the GitHub App authority model. The
/// delivery lane selects an already-configured local CLI; installation,
/// token, repository-grant, and webhook authority remain GH-001 concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Forge {
    #[serde(rename = "github")]
    GitHub,
    #[serde(rename = "gitlab")]
    GitLab,
    #[serde(rename = "bitbucket")]
    Bitbucket,
    #[serde(rename = "azure_devops")]
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
            "azure_devops" | "azure-devops" | "azure" | "ado" => Ok(Self::AzureDevOps),
            other => Err(format!(
                "unknown forge '{other}'; expected github, gitlab, bitbucket, azure_devops"
            )),
        }
    }

    /// Local CLI adapters currently have complete PR/CI command mappings only
    /// for GitHub and GitLab. Other selectors stay visible but fail closed.
    pub const fn supports_pr_ci(self) -> bool {
        matches!(self, Self::GitHub | Self::GitLab)
    }

    pub const fn cli_binary(self) -> Option<&'static str> {
        match self {
            Self::GitHub => Some("gh"),
            Self::GitLab => Some("glab"),
            Self::Bitbucket | Self::AzureDevOps => None,
        }
    }

    pub const fn api_base(self) -> &'static str {
        match self {
            Self::GitHub => "https://api.github.com",
            Self::GitLab => "https://gitlab.com/api/v4",
            Self::Bitbucket => "https://api.bitbucket.org/2.0",
            Self::AzureDevOps => "https://dev.azure.com",
        }
    }
}

impl std::fmt::Display for Forge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A GitHub pull request summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub html_url: String,
    pub head_ref: String,
    pub base_ref: String,
}

/// A file changed in a pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrFile {
    pub filename: String,
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    pub changes: u64,
    pub patch: Option<String>,
}

/// A thin GitHub REST API client.
pub struct GitHubClient {
    token: String,
    base_url: String,
    client: AsyncClient,
    target_guard: TargetGuard,
}

impl GitHubClient {
    /// Create a new client using the GitHub API.
    ///
    /// `base_url` defaults to `"https://api.github.com"` when `None`. HTTPS is
    /// required for any non-localhost `base_url`, and the resolved host must
    /// not be a private/reserved network address — plaintext HTTP or a
    /// caller-supplied internal target would send the GitHub token somewhere
    /// unintended. Use [`GitHubClient::new_with_private_approval`] for an
    /// internal Enterprise instance on a private network.
    pub fn new(token: impl Into<String>, base_url: Option<String>) -> Self {
        Self::build(token, base_url, false)
    }

    /// Same as [`GitHubClient::new`] but allows the resolved target to be a
    /// private/reserved network address (LAN GitHub Enterprise, etc.). Callers
    /// must only set this from an explicit, user-approved server config entry —
    /// never as a default (mirrors A-3's `approved_private` pattern).
    pub fn new_with_private_approval(token: impl Into<String>, base_url: Option<String>) -> Self {
        Self::build(token, base_url, true)
    }

    fn build(token: impl Into<String>, base_url: Option<String>, approved_private: bool) -> Self {
        let base_url = base_url.unwrap_or_else(|| "https://api.github.com".to_string());
        // Public addresses only (the egress guard); an operator-approved private
        // Enterprise host is the one exception.
        let mut policy = AddressPolicy::public_web();
        if approved_private {
            if let Some(host) = crate::agent_providers::http::base_url_host(&base_url) {
                policy = policy.allow_if(vec![host], |_| true);
            }
        }
        let config = ClientConfig {
            connect_timeout: Some(CONNECT_TIMEOUT),
            request_timeout: REQUEST_TIMEOUT,
            response_timeout: Some(REQUEST_TIMEOUT),
            address_policy: policy,
            ..ClientConfig::default()
        };
        let http = AsyncClient::new(config.clone()).unwrap_or_else(|_| {
            AsyncClient::new(config)
                .expect("rightkit-http client builds with the platform verifier")
        });
        Self {
            token: token.into(),
            target_guard: TargetGuard::new(base_url.clone(), approved_private),
            base_url,
            client: http,
        }
    }

    /// Build the authorization header value.
    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// One authenticated GitHub API call. 401 is `Auth`, any other non-2xx keeps
    /// the (capped) response body, transport failures are `Network`.
    async fn call(&self, request: Request) -> Result<rightkit_http::Response, ProviderError> {
        let request = request
            .secret_header("Authorization", self.auth_header())
            .header("Accept", "application/vnd.github.v3+json")
            .header("User-Agent", "rightkit-agent-providers/0.1");
        let resp = self.client.send_any(&request).await?;
        if resp.status == 401 {
            return Err(ProviderError::Auth);
        }
        if !(200..300).contains(&resp.status) {
            return Err(ProviderError::Http {
                status: resp.status,
                body: capped_error_text(&resp.body),
            });
        }
        Ok(resp)
    }

    /// Get the decoded contents of a file at a given ref.
    pub async fn get_file(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        ref_: &str,
    ) -> Result<String, ProviderError> {
        self.target_guard.ensure_allowed().await?;
        let url = format!(
            "{}/repos/{}/{}/contents/{}?ref={}",
            self.base_url, owner, repo, path, ref_
        );

        let resp = self.call(Request::get(&url)).await?;

        let json: serde_json::Value = resp.json()?;
        let encoded = json["content"]
            .as_str()
            .ok_or_else(|| ProviderError::Provider("missing content field".to_string()))?;

        // GitHub returns base64-encoded content with newlines
        let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        let decoded = base64_decode(&cleaned)?;
        Ok(decoded)
    }

    /// List open pull requests for a repository.
    pub async fn list_prs(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<PullRequest>, ProviderError> {
        self.target_guard.ensure_allowed().await?;
        let url = format!(
            "{}/repos/{}/{}/pulls?state=open&per_page=100",
            self.base_url, owner, repo
        );

        let resp = self.call(Request::get(&url)).await?;

        let raw: Vec<serde_json::Value> = resp.json()?;
        let prs = raw
            .into_iter()
            .map(|v| PullRequest {
                number: v["number"].as_u64().unwrap_or(0),
                title: v["title"].as_str().unwrap_or("").to_string(),
                body: v["body"].as_str().map(str::to_string),
                state: v["state"].as_str().unwrap_or("").to_string(),
                html_url: v["html_url"].as_str().unwrap_or("").to_string(),
                head_ref: v["head"]["ref"].as_str().unwrap_or("").to_string(),
                base_ref: v["base"]["ref"].as_str().unwrap_or("").to_string(),
            })
            .collect();

        Ok(prs)
    }

    /// Add a comment to a pull request.
    pub async fn add_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<(), ProviderError> {
        self.target_guard.ensure_allowed().await?;
        let url = format!(
            "{}/repos/{}/{}/issues/{}/comments",
            self.base_url, owner, repo, pr_number
        );

        let payload = serde_json::json!({ "body": body });

        self.call(Request::post(&url).json(&payload)?).await?;

        Ok(())
    }

    /// List the files changed in a pull request.
    pub async fn get_pr_files(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<PrFile>, ProviderError> {
        self.target_guard.ensure_allowed().await?;
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/files?per_page=100",
            self.base_url, owner, repo, pr_number
        );

        let resp = self.call(Request::get(&url)).await?;

        let raw: Vec<serde_json::Value> = resp.json()?;
        let files = raw
            .into_iter()
            .map(|v| PrFile {
                filename: v["filename"].as_str().unwrap_or("").to_string(),
                status: v["status"].as_str().unwrap_or("").to_string(),
                additions: v["additions"].as_u64().unwrap_or(0),
                deletions: v["deletions"].as_u64().unwrap_or(0),
                changes: v["changes"].as_u64().unwrap_or(0),
                patch: v["patch"].as_str().map(str::to_string),
            })
            .collect();

        Ok(files)
    }
}

/// Simple base64 decoder (no external dep needed — uses basic alphabet).
fn base64_decode(input: &str) -> Result<String, ProviderError> {
    // Use the base64 alphabet manually
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [0u8; 256];
    for (i, &b) in alphabet.iter().enumerate() {
        lookup[b as usize] = i as u8;
    }

    let bytes: Vec<u8> = input.bytes().filter(|&b| b != b'=').collect();
    let mut out = Vec::new();

    let mut i = 0;
    while i + 3 < bytes.len() {
        let b0 = lookup[bytes[i] as usize];
        let b1 = lookup[bytes[i + 1] as usize];
        let b2 = lookup[bytes[i + 2] as usize];
        let b3 = lookup[bytes[i + 3] as usize];

        out.push((b0 << 2) | (b1 >> 4));
        out.push((b1 << 4) | (b2 >> 2));
        out.push((b2 << 6) | b3);
        i += 4;
    }

    // Handle remaining bytes
    let rem = bytes.len() - i;
    if rem >= 2 {
        let b0 = lookup[bytes[i] as usize];
        let b1 = lookup[bytes[i + 1] as usize];
        out.push((b0 << 2) | (b1 >> 4));
        if rem >= 3 {
            let b2 = lookup[bytes[i + 2] as usize];
            out.push((b1 << 4) | (b2 >> 2));
        }
    }

    String::from_utf8(out).map_err(|e| ProviderError::Provider(format!("UTF-8 decode error: {e}")))
}
