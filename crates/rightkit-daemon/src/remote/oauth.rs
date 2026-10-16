//! Adapter from the daemon's OAuth callback class to `rightkit-oauth`'s
//! server flow. The flow consumes the one-time state and PKCE verifier; this
//! adapter additionally refuses a callback whose completed provider differs
//! from the route's provider, and persists tokens through the host `TokenStore`.

use std::sync::Arc;

use rightkit_oauth::server::{CallbackQuery, ServerAuthError, ServerAuthFlow, TokenStore};

use crate::remote::core::{OAuthCallbackHandler, OAuthOutcome};

pub struct FlowOAuthHandler {
    pub flow: Arc<ServerAuthFlow>,
    pub tokens: Arc<dyn TokenStore>,
}

impl OAuthCallbackHandler for FlowOAuthHandler {
    fn complete(
        &self,
        provider: &str,
        code: Option<String>,
        state: Option<String>,
        error: Option<String>,
    ) -> OAuthOutcome {
        let done = match self.flow.complete(CallbackQuery { code, state, error }) {
            Ok(d) => d,
            Err(
                ServerAuthError::StateMismatch
                | ServerAuthError::StateExpired
                | ServerAuthError::InvalidCallback
                | ServerAuthError::ProviderError,
            ) => return OAuthOutcome::Rejected,
            Err(_) => return OAuthOutcome::Unavailable,
        };
        if done.provider != provider {
            // Tokens for another provider never attach to this route.
            return OAuthOutcome::Rejected;
        }
        match self
            .tokens
            .put(&done.account_hint, &done.provider, &done.tokens)
        {
            Ok(()) => OAuthOutcome::Completed,
            Err(_) => OAuthOutcome::Unavailable,
        }
    }
}
