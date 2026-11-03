// Time bounds for the Gmail sync path. Per-request transport limits (connect,
// response, body, retry budget) belong to rightkit-http; `google.rs` applies the
// JSON request budget to each call. The outer bound here also covers work that
// sits outside one HTTP request, such as the token mint and the 401 refresh, so
// every Gmail call site still wraps its await.
use crate::google::GoogleError;
use std::future::Future;
use std::time::Duration;

/// Total bound for one Gmail request including the 401-refresh and 429/5xx
/// retries (connect + response + body). Shared with `google.rs`.
pub const REQUEST_TIMEOUT: Duration = crate::google::REQUEST_DEADLINE;

/// Overall deadline for one account's whole sync pass (`fetch_sync`).
pub const SYNC_PASS_DEADLINE: Duration = Duration::from_secs(300);

/// Run `fut`, failing with a retryable `Transient` error if it exceeds `limit`.
pub async fn bounded<T, F>(limit: Duration, what: &str, fut: F) -> Result<T, GoogleError>
where
    F: Future<Output = Result<T, GoogleError>>,
{
    match tokio::time::timeout(limit, fut).await {
        Ok(r) => r,
        Err(_) => Err(GoogleError::Transient(format!(
            "{what} timed out after {}s",
            limit.as_secs_f32()
        ))),
    }
}

/// `Google::get_json` bounded by `REQUEST_TIMEOUT`.
pub async fn get_json<T: serde::de::DeserializeOwned>(
    g: &crate::google::Google,
    what: &str,
    url: &str,
    query: &[(&str, String)],
) -> Result<T, GoogleError> {
    bounded(REQUEST_TIMEOUT, what, g.get_json(url, query)).await
}
