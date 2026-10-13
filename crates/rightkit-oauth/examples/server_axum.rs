//! Route adapter using Axum.
//! Mount behind a TLS reverse proxy. Host owns authenticated begin routes, tenant/account
//! mapping, CSRF protection, callback rate limits and redacted proxy/access logs.
//! This example constructs routes only: no listener or credentials are provisioned here.
use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Extension, Router,
};
use rightkit_oauth::server::{CallbackQuery, ServerAuthError, ServerAuthFlow, TokenStore};
use std::sync::Arc;

#[derive(Clone)]
pub struct OAuthState {
    pub flow: Arc<ServerAuthFlow>,
    pub tokens: Arc<dyn TokenStore>,
}

/// Host middleware derives this from authenticated browser session & registered route.
/// Missing extension rejects callback before token exchange.
#[derive(Clone)]
pub struct CallbackBinding {
    pub provider: String,
    pub account: String,
}

pub fn oauth_routes(state: OAuthState) -> Router {
    Router::new()
        .route("/oauth/callback", get(callback))
        .with_state(state)
}

async fn callback(
    State(state): State<OAuthState>,
    Extension(binding): Extension<CallbackBinding>,
    Query(query): Query<CallbackQuery>,
) -> StatusCode {
    // rightkit-http and store locks are blocking; keep them off Axum's async executor.
    let result = tokio::task::spawn_blocking(move || {
        let complete = state
            .flow
            .complete_bound(query, &binding.provider, &binding.account)?;
        // Ownership comes from authenticated begin(), never callback query parameters.
        // On storage failure, host must securely recover these tokens or require sign-in.
        state
            .tokens
            .put(&complete.account_hint, &complete.provider, &complete.tokens)
    })
    .await;
    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        Ok(Err(ServerAuthError::StateMismatch | ServerAuthError::StateExpired)) => {
            StatusCode::UNAUTHORIZED
        }
        Ok(Err(ServerAuthError::ProviderError | ServerAuthError::InvalidCallback)) => {
            StatusCode::BAD_REQUEST
        }
        _ => StatusCode::BAD_GATEWAY,
    }
}
fn main() {}
