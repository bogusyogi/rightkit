use super::*;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;
use tokio::time::Instant;

/// Build a `FetchedMessage` with just enough fields populated to test
/// ordering and grouping — we don't need headers/bodies.
fn msg(id: &str) -> FetchedMessage {
    FetchedMessage {
        id: id.to_string(),
        thread_id: format!("t-{id}"),
        from_name: String::new(),
        from_addr: String::new(),
        to_addrs: String::new(),
        cc_addrs: String::new(),
        reply_to: None,
        list_unsubscribe: None,
        auth_results: None,
        subject: String::new(),
        snippet: String::new(),
        internal_date: 0,
        unread: false,
        label_ids: Vec::new(),
        body_plain: None,
        body_html: None,
        has_attachments: false,
        attachments: Vec::new(),
        size_estimate: None,
        rfc822_message_id: None,
    }
}

/// Wall-clock overlap test: N=10 ids, each getter sleeps 200ms. With
/// concurrency=6 the slowest critical path is two waves of ~6+4 in
/// flight, so elapsed should be well under the sequential 10×200ms=2000ms
/// — and must be strictly less.
#[tokio::test]
async fn n_fetches_overlap_wall_clock_below_sequential_sum() {
    let n: usize = 10;
    let per_id_sleep = Duration::from_millis(200);
    let ids: Vec<String> = (0..n).map(|i| format!("id-{i}")).collect();
    let started = Instant::now();
    let out = fetch_with_concurrency(&ids, INITIAL_CONCURRENCY, |id| async move {
        tokio::time::sleep(per_id_sleep).await;
        Ok(msg(&id))
    })
    .await;
    let elapsed = started.elapsed();

    assert_eq!(out.len(), n, "every id returns exactly one result");
    // Ordering by input index.
    let inputs: Vec<String> = (0..n).map(|i| format!("id-{i}")).collect();
    let actual: Vec<String> = out.iter().map(|(_, id, _)| id.clone()).collect();
    assert_eq!(actual, inputs, "results come back in input order");
    // Sequential sum would be ~2000ms; six-wide should fit in two waves.
    let sequential_sum = per_id_sleep * n as u32;
    assert!(
        elapsed < sequential_sum,
        "elapsed {elapsed:?} must be < sequential sum {sequential_sum:?} (proves overlap)",
    );
}

/// Recording sleeper: returns immediately and logs each requested duration,
/// so backoff assertions never depend on wall-clock time.
fn recorder() -> (
    Arc<std::sync::Mutex<Vec<Duration>>>,
    impl Fn(Duration) -> std::future::Ready<()>,
) {
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let l = log.clone();
    (log, move |d| {
        l.lock().unwrap().push(d);
        std::future::ready(())
    })
}

/// Rate-limited twice, then succeeds: sleeps are exactly [1s, 2s].
#[tokio::test]
async fn rate_limited_getter_is_backed_off_and_eventually_succeeds() {
    let counter = Arc::new(AtomicUsize::new(0));
    let ids = vec!["backoff-1".to_string()];
    let counter_for_closure = counter.clone();
    let (log, sleeper) = recorder();
    let out = fetch_with_concurrency_using(
        &ids,
        INITIAL_CONCURRENCY,
        move |id| {
            let counter = counter_for_closure.clone();
            async move {
                let attempt = counter.fetch_add(1, AtomicOrdering::SeqCst);
                if attempt < 2 {
                    Err(GoogleError::RateLimited("quota".into()))
                } else {
                    Ok(msg(&id))
                }
            }
        },
        sleeper,
    )
    .await;

    assert_eq!(out.len(), 1);
    let (_, id_back, res) = &out[0];
    assert_eq!(id_back, "backoff-1");
    assert!(res.is_ok(), "third attempt must succeed: {res:?}");
    assert_eq!(
        *log.lock().unwrap(),
        vec![Duration::from_secs(1), Duration::from_secs(2)]
    );
}

/// Never recovers: the budget is exactly [1s, 2s, 4s], then the last error
/// surfaces after 4 attempts. Bounded, never indefinite.
#[tokio::test]
async fn backoff_budget_is_bounded_and_surfaces_error() {
    let ids = vec!["die-1".to_string()];
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    let (log, sleeper) = recorder();
    let out = fetch_with_concurrency_using(
        &ids,
        INITIAL_CONCURRENCY,
        move |_id| {
            a.fetch_add(1, AtomicOrdering::SeqCst);
            async { Err::<FetchedMessage, _>(GoogleError::RateLimited("quota".into())) }
        },
        sleeper,
    )
    .await;

    assert_eq!(out.len(), 1);
    let (_, _, res) = &out[0];
    assert!(matches!(res, Err(GoogleError::RateLimited(_))));
    assert_eq!(attempts.load(AtomicOrdering::SeqCst), 4);
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4)
        ]
    );
}

