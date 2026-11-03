//! Single-flight, retry-on-failure, cancel-safe async initializer.
//!
//! Replaces the `OnceLock<Result<(DaemonState, Option<Arc<InlineResidentSource>>), String>>`
//! that used to cache InlineHost's first build attempt forever, including a
//! first-attempt failure (KRN defect #2). This module knows nothing about
//! `DaemonState` or Membrane -- it is generic over the built value so it can
//! be unit-tested with a plain injected builder, and `main_runtime_session_host.rs`
//! is the only caller that supplies the real InlineHost builder.
//!
//! Contract:
//! - Concurrent callers arriving while a build is in flight all await the
//!   SAME attempt; the builder runs exactly once per attempt.
//! - A failed attempt is never cached -- the very next call starts a fresh
//!   attempt (retry within the same process).
//! - A successful attempt is cached until [`InlineHostInitializer::invalidate`]
//!   is called (e.g. because a cached resident lease could no longer be
//!   re-acquired: worker generation changed or the lease was lost).
//! - Every attempt has a monotonically increasing id. A result computed by
//!   an attempt that has since been superseded (by `invalidate`, which may
//!   race an in-flight build) is discarded rather than published as current.
//! - Waiting is cancellation-safe: dropping the calling future while it is
//!   `.await`ing [`InlineHostInitializer::ensure`] never poisons shared
//!   state for the next caller.
//! - The blocking builder runs on `tokio::task::spawn_blocking`, off the
//!   calling async worker thread, so a slow native connect never blocks the
//!   executor.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

type AttemptTx<T> = tokio::sync::watch::Sender<Option<Result<T, String>>>;
type AttemptRx<T> = tokio::sync::watch::Receiver<Option<Result<T, String>>>;

enum State<T> {
    Idle,
    InFlight { attempt_id: u64, rx: AttemptRx<T> },
    Ready { value: T },
}

/// See module docs for the full contract.
pub struct InlineHostInitializer<T: Clone + Send + Sync + 'static> {
    state: Mutex<State<T>>,
    next_attempt_id: AtomicU64,
}

impl<T: Clone + Send + Sync + 'static> Default for InlineHostInitializer<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::Idle),
            next_attempt_id: AtomicU64::new(1),
        }
    }
}

enum Action<T: Clone + Send + Sync + 'static> {
    Join { attempt_id: u64, rx: AttemptRx<T> },
    Drive { attempt_id: u64, tx: AttemptTx<T> },
}

impl<T: Clone + Send + Sync + 'static> InlineHostInitializer<T> {
    /// Force the next [`ensure`](Self::ensure) to start a fresh attempt even
    /// though a value is currently cached. Any attempt already in flight at
    /// the time this is called is orphaned: if it later succeeds, its
    /// result is discarded rather than published as current (see module
    /// docs -- obsolete-attempt discard).
    pub fn invalidate(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *state = State::Idle;
    }

    /// The cached value if a build already succeeded; never starts or joins
    /// a build. Presentation polling uses this so a slow or failing context
    /// build can never stall the terminal loop.
    pub fn peek(&self) -> Option<T> {
        match &*self.state.lock().unwrap_or_else(|e| e.into_inner()) {
            State::Ready { value, .. } => Some(value.clone()),
            _ => None,
        }
    }

    /// Resolve the current value, building it with `builder` if necessary.
    pub async fn ensure<B>(&self, builder: B) -> Result<T, String>
    where
        B: FnOnce() -> Result<T, String> + Send + 'static,
    {
        loop {
            let action = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                match &*state {
                    State::Ready { value, .. } => return Ok(value.clone()),
                    State::InFlight { attempt_id, rx } => Action::Join {
                        attempt_id: *attempt_id,
                        rx: rx.clone(),
                    },
                    State::Idle => {
                        let attempt_id = self.next_attempt_id.fetch_add(1, Ordering::SeqCst);
                        let (tx, rx) = tokio::sync::watch::channel(None);
                        *state = State::InFlight { attempt_id, rx };
                        Action::Drive { attempt_id, tx }
                    }
                }
            };
            match action {
                Action::Join { attempt_id, mut rx } => {
                    match wait_for_attempt(&mut rx).await {
                        Some(result) => {
                            if self.is_current_in_flight(attempt_id) {
                                return match result {
                                    Ok(value) => {
                                        self.publish_ready(attempt_id, value.clone());
                                        Ok(value)
                                    }
                                    Err(error) => {
                                        self.clear_if_current(attempt_id);
                                        Err(error)
                                    }
                                };
                            }
                            // This attempt is no longer the current one
                            // (superseded by `invalidate` or a later
                            // `Ready`/`InFlight`); its result -- success or
                            // failure -- must never become current. Loop
                            // and pick up whatever is current now.
                        }
                        None => {
                            // Driver dropped its sender without publishing a
                            // result (panic path already turns a panic into
                            // an `Err`, so this should not happen in
                            // practice). Treat it as this attempt having
                            // failed so a fresh one can start.
                            self.clear_if_current(attempt_id);
                        }
                    }
                }
                Action::Drive { attempt_id, tx } => {
                    let result =
                        tokio::task::spawn_blocking(builder)
                            .await
                            .unwrap_or_else(|join_error| {
                                Err(format!("inline host build task panicked: {join_error}"))
                            });
                    if self.is_current_in_flight(attempt_id) {
                        match &result {
                            Ok(value) => self.publish_ready(attempt_id, value.clone()),
                            Err(_) => self.clear_if_current(attempt_id),
                        }
                    }
                    // Broadcast to every waiter that joined this attempt,
                    // regardless of whether it is still current -- a joiner
                    // that finds it obsolete loops and re-reads real state.
                    let _ = tx.send(Some(result.clone()));
                    return result;
                }
            }
        }
    }

    fn is_current_in_flight(&self, attempt_id: u64) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        matches!(&*state, State::InFlight { attempt_id: current, .. } if *current == attempt_id)
    }

    fn publish_ready(&self, attempt_id: u64, value: T) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&*state, State::InFlight { attempt_id: current, .. } if *current == attempt_id)
        {
            *state = State::Ready { value };
        }
    }

    fn clear_if_current(&self, attempt_id: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&*state, State::InFlight { attempt_id: current, .. } if *current == attempt_id)
        {
            *state = State::Idle;
        }
    }
}

