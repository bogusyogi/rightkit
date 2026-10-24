use super::{ClientAuth, ServerAuthError};
use crate::OAuthConfig;
use rightkit_http::Secret;

/// Server-only policy wrapped around the existing desktop provider descriptor.
#[derive(Clone)]
pub struct ServerProvider {
    pub id: String,
    pub config: OAuthConfig,
    pub redirect_uri: String,
    pub client_auth: ClientAuth,
    pub pkce: bool,
    /// Allows `openid` scopes. A validator must be installed on the flow.
    pub oidc: bool,
    pub client_id_parameter: ClientIdParameter,
    pub scope_separator: char,
}

impl std::fmt::Debug for ServerProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerProvider { [redacted] }")
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ClientIdParameter {
    ClientId,
    ClientKey,
}

impl ClientIdParameter {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::ClientId => "client_id",
            Self::ClientKey => "client_key",
        }
    }
}

impl ServerProvider {
    /// Generic OAuth descriptor. PKCE defaults on; disable only for unsupported providers.
    pub fn new(
        id: impl Into<String>,
        config: OAuthConfig,
        redirect_uri: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            config,
            redirect_uri: redirect_uri.into(),
            client_auth: ClientAuth::SecretPost,
            pkce: true,
            oidc: false,
            client_id_parameter: ClientIdParameter::ClientId,
            scope_separator: ' ',
        }
    }

    /// Facebook Login, including Instagram Graph via Facebook Login (not Instagram Login).
    /// <https://developers.facebook.com/docs/facebook-login/guides/advanced/manual-flow/>
    /// UNVERIFIED: docs returned HTTP 429 on 2026-10-08; endpoint templates, POST auth,
    /// comma scopes and absent web PKCE need confirmation against the registered app.
    /// Caller explicitly selects a supported Graph version; no guessed default version.
    pub fn meta(
        client_id: impl Into<String>,
        secret: impl Into<String>,
        redirect: impl Into<String>,
        graph_version: &str,
    ) -> Result<Self, ServerAuthError> {
        if !graph_version.starts_with('v')
            || graph_version.len() < 4
            || !graph_version[1..].contains('.')
            || !graph_version[1..]
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'.')
        {
            return Err(ServerAuthError::InvalidConfig);
        }
        let config = OAuthConfig::new(
            client_id,
            format!("https://www.facebook.com/{graph_version}/dialog/oauth"),
            format!("https://graph.facebook.com/{graph_version}/oauth/access_token"),
        )
        .with_client_secret(secret);
        let mut p = Self::new("meta", config, redirect);
        p.pkce = false;
        p.scope_separator = ',';
        Ok(p)
    }

    /// <https://learn.microsoft.com/en-us/linkedin/shared/authentication/authorization-code-flow>
    /// Web confidential-client flow uses secret_post. PKCE's separate native endpoint
    /// requires partner enablement; do not substitute it for the public web redirect.
    /// <https://learn.microsoft.com/en-us/linkedin/shared/authentication/authorization-code-flow-native>
    /// OIDC scopes: <https://learn.microsoft.com/en-us/linkedin/consumer/integrations/self-serve/sign-in-with-linkedin-v2>
    pub fn linkedin(
        client_id: impl Into<String>,
        secret: impl Into<String>,
        redirect: impl Into<String>,
    ) -> Self {
        let config = OAuthConfig::new(
            client_id,
            "https://www.linkedin.com/oauth/v2/authorization",
            "https://www.linkedin.com/oauth/v2/accessToken",
        )
        .with_client_secret(secret);
        let mut p = Self::new("linkedin", config, redirect);
        p.pkce = false;
        p.oidc = true;
        p
    }

    /// <https://developers.tiktok.com/docs/en/login-kit-web>
    /// <https://developers.tiktok.com/docs/en/oauth-user-access-token-management>
    /// Web flow uses client_key, comma scopes and secret_post. Documented PKCE is
    /// mobile/desktop only; do not send it with the confidential web registration.
    pub fn tiktok(
        client_key: impl Into<String>,
        secret: impl Into<String>,
        redirect: impl Into<String>,
    ) -> Self {
        let config = OAuthConfig::new(
            client_key,
            "https://www.tiktok.com/v2/auth/authorize/",
            "https://open.tiktokapis.com/v2/oauth/token/",
        )
        .with_client_secret(secret);
        let mut p = Self::new("tiktok", config, redirect);
        p.pkce = false;
        p.client_id_parameter = ClientIdParameter::ClientKey;
        p.scope_separator = ',';
        p
    }

    /// <https://developers.pinterest.com/docs/getting-started/set-up-authentication-and-authorization/>
    /// <https://github.com/pinterest/api-quickstart/blob/main/bash/scripts/get_access_token.sh>
    /// Confidential web flow uses Basic auth and comma scopes; web PKCE is not documented.
    pub fn pinterest(
        client_id: impl Into<String>,
        secret: impl Into<String>,
        redirect: impl Into<String>,
    ) -> Self {
        let config = OAuthConfig::new(
            client_id,
            "https://www.pinterest.com/oauth/",
            "https://api.pinterest.com/v5/oauth/token",
        )
        .with_client_secret(secret);
        let mut p = Self::new("pinterest", config, redirect);
        p.pkce = false;
        p.client_auth = ClientAuth::SecretBasic;
        p.scope_separator = ',';
        p
    }

    pub(super) fn validate(&self) -> Result<(), ServerAuthError> {
        crate::validate_config(&self.config).map_err(|_| ServerAuthError::InvalidConfig)?;
        let url =
            url::Url::parse(&self.redirect_uri).map_err(|_| ServerAuthError::InvalidConfig)?;
        if self.id.trim().is_empty()
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.query().is_some()
            || (matches!(self.client_auth, ClientAuth::None) && !self.pkce)
            || !matches!(self.scope_separator, ' ' | ',')
        {
            return Err(ServerAuthError::InvalidConfig);
        }
        if !matches!(self.client_auth, ClientAuth::None)
            && self
                .config
                .client_secret
                .as_ref()
                .map(Secret::expose)
                .unwrap_or("")
                .is_empty()
        {
            return Err(ServerAuthError::InvalidConfig);
        }
        // Reject parameters that could replace security-critical generated values.
        for (k, _) in &self.config.authorization_params {
            if matches!(
                k.as_str(),
                "response_type"
                    | "client_id"
                    | "client_key"
                    | "redirect_uri"
                    | "state"
                    | "scope"
                    | "code_challenge"
                    | "code_challenge_method"
                    | "nonce"
                    | "client_secret"
                    | "code"
                    | "access_token"
                    | "refresh_token"
            ) {
                return Err(ServerAuthError::InvalidConfig);
            }
        }
        for endpoint in [&self.config.authorization_url, &self.config.token_url] {
            let u = url::Url::parse(endpoint).map_err(|_| ServerAuthError::InvalidConfig)?;
            if u.host_str().is_none()
                || u.query().is_some()
                || u.fragment().is_some()
                || !u.username().is_empty()
                || u.password().is_some()
            {
                return Err(ServerAuthError::InvalidConfig);
            }
        }
        Ok(())
    }
}
