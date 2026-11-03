// Bounded-concurrency Gmail batch fetch (L12). The prior loop in
// `fetch_full_batch` / `fetch_metadata_batch` awaited one message id at a time
// — at the historical 50-ids-backfill batch that serialized ~50 round-trips
// per pass, dominated by per-request latency. This module runs them in
// parallel with a configurable cap, re-attempts rate-limited messages with
// exponential backoff, and leaves the next batch's caller to apply the
// "drop-to-2" rate-limit response.
//
// Apply-side ordering: results are sorted by INPUT index so callers that
// today rely on the sequential loop see a `SyncOutcome.messages` ordering
// that is byte-identical to the prior code path. Per-message failures still
// become `SkipInfo` at the gmail.rs call site — never swallowed.
use super::FetchedMessage;
use crate::google::GoogleError;
use futures::stream::{self, StreamExt};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;
use tokio::sync::Semaphore;

/// Initial concurrency cap for a batch fetch. Six in flight is well under
/// Gmail's per-user and per-IP rate ceilings while still collapsing a 50-id
/// backfill batch into ~one round-trip's worth of wall-clock.
pub const INITIAL_CONCURRENCY: usize = 6;

/// Concurrency floor after Gmail signals a rate-limit response. The
/// L12 contract says DROP concurrency to 2 for the remainder of that batch.
/// Because `buffer_unordered`'s cap is fixed at stream-build time, the cap
/// is selected up-front based on whether the caller observed rate-limit
/// responses in the previous batch — see `fetch_with_concurrency`.
pub const REDUCED_CONCURRENCY: usize = 2;

/// Ceiling for any one backoff sleep, including a server-advised Retry-After.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// The outer rate-limit retry schedule, as the shared client's `RetryPolicy`: four tries in all
/// (three retries) with sleeps of 1s, 2s, 4s (no jitter), and a server-advised Retry-After wins
/// when it is longer, up to `MAX_BACKOFF`.
fn backoff_policy() -> rightkit_http::RetryPolicy {
    rightkit_http::RetryPolicy {
        max_attempts: 4,
        initial_backoff: Duration::from_secs(1),
        max_backoff: MAX_BACKOFF,
        multiplier: 2.0,
        jitter: 0.0,
        honor_retry_after: true,
        max_retry_after: MAX_BACKOFF,
        ..rightkit_http::RetryPolicy::none()
    }
}

/// Sleep before retry `attempt` (0-based): the policy's `delay`, i.e. `max(advised, scheduled)`
/// capped at `MAX_BACKOFF`.
pub fn backoff_for(attempt: usize, e: &GoogleError) -> Duration {
    let advised = match e {
        GoogleError::RateLimitedAfter { retry_after, .. } => Some(*retry_after),
        _ => None,
    };
    backoff_policy().delay(attempt as u32 + 1, advised)
}

/// True when `e` is the kind of error Gmail uses to mean "you are asking too
/// fast, slow down". Two cases:
///   * `GoogleError::RateLimited` — Google's quota-exhausted response after the
///     shared client's own internal retries were exhausted.
///   * `GoogleError::Http { status: 403, .. }` — Gmail returns 403 with a
///     `rateLimitExceeded` / `userRateLimitExceeded` body for the same shape of
///     overload; the shared client surfaces that without retrying. Treat it
///     the same as a rate-limit for our outer backoff + concurrency drop.
pub fn is_rate_limited(e: &GoogleError) -> bool {
    matches!(
        e,
        GoogleError::RateLimited(_)
            | GoogleError::RateLimitedAfter { .. }
            | GoogleError::Http { status: 403, .. }
    )
}

type Fetched = (usize, String, Result<FetchedMessage, GoogleError>);

/// Shared scheduler state for one batch.
struct Gate {
    semaphore: Semaphore,
    rate_limited_seen: AtomicBool,
    forfeited: AtomicUsize,
    max_forfeit: usize,
}