/// Cancellation-safe wait for an attempt's published result: dropping the
/// enclosing future here just drops `rx`'s borrow; the watch channel keeps
/// broadcasting to any other waiter and the driver task is unaffected.
async fn wait_for_attempt<T: Clone>(rx: &mut AttemptRx<T>) -> Option<Result<T, String>> {
    loop {
        if let Some(result) = rx.borrow_and_update().clone() {
            return Some(result);
        }
        if rx.changed().await.is_err() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::InlineHostInitializer;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn peek_never_builds_and_sees_only_a_published_value() {
        let init = InlineHostInitializer::<u32>::default();
        assert_eq!(init.peek(), None);
        assert_eq!(init.peek(), None, "peeking must not start a build");
        assert_eq!(init.ensure(|| Ok(7)).await, Ok(7));
        assert_eq!(init.peek(), Some(7));
        init.invalidate();
        assert_eq!(init.peek(), None);
    }

    #[tokio::test]
    async fn concurrent_callers_build_exactly_once() {
        let initializer = Arc::new(InlineHostInitializer::<u32>::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let initializer = Arc::clone(&initializer);
            let calls = Arc::clone(&calls);
            handles.push(tokio::spawn(async move {
                initializer
                    .ensure(move || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        Ok(7)
                    })
                    .await
            }));
        }
        for handle in handles {
            assert_eq!(handle.await.unwrap(), Ok(7));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failure_is_not_cached_then_a_later_call_retries_and_succeeds() {
        let initializer = InlineHostInitializer::<u32>::default();
        let first = initializer.ensure(|| Err("boom".to_string())).await;
        assert_eq!(first, Err("boom".to_string()));

        let second = initializer.ensure(|| Ok(42)).await;
        assert_eq!(second, Ok(42));

        // Cached now: a builder that would panic if called proves no
        // further build happens.
        let third = initializer
            .ensure(|| panic!("must not rebuild a cached success"))
            .await;
        assert_eq!(third, Ok(42));
    }

    #[tokio::test]
    async fn obsolete_attempt_result_is_discarded() {
        let initializer = Arc::new(InlineHostInitializer::<u32>::default());

        // Start a slow attempt (A) and let it begin running.
        let (release_a, wait_a) = tokio::sync::oneshot::channel::<()>();
        let init_a = Arc::clone(&initializer);
        let attempt_a = tokio::spawn(async move {
            init_a
                .ensure(move || {
                    let _ = wait_a.blocking_recv();
                    Ok(1_u32)
                })
                .await
        });
        // Give the driver a moment to move into InFlight before invalidating.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Force a fresh attempt (B) while A is still blocked in its builder.
        initializer.invalidate();
        let second = initializer.ensure(|| Ok(2_u32)).await;
        assert_eq!(second, Ok(2));

        // Now let A finish. Its success must not overwrite B's cached value.
        let _ = release_a.send(());
        let a_result = attempt_a.await.unwrap();
        assert_eq!(
            a_result,
            Ok(1),
            "the joiner of an obsolete attempt still gets its own result"
        );

        let third = initializer
            .ensure(|| panic!("must still be serving B's cached value, not A's stale one"))
            .await;
        assert_eq!(third, Ok(2));
    }

    #[tokio::test]
    async fn invalidate_after_success_forces_a_fresh_build() {
        let initializer = InlineHostInitializer::<u32>::default();
        let first = initializer.ensure(|| Ok(1)).await;
        assert_eq!(first, Ok(1));

        initializer.invalidate();

        let second = initializer.ensure(|| Ok(2)).await;
        assert_eq!(
            second,
            Ok(2),
            "invalidate must force a fresh build, not replay the cache"
        );
    }

    #[tokio::test]
    async fn dropping_a_waiter_does_not_wedge_state_for_the_next_caller() {
        let initializer = Arc::new(InlineHostInitializer::<u32>::default());
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let init_clone = Arc::clone(&initializer);
        let driver = tokio::spawn(async move {
            init_clone
                .ensure(move || {
                    let _ = wait.blocking_recv();
                    Ok(9_u32)
                })
                .await
        });

        // A second caller joins the same in-flight attempt, then is
        // cancelled (dropped) before it resolves.
        let init_clone2 = Arc::clone(&initializer);
        let joiner = tokio::spawn(async move { init_clone2.ensure(|| Ok(0)).await });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        joiner.abort();
        let _ = joiner.await;

        let _ = release.send(());
        assert_eq!(driver.await.unwrap(), Ok(9));

        // The initializer must still serve the cached value normally.
        let after = initializer.ensure(|| panic!("must be cached")).await;
        assert_eq!(after, Ok(9));
    }
}