/// A retry sleeping in backoff must not hold a download slot: with cap 1 and
/// a sleeper that waits for the second id to finish, the batch completes.
#[tokio::test]
async fn backoff_sleep_releases_the_permit() {
    let ids = vec!["slow".to_string(), "fast".to_string()];
    let fast_done = Arc::new(tokio::sync::Notify::new());
    let first = Arc::new(AtomicBool::new(true));
    let (fd, fd2) = (fast_done.clone(), fast_done.clone());
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        fetch_with_concurrency_using(
            &ids,
            1,
            move |id| {
                let (fd, first) = (fd.clone(), first.clone());
                async move {
                    if id == "slow" && first.swap(false, AtomicOrdering::SeqCst) {
                        return Err(GoogleError::RateLimited("q".into()));
                    }
                    if id == "fast" {
                        fd.notify_one();
                    }
                    Ok(msg(&id))
                }
            },
            move |_d| {
                let fd = fd2.clone();
                async move { fd.notified().await }
            },
        ),
    )
    .await
    .expect("sleeping retry must not starve the other id");
    assert!(out.iter().all(|(_, _, r)| r.is_ok()));
}

/// Mixed-result grouping: some ids Ok, some RateLimited-beyond-budget,
/// one returns an unrelated Http error. The partition helper must
/// produce exactly the right `messages` and `skipped` lists in input
/// order.
#[test]
fn partition_outcome_splits_ok_and_err_in_input_order() {
    let ordered = vec![
        (0, "a".to_string(), Ok(msg("a"))),
        (
            1,
            "b".to_string(),
            Err(GoogleError::RateLimited("q".into())),
        ),
        (2, "c".to_string(), Ok(msg("c"))),
        (
            3,
            "d".to_string(),
            Err(GoogleError::Http {
                status: 404,
                body: "no".into(),
            }),
        ),
    ];
    let (messages, skipped) = partition_outcome(ordered);
    let ids_msgs: Vec<&str> = messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids_msgs, vec!["a", "c"]);
    assert_eq!(skipped.len(), 2);
    assert_eq!(skipped[0].id, "b");
    assert_eq!(skipped[0].reason, "rate_limited");
    assert_eq!(skipped[1].id, "d");
    assert_eq!(skipped[1].reason, "http");
}

/// The rate-limit detector must include both RateLimited and HTTP 403;
/// other 4xx must NOT be retried (those are real validation failures).
#[test]
fn rate_limit_detector_matches_quota_shapes_only() {
    assert!(is_rate_limited(&GoogleError::RateLimited("x".into())));
    assert!(is_rate_limited(&GoogleError::Http {
        status: 403,
        body: "rateLimitExceeded".into()
    }));
    assert!(!is_rate_limited(&GoogleError::Http {
        status: 404,
        body: "notFound".into()
    }));
    assert!(!is_rate_limited(&GoogleError::Http {
        status: 400,
        body: "bad".into()
    }));
    assert!(!is_rate_limited(&GoogleError::Auth("invalid_grant".into())));
    assert!(!is_rate_limited(&GoogleError::Transient("net".into())));
    assert!(!is_rate_limited(&GoogleError::Parse("bad json".into())));
}