/// Fetch one id. Each attempt holds a permit only while the request is in
/// flight; the permit is released BEFORE any backoff sleep so a sleeping
/// retry never occupies a download slot. Once any rate-limit response has been
/// seen in the batch, returned permits are forfeited (never re-released) until
/// admission has shrunk to `REDUCED_CONCURRENCY` — a one-way ratchet for the
/// remainder of the batch. Only rate-limit errors retry (1s, 2s, 4s); `Auth`,
/// other `Http 4xx` and `Parse` return immediately.
async fn fetch_with_backoff<F, Fut, S, SFut>(
    id: String,
    fetch_one: &F,
    gate: &Gate,
    sleeper: &S,
) -> (String, Result<FetchedMessage, GoogleError>)
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<FetchedMessage, GoogleError>>,
    S: Fn(Duration) -> SFut,
    SFut: std::future::Future<Output = ()>,
{
    let mut attempt = 0usize;
    loop {
        let permit = gate
            .semaphore
            .acquire()
            .await
            .expect("semaphore never closed during a batch");
        let res = fetch_one(id.clone()).await;
        let limited = matches!(&res, Err(e) if is_rate_limited(e));
        if limited {
            gate.rate_limited_seen.store(true, AtomicOrdering::Relaxed);
        }
        if gate.rate_limited_seen.load(AtomicOrdering::Relaxed) {
            let prior = gate.forfeited.fetch_add(1, AtomicOrdering::AcqRel);
            if prior < gate.max_forfeit {
                permit.forget();
            } else {
                drop(permit);
            }
        } else {
            drop(permit);
        }
        match res {
            Err(e) if limited => {
                if attempt < (backoff_policy().max_attempts - 1) as usize {
                    sleeper(backoff_for(attempt, &e)).await;
                    attempt += 1;
                    continue;
                }
                return (id, Err(e));
            }
            other => return (id, other),
        }
    }
}

/// Run `fetch_one` over `ids` with bounded concurrency. Returns
/// `(input_index, id, Result<...>)` ordered by input index so callers see the
/// same ordering as a sequential loop. `limit` is the starting cap (pass
/// `INITIAL_CONCURRENCY`).
pub async fn fetch_with_concurrency<F, Fut>(
    ids: &[String],
    limit: usize,
    fetch_one: F,
) -> Vec<Fetched>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<FetchedMessage, GoogleError>>,
{
    fetch_with_concurrency_using(ids, limit, fetch_one, |d| tokio::time::sleep(d)).await
}

/// `fetch_with_concurrency` with an injectable sleep (tests record durations
/// instead of waiting wall-clock time).
pub async fn fetch_with_concurrency_using<F, Fut, S, SFut>(
    ids: &[String],
    limit: usize,
    fetch_one: F,
    sleeper: S,
) -> Vec<Fetched>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<FetchedMessage, GoogleError>>,
    S: Fn(Duration) -> SFut,
    SFut: std::future::Future<Output = ()>,
{
    if ids.is_empty() {
        return Vec::new();
    }
    let cap = limit.max(1);
    let reduced = REDUCED_CONCURRENCY.max(1).min(cap);
    let gate = Gate {
        semaphore: Semaphore::new(cap),
        rate_limited_seen: AtomicBool::new(false),
        forfeited: AtomicUsize::new(0),
        max_forfeit: cap.saturating_sub(reduced),
    };
    let (gate, fetch_one, sleeper) = (&gate, &fetch_one, &sleeper);
    // The semaphore (not the stream) bounds in-flight requests, so every id is
    // polled and tasks parked in a backoff sleep do not block new ones.
    let mut futs = stream::iter(ids.iter().cloned().enumerate())
        .map(|(idx, id)| async move {
            let (id_back, res) = fetch_with_backoff(id, fetch_one, gate, sleeper).await;
            (idx, id_back, res)
        })
        .buffer_unordered(ids.len());

    let mut joined: Vec<Fetched> = Vec::with_capacity(ids.len());
    while let Some(item) = futs.next().await {
        joined.push(item);
    }
    joined.sort_by_key(|(idx, _, _)| *idx);
    joined
}

/// Build the `SyncOutcome`'s two lists from the parallel-fetch output.
/// Convenience for `gmail.rs` — keeps the partitioning invariant next to the
/// parallel-policy code.
pub fn partition_outcome(
    ordered: Vec<(usize, String, Result<FetchedMessage, GoogleError>)>,
) -> (Vec<FetchedMessage>, Vec<super::SkipInfo>) {
    let mut messages = Vec::new();
    let mut skipped = Vec::new();
    for (_idx, id, res) in ordered {
        match res {
            Ok(m) => messages.push(m),
            Err(e) => skipped.push(super::SkipInfo {
                id,
                reason: e.class().to_string(),
            }),
        }
    }
    (messages, skipped)
}
