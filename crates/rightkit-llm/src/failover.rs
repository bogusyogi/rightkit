//! Route failover with a circuit breaker. Ported from CodeRight
//! `providers::failover_middleware` semantics:
//!
//! 1. Fail over only on retryable conditions (transport, timeout, 408/425/429/5xx,
//!    overload bodies). Auth and validation errors surface immediately.
//! 2. Streaming fails over only BEFORE the first token; later errors surface as
//!    [`LlmError::MidStream`] with no partial-answer stitching.
//! 3. A first-token timeout fails over.
//! 4. When every route fails, one terminal error names each route and reason; the
//!    chain never loops back to the head.
//! 5. After `breaker_threshold` fail-overs away from a route within
//!    `breaker_window`, the route is skipped for `breaker_cool_down`.

use std::collections::HashMap;
use std::fmt;
use std::ops::ControlFlow;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Attempt, LlmError};
use crate::provider::Provider;
use crate::types::*;

#[derive(Clone)]
pub struct Route {
    pub name: String,
    pub provider: Arc<dyn Provider>,
    /// Model slug to use on this route (providers name models differently).
    pub model: String,
}

impl Route {
    pub fn new(name: &str, provider: Arc<dyn Provider>, model: &str) -> Self {
        Self {
            name: name.into(),
            provider,
            model: model.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FailoverPolicy {
    pub first_token_timeout: Duration,
    pub breaker_threshold: u32,
    pub breaker_window: Duration,
    pub breaker_cool_down: Duration,
}

impl Default for FailoverPolicy {
    fn default() -> Self {
        Self {
            first_token_timeout: Duration::from_secs(60),
            breaker_threshold: 3,
            breaker_window: Duration::from_secs(600),
            breaker_cool_down: Duration::from_secs(300),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailoverEvent {
    pub from: String,
    pub to: Option<String>,
    pub reason: String,
}

#[derive(Default)]
struct Breaker {
    away: Vec<Instant>,
    cooling_until: Option<Instant>,
}

type Observer = Box<dyn Fn(&FailoverEvent) + Send + Sync>;

/// Which errors move a call to the next route. Applies only before the first token: a
/// route that already delivered output is never failed over, whatever this policy says.
#[derive(Clone, Default)]
pub enum FailoverOn {
    /// Retryable errors only: [`LlmError::is_failover_trigger`]. The default.
    #[default]
    Retryable,
    /// Every error except [`LlmError::Cancelled`] (the caller stopped the call),
    /// [`LlmError::MidStream`] (output already reached the caller), and
    /// [`LlmError::InvalidRequest`] (a caller-side request problem that every route would
    /// repeat). Auth errors and rejected output (`Malformed`, `Truncated`, `Provider`, ...)
    /// do fail over.
    AnyError,
    /// A predicate chosen by the caller. It is consulted only before the first token.
    Custom(Arc<dyn Fn(&LlmError) -> bool + Send + Sync>),
}

impl FailoverOn {
    /// [`FailoverOn::Custom`] from a closure.
    pub fn custom(f: impl Fn(&LlmError) -> bool + Send + Sync + 'static) -> Self {
        FailoverOn::Custom(Arc::new(f))
    }

    /// Whether `error` moves the call to the next route under this policy.
    pub fn matches(&self, error: &LlmError) -> bool {
        match self {
            FailoverOn::Retryable => error.is_failover_trigger(),
            FailoverOn::AnyError => !matches!(
                error,
                LlmError::Cancelled | LlmError::MidStream(_) | LlmError::InvalidRequest(_)
            ),
            FailoverOn::Custom(f) => f(error),
        }
    }
}

impl fmt::Debug for FailoverOn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FailoverOn::Retryable => f.write_str("Retryable"),
            FailoverOn::AnyError => f.write_str("AnyError"),
            FailoverOn::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

pub struct FailoverChain {
    routes: Vec<Route>,
    policy: FailoverPolicy,
    breakers: Mutex<HashMap<String, Breaker>>,
    observer: Option<Observer>,
    on: FailoverOn,
}

impl FailoverChain {
    pub fn new(routes: Vec<Route>, policy: FailoverPolicy) -> Self {
        Self {
            routes,
            policy,
            breakers: Mutex::new(HashMap::new()),
            observer: None,
            on: FailoverOn::default(),
        }
    }
    pub fn on_failover(mut self, f: impl Fn(&FailoverEvent) + Send + Sync + 'static) -> Self {
        self.observer = Some(Box::new(f));
        self
    }
    /// Choose which errors fail over. The default is [`FailoverOn::Retryable`].
    pub fn fail_over_on(mut self, on: FailoverOn) -> Self {
        self.on = on;
        self
    }

    /// Routes to try, in order; cooling routes are skipped unless that would
    /// leave nothing to try.
    fn order(&self) -> Vec<usize> {
        let now = Instant::now();
        let guard = self.breakers.lock().unwrap();
        let live: Vec<usize> = (0..self.routes.len())
            .filter(|&i| {
                guard
                    .get(&self.routes[i].name)
                    .and_then(|b| b.cooling_until)
                    .map(|t| now >= t)
                    .unwrap_or(true)
            })
            .collect();
        if live.is_empty() {
            (0..self.routes.len()).collect()
        } else {
            live
        }
    }

    fn record_away(&self, name: &str) {
        let now = Instant::now();
        let mut g = self.breakers.lock().unwrap();
        let b = g.entry(name.to_string()).or_default();
        b.away
            .retain(|t| now.duration_since(*t) < self.policy.breaker_window);
        b.away.push(now);
        if b.away.len() as u32 >= self.policy.breaker_threshold {
            b.cooling_until = Some(now + self.policy.breaker_cool_down);
            b.away.clear();
        }
    }

    fn record_success(&self, name: &str) {
        if let Some(b) = self.breakers.lock().unwrap().get_mut(name) {
            b.away.clear();
            b.cooling_until = None;
        }
    }

    fn notify(&self, from: &str, to: Option<&str>, reason: &str) {
        if let Some(o) = &self.observer {
            o(&FailoverEvent {
                from: from.into(),
                to: to.map(String::from),
                reason: reason.into(),
            });
        }
    }

    /// `attempt_route` sets its `sent` flag once the route delivered output; from then on the
    /// call is never failed over.
    fn run<T>(
        &self,
        req: &ChatRequest,
        mut attempt_route: impl FnMut(&Route, &ChatRequest, &mut bool) -> Result<T, LlmError>,
    ) -> Result<T, LlmError> {
        let order = self.order();
        let mut attempts: Vec<Attempt> = Vec::new();
        for (pos, &i) in order.iter().enumerate() {
            let route = &self.routes[i];
            let mut r = req.clone();
            r.model = route.model.clone();
            let mut sent = false;
            match attempt_route(route, &r, &mut sent) {
                Ok(v) => {
                    self.record_success(&route.name);
                    return Ok(v);
                }
                Err(e) if !sent && self.on.matches(&e) => {
                    let reason = e.to_string();
                    self.record_away(&route.name);
                    let next = order.get(pos + 1).map(|&n| self.routes[n].name.as_str());
                    self.notify(&route.name, next, &reason);
                    attempts.push(Attempt {
                        route: route.name.clone(),
                        reason,
                    });
                }
                Err(e) => return Err(e),
            }
        }
        Err(LlmError::AllRoutesFailed(attempts))
    }
}

enum Msg {
    Event(StreamEvent),
    Done(Result<ChatResponse, LlmError>),
}

impl Provider for FailoverChain {
    fn id(&self) -> &str {
        "failover"
    }

    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        self.run(req, |route, r, _| route.provider.complete(r))
    }

    fn stream(
        &self,
        req: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ChatResponse, LlmError> {
        self.run(req, |route, r, sent| {
            let (tx, rx) = mpsc::channel::<Msg>();
            let provider = route.provider.clone();
            let request = r.clone();
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let cancel2 = cancel.clone();
            thread::spawn(move || {
                let tx_events = tx.clone();
                let result = provider.stream(&request, &mut |ev| {
                    if cancel2.load(std::sync::atomic::Ordering::SeqCst)
                        || tx_events.send(Msg::Event(ev)).is_err()
                    {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                });
                let _ = tx.send(Msg::Done(result));
            });

            // Phase 1: wait for the first event within the first-token deadline.
            let deadline = Instant::now() + self.policy.first_token_timeout;
            loop {
                let msg = if *sent {
                    rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
                } else {
                    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                };
                match msg {
                    Ok(Msg::Event(ev)) => {
                        *sent = true;
                        if on_event(ev).is_break() {
                            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                            return Err(LlmError::Cancelled);
                        }
                    }
                    Ok(Msg::Done(res)) => return res,
                    Err(RecvTimeoutError::Timeout) => {
                        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                        return Err(LlmError::Http(rightkit_http::HttpError::Timeout {
                            message: "first token deadline".into(),
                            phase: rightkit_http::Phase::Body,
                        }));
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(LlmError::Malformed(
                            "stream worker ended unexpectedly".into(),
                        ))
                    }
                }
            }
        })
    }
}
