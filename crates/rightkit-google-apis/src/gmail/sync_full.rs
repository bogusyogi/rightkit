use super::*;
use std::future::Future;

/// The Gmail calls a full sync makes. Implemented by the real `Google` client
/// (zero behaviour change) and by a recording fake in tests.
pub trait FullSyncApi: Sync {
    fn profile_history_id(&self) -> impl Future<Output = Result<String, GoogleError>> + Send;
    fn labels(&self) -> impl Future<Output = Result<Vec<LabelInfo>, GoogleError>> + Send;
    fn label_ids(
        &self,
        label: &str,
        cap: u32,
    ) -> impl Future<Output = Result<Vec<String>, GoogleError>> + Send;
    fn message(&self, id: &str)
        -> impl Future<Output = Result<FetchedMessage, GoogleError>> + Send;
}

impl FullSyncApi for Google {
    async fn profile_history_id(&self) -> Result<String, GoogleError> {
        get_profile_history_id(self).await
    }
    async fn labels(&self) -> Result<Vec<LabelInfo>, GoogleError> {
        list_labels(self).await
    }
    async fn label_ids(&self, label: &str, cap: u32) -> Result<Vec<String>, GoogleError> {
        list_label_ids(self, label, cap).await
    }
    async fn message(&self, id: &str) -> Result<FetchedMessage, GoogleError> {
        get_full(self, id).await
    }
}

pub async fn run_full_sync<A: FullSyncApi>(g: &A) -> Result<SyncResult, String> {
    // Baseline cursor for the next delta sync, captured BEFORE any listing or
    // fetching: changes that land while the fetch runs have a higher historyId,
    // so the next incremental sync replays them (idempotent upsert). A failed
    // profile call yields None, which leaves the stored cursor untouched.
    let history_id = g.profile_history_id().await.ok();

    // Label catalog (best-effort: custom labels on synced messages are still
    // preserved via thread_labels even if this single call fails).
    let labels = g.labels().await.unwrap_or_default();

    // Deduped union of message ids across the system folders.
    let mut id_set: HashSet<String> = HashSet::new();
    let mut first_err: Option<String> = None;
    for (label, cap) in SYNC_FOLDERS {
        match g.label_ids(label, *cap).await {
            Ok(ids) => id_set.extend(ids),
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e.to_string());
                }
            }
        }
    }

    // Also pull a capped window from user labels so label-based triage
    // (the target audience's primary workflow) sees recent bodies.
    const USER_LABEL_SYNC_CAP: u32 = 25;
    const USER_LABEL_MAX_LABELS: usize = 10;
    let user_labels: Vec<&LabelInfo> = labels
        .iter()
        .filter(|l| l.label_type == "user")
        .take(USER_LABEL_MAX_LABELS)
        .collect();
    for li in user_labels {
        let _ = g
            .label_ids(&li.id, USER_LABEL_SYNC_CAP)
            .await
            .map(|ids| id_set.extend(ids));
    }
    // If every folder listing failed (e.g. a revoked token), surface the error
    // instead of silently reporting an empty sync.
    if id_set.is_empty() {
        if let Some(e) = first_err {
            return Err(e);
        }
    }

    // Same id order as before; results come back in input order.
    let ids: Vec<String> = id_set.into_iter().collect();
    let ordered = batch_fetch::fetch_with_concurrency(
        &ids,
        batch_fetch::INITIAL_CONCURRENCY,
        |id| async move { g.message(&id).await },
    )
    .await;
    let (messages, skipped) = batch_fetch::partition_outcome(ordered);

    Ok(SyncResult {
        outcome: SyncOutcome { messages, skipped },
        labels,
        history_id,
    })
}

/// The account's current historyId — the cursor stored after a full sync.
async fn get_profile_history_id(g: &Google) -> Result<String, GoogleError> {
    #[derive(Deserialize)]
    struct P {
        #[serde(rename = "historyId", default)]
        history_id: String,
    }
    let p: P = super::deadline::bounded(
        super::deadline::REQUEST_TIMEOUT,
        "profile",
        g.get_json(PROFILE_ENDPOINT, &[]),
    )
    .await?;
    Ok(p.history_id)
}

/// Decide and run the sync for one account: a History delta if we have a cursor
/// (falling back to a full sync when the cursor is stale), else a full sync.
pub async fn run_sync_pass<A: IncrementalSyncApi>(
    g: &A,
    start_history_id: Option<&str>,
) -> Result<SyncWork, String> {
    if let Some(start) = start_history_id {
        let delta = run_delta_sync(g, start).await?;
        if !delta.stale {
            return Ok(SyncWork::Delta(delta));
        }
        // Cursor expired -> fall through to a full resync.
    }
    Ok(SyncWork::Full(run_full_sync(g).await?))
}

/// The whole pass is bounded by `SYNC_PASS_DEADLINE`; on expiry the pass is
/// dropped (cancelled) and a retryable error is returned for the next pass.
pub async fn fetch_sync(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    start_history_id: Option<&str>,
) -> Result<SyncWork, String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );
    with_pass_deadline(
        super::deadline::SYNC_PASS_DEADLINE,
        run_sync_pass(&g, start_history_id),
    )
    .await
}

/// Bound one whole sync pass by `limit`; expiry drops the pass (cancelling it)
/// and returns a retryable error.
pub async fn with_pass_deadline<T>(
    limit: std::time::Duration,
    pass: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match tokio::time::timeout(limit, pass).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "transient error: sync pass exceeded {}s deadline",
            limit.as_secs()
        )),
    }
}
