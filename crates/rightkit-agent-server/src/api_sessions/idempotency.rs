//! Shared nonce/digest claim for retryable session REST ingress (CR3 #57 item 5).
//!
//! `post_message` owns the richest flow (admission fence, dispatch fence). Every
//! other state-changing route that a client may retry uses this simpler
//! claim -> run -> complete wrapper over the same durable
//! `machine_request_idempotency` table and the same storage claim primitives:
//!
//! * routes without a body (`POST /sessions/{id}/compact`) carry the key in an
//!   `Idempotency-Key` header; a body nonce, when present, wins over the header;
//! * no `nonce` in the request: behavior is unchanged (no deduplication);
//! * fresh nonce: the claim is recorded, the handler runs once, and its exact
//!   response is committed for replay;
//! * same nonce and same digest after commit: the original response is replayed
//!   and the handler is not run again;
//! * same nonce, different digest: `409 CONFLICT`;
//! * same nonce while the original has no proven terminal outcome: `409`
//!   (never executed a second time automatically; crash after dispatch stays
//!   unknown until reconciliation, and no exactly-once external effect is
//!   promised);
//! * handler refused with a 4xx (rejected before any effect): the claim is
//!   released so the client may retry the same nonce;
//! * handler failed with a 5xx: the claim is marked `unknown`.

use std::future::Future;

use axum::{http::StatusCode, response::Json};

use crate::api::{now_iso, SharedStorage};

pub(super) const MAX_NONCE_BYTES: usize = 256;

/// Trim and validate a client nonce. Blank means "no nonce".
pub(super) fn normalize_nonce(raw: Option<&str>) -> Result<Option<String>, StatusCode> {
    let key = raw.map(str::trim).filter(|key| !key.is_empty());
    match key {
        Some(key) if key.len() > MAX_NONCE_BYTES => Err(StatusCode::BAD_REQUEST),
        Some(key) => Ok(Some(key.to_owned())),
        None => Ok(None),
    }
}

pub(super) const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

/// Resolve the client key: the body nonce when present, else the
/// `Idempotency-Key` header. Blank means "no key"; a non-UTF-8 or oversized
/// header is a `400`.
pub(super) fn resolve_nonce(
    body_nonce: Option<&str>,
    headers: &axum::http::HeaderMap,
) -> Result<Option<String>, StatusCode> {
    if let Some(key) = normalize_nonce(body_nonce)? {
        return Ok(Some(key));
    }
    match headers.get(IDEMPOTENCY_KEY_HEADER) {
        Some(value) => {
            let raw = value.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
            normalize_nonce(Some(raw))
        }
        None => Ok(None),
    }
}

/// Digest binding the operation and the canonical request body. The nonce
/// itself must not be part of `canonical`, so a retry with the same body but a
/// new key still digests equal and a changed body digests differently.
pub(super) fn request_digest(operation: &str, canonical: &serde_json::Value) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(operation.as_bytes());
    hasher.update(b"\n");
    hasher.update(canonical.to_string().as_bytes());
    hasher.finalize()
}

/// Run `run` under the nonce claim described in the module docs.
pub(super) async fn run_idempotent<F, Fut>(
    storage: Option<SharedStorage>,
    session_id: &str,
    operation: &str,
    nonce: Option<String>,
    digest: String,
    run: F,
) -> Result<Json<serde_json::Value>, StatusCode>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Json<serde_json::Value>, StatusCode>>,
{
    let Some(key) = nonce else {
        return run().await;
    };
    let Some(storage) = storage else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let correlation_id = rightkit_agent_engine_protocol::uuid_v7();
    let decision = storage
        .lock()
        .await
        .claim_idempotent_request_wire(
            session_id,
            operation,
            &key,
            &digest,
            &correlation_id,
            &now_iso(),
        )
        .map_err(|error| {
            tracing::error!(session_id, operation, %error, "idempotency claim failed");
            StatusCode::SERVICE_UNAVAILABLE
        })?;
    match decision["decision"].as_str() {
        Some("fresh") => {}
        Some("replay") => {
            let original = decision["responseJson"]
                .as_str()
                .and_then(|value| serde_json::from_str(value).ok())
                .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
            return Ok(Json(original));
        }
        // Key reused with a different request digest, or the original request
        // has no proven terminal outcome: never run it a second time.
        _ => return Err(StatusCode::CONFLICT),
    }

    let outcome = run().await;
    match outcome {
        Ok(Json(response)) => {
            let response_json = serde_json::to_string(&response).unwrap_or_default();
            let committed = storage
                .lock()
                .await
                .complete_idempotent_request(
                    session_id,
                    operation,
                    &key,
                    &digest,
                    &response_json,
                    &now_iso(),
                )
                .unwrap_or(false);
            if !committed {
                // The effect happened but its replay receipt did not persist;
                // the row stays unfinished so a retry reports a conflict
                // instead of applying the effect again.
                tracing::error!(session_id, operation, "idempotency receipt not persisted");
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
            Ok(Json(response))
        }
        Err(status) => {
            let store = storage.lock().await;
            if status.is_client_error() {
                // Rejected before any effect: free the key for a retry.
                let _ = store.release_idempotent_request(session_id, operation, &key, &digest);
            } else {
                let _ = store.mark_idempotent_request_unknown(
                    session_id,
                    operation,
                    &key,
                    &digest,
                    &now_iso(),
                );
            }
            Err(status)
        }
    }
}
