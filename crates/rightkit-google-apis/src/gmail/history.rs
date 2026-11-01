use super::*;

// History API record shapes (only the fields we act on).
#[derive(Deserialize)]
pub struct HistMsgRef {
    pub id: String,
}
#[derive(Deserialize)]
pub struct HistMsgWrap {
    pub message: HistMsgRef,
}
#[derive(Deserialize, Default)]
pub struct HistoryRecord {
    #[serde(rename = "messagesAdded", default)]
    pub messages_added: Vec<HistMsgWrap>,
    #[serde(rename = "messagesDeleted", default)]
    pub messages_deleted: Vec<HistMsgWrap>,
    #[serde(rename = "labelsAdded", default)]
    pub labels_added: Vec<HistMsgWrap>,
    #[serde(rename = "labelsRemoved", default)]
    pub labels_removed: Vec<HistMsgWrap>,
}

/// Collapse history records into (ids to re-fetch, ids deleted). A message that
/// was added OR had labels added/removed is re-fetched in full so we get its
/// authoritative current label set — this is what makes an archive-on-phone
/// (labelsRemoved INBOX) correctly drop the thread from the Inbox folder.
/// Deleted ids are removed and never re-fetched.
pub fn collect_history_changes(records: &[HistoryRecord]) -> (HashSet<String>, HashSet<String>) {
    let mut changed = HashSet::new();
    let mut deleted = HashSet::new();
    for rec in records {
        for m in &rec.messages_added {
            changed.insert(m.message.id.clone());
        }
        for m in &rec.labels_added {
            changed.insert(m.message.id.clone());
        }
        for m in &rec.labels_removed {
            changed.insert(m.message.id.clone());
        }
        for m in &rec.messages_deleted {
            deleted.insert(m.message.id.clone());
        }
    }
    // A deleted message must not also be re-fetched.
    for d in &deleted {
        changed.remove(d);
    }
    (changed, deleted)
}

/// One page of `history.list` (only the fields we act on).
#[derive(Deserialize)]
pub struct HistoryPage {
    #[serde(default)]
    pub history: Vec<HistoryRecord>,
    #[serde(rename = "historyId", default)]
    pub history_id: Option<String>,
    #[serde(rename = "nextPageToken", default)]
    pub next_page_token: Option<String>,
}

/// The extra Gmail call an incremental pass makes beyond a full sync: paged
/// `history.list`. Changed messages are fetched with `FullSyncApi::message`.
/// Implemented by the real `Google` client (zero behaviour change) and by a
/// recording fake in tests.
pub trait IncrementalSyncApi: FullSyncApi {
    fn history_page(
        &self,
        start_history_id: &str,
        page_token: Option<&str>,
    ) -> impl std::future::Future<Output = Result<HistoryPage, GoogleError>> + Send;
}

impl IncrementalSyncApi for Google {
    async fn history_page(
        &self,
        start_history_id: &str,
        page_token: Option<&str>,
    ) -> Result<HistoryPage, GoogleError> {
        let mut query = vec![
            ("startHistoryId", start_history_id.to_string()),
            ("maxResults", "500".to_string()),
        ];
        if let Some(t) = page_token {
            query.push(("pageToken", t.to_string()));
        }
        super::deadline::bounded(
            super::deadline::REQUEST_TIMEOUT,
            "history.list",
            self.get_json::<HistoryPage>(HISTORY_ENDPOINT, &query),
        )
        .await
    }
}

/// Run a History API delta from `start_history_id`, fetching changed messages in
/// full. A 404 (cursor too old) returns `stale = true` so the caller falls back
/// to a full sync.
pub async fn run_delta_sync<A: IncrementalSyncApi>(
    g: &A,
    start_history_id: &str,
) -> Result<DeltaOutcome, String> {
    let mut records: Vec<HistoryRecord> = Vec::new();
    let mut new_history_id: Option<String> = None;
    let mut page_token: Option<String> = None;
    loop {
        let resp = match g
            .history_page(start_history_id, page_token.as_deref())
            .await
        {
            Ok(r) => r,
            Err(GoogleError::Http { status: 404, .. }) => {
                return Ok(DeltaOutcome {
                    changed: vec![],
                    deleted: vec![],
                    new_history_id: None,
                    latest_history_id: None,
                    stale: true,
                    failed: 0,
                    failed_ids: vec![],
                });
            }
            Err(e) => return Err(e.to_string()),
        };
        records.extend(resp.history);
        if resp.history_id.is_some() {
            new_history_id = resp.history_id;
        }
        match resp.next_page_token {
            Some(t) => page_token = Some(t),
            None => break,
        }
    }

    let (changed_ids, deleted) = collect_history_changes(&records);
    let mut changed = Vec::with_capacity(changed_ids.len());
    let mut failed = 0usize;
    let mut failed_ids = Vec::new();
    let ids: Vec<String> = changed_ids.into_iter().collect();
    let ordered = batch_fetch::fetch_with_concurrency(
        &ids,
        batch_fetch::INITIAL_CONCURRENCY,
        |id| async move { g.message(&id).await },
    )
    .await;
    for (_idx, id, res) in ordered {
        match res {
            Ok(m) => changed.push(m),
            // 404: the message was removed between the history event and our
            // fetch — correctly absent, safe to drop.
            Err(GoogleError::Http { status: 404, .. }) => {}
            // Any other failure (after retries) is unreconciled.
            Err(_) => {
                failed += 1;
                failed_ids.push(id);
            }
        }
    }
    // Do NOT advance the cursor past changes we couldn't fetch: the next delta
    // must re-attempt them (idempotent re-apply). There is no recurring full
    // resync, so advancing here would lose an inbound message permanently.
    let latest_history_id = new_history_id.clone();
    let new_history_id = if failed > 0 { None } else { new_history_id };
    Ok(DeltaOutcome {
        changed,
        deleted: deleted.into_iter().collect(),
        new_history_id,
        latest_history_id,
        stale: false,
        failed,
        failed_ids,
    })
}
