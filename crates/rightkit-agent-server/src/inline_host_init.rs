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