/// Concurrency cap is honored under near-uniform work: with per_id sleep
/// longer than the cap-divided wall-clock, observed parallelism
/// (concurrent in-flight sleeps) should approach the cap.
#[tokio::test]
async fn concurrency_reaches_initial_cap_during_a_batch() {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let n: usize = 12; // > INITIAL_CONCURRENCY so multiple waves are needed
    let per_id_sleep = Duration::from_millis(150);
    let ids: Vec<String> = (0..n).map(|i| format!("p-{i}")).collect();

    let peak_observer = peak.clone();
    let in_flight_observer = in_flight.clone();
    let _ = fetch_with_concurrency(&ids, INITIAL_CONCURRENCY, move |id| {
        let in_flight = in_flight_observer.clone();
        let peak = peak_observer.clone();
        async move {
            let now = in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            let mut cur = peak.load(AtomicOrdering::Relaxed);
            while now > cur {
                match peak.compare_exchange_weak(
                    cur,
                    now,
                    AtomicOrdering::Relaxed,
                    AtomicOrdering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(observed) => cur = observed,
                }
            }
            tokio::time::sleep(per_id_sleep).await;
            in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
            Ok(msg(&id))
        }
    })
    .await;

    let observed_peak = peak.load(AtomicOrdering::Relaxed);
    assert!(
        observed_peak >= INITIAL_CONCURRENCY,
        "peak in-flight {observed_peak} should reach INITIAL_CONCURRENCY={INITIAL_CONCURRENCY}",
    );
    assert!(
        observed_peak <= INITIAL_CONCURRENCY,
        "peak in-flight {observed_peak} must NOT exceed the cap of {INITIAL_CONCURRENCY}",
    );
}

/// REDUCED_CONCURRENCY is selectable by the caller (the L12 "drop to 2
/// for the remainder of that batch" contract is the caller's signal to
/// the next batch). Verify the cap is honored when explicitly chosen.
#[tokio::test]
async fn reduced_concurrency_caps_inflight_at_two() {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let n: usize = 6; // > REDUCED_CONCURRENCY so multiple waves
    let per_id_sleep = Duration::from_millis(100);
    let ids: Vec<String> = (0..n).map(|i| format!("r-{i}")).collect();

    let peak_observer = peak.clone();
    let in_flight_observer = in_flight.clone();
    let _ = fetch_with_concurrency(&ids, REDUCED_CONCURRENCY, move |id| {
        let in_flight = in_flight_observer.clone();
        let peak = peak_observer.clone();
        async move {
            let now = in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            let mut cur = peak.load(AtomicOrdering::Relaxed);
            while now > cur {
                match peak.compare_exchange_weak(
                    cur,
                    now,
                    AtomicOrdering::Relaxed,
                    AtomicOrdering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(observed) => cur = observed,
                }
            }
            tokio::time::sleep(per_id_sleep).await;
            in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
            Ok(msg(&id))
        }
    })
    .await;

    let observed_peak = peak.load(AtomicOrdering::Relaxed);
    assert!(
        observed_peak >= REDUCED_CONCURRENCY,
        "peak in-flight {observed_peak} should reach REDUCED_CONCURRENCY={REDUCED_CONCURRENCY}",
    );
    assert!(
        observed_peak <= REDUCED_CONCURRENCY,
        "peak in-flight {observed_peak} must NOT exceed the cap of {REDUCED_CONCURRENCY}",
    );
}

/// Run one id that fails `fails` times with `mk()`, then succeeds; return sleeps.
async fn sleeps_for(fails: usize, mk: fn() -> GoogleError) -> Vec<Duration> {
    let ids = vec!["ra-1".to_string()];
    let n = Arc::new(AtomicUsize::new(0));
    let (log, sleeper) = recorder();
    let _ = fetch_with_concurrency_using(
        &ids,
        INITIAL_CONCURRENCY,
        move |id| {
            let k = n.fetch_add(1, AtomicOrdering::SeqCst);
            async move {
                if k < fails {
                    Err(mk())
                } else {
                    Ok(msg(&id))
                }
            }
        },
        sleeper,
    )
    .await;
    let v = log.lock().unwrap().clone();
    v
}

fn advised(secs: u64) -> GoogleError {
    GoogleError::RateLimitedAfter {
        message: "q".into(),
        retry_after: Duration::from_secs(secs),
    }
}

#[tokio::test]
async fn advised_delay_longer_than_step_is_used() {
    let v = sleeps_for(3, || advised(10)).await;
    assert_eq!(v, vec![Duration::from_secs(10); 3]);
}

#[tokio::test]
async fn advised_delay_shorter_than_step_keeps_schedule() {
    let v = sleeps_for(3, || advised(0)).await;
    assert_eq!(
        v,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4)
        ]
    );
}

#[tokio::test]
async fn advised_delay_beyond_cap_is_capped() {
    let v = sleeps_for(3, || advised(3600)).await;
    assert_eq!(v, vec![Duration::from_secs(60); 3]);
}

#[tokio::test]
async fn no_advised_delay_keeps_exact_series() {
    let v = sleeps_for(3, || GoogleError::RateLimited("q".into())).await;
    assert_eq!(
        v,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4)
        ]
    );
}
