//! In-flight host work: one token, one deadline, one outcome.
//!
//! The controller never shows the result of host work (a copy, a host action) before the host
//! reports it. When the work starts it takes a token and a deadline from a [`Tracker`]; the
//! worker thread reports back with that token; the driver resolves it at the moment it handles
//! the report, passing the current `Instant`. A report that arrives after the surface it belonged
//! to was replaced, after a newer attempt began, or after the attempt already settled is *stale*
//! and changes nothing. A report for the current token whose deadline has passed *when it is
//! resolved* is [`Resolution::Overdue`]: the attempt failed, exactly as if the timer had found it
//! (it never counts as success, whether or not the timer already ran). A deadline that passes
//! with no report is a failure ([`Tracker::expire`]). A token resolves at most once: every path
//! that returns a key removes the attempt.
//!
//! Pure logic: no threads, no clock of its own (the caller passes `Instant`s), no config. It
//! tests on every host.

use std::time::Instant;

use crate::clock::after;

/// What [`Tracker::resolve`] found for a report.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Resolution<K> {
    /// The current token, answered before its deadline: act on the host's result.
    Resolved(K),
    /// The current token, but its deadline had passed when the report was resolved: the attempt
    /// failed; the host's result must be ignored.
    Overdue(K),
    /// Not the current token (expired, cancelled, superseded or already resolved).
    Stale,
}

#[derive(Clone, Debug)]
struct Pending<K> {
    token: u64,
    key: K,
    deadline: Instant,
}

/// At most one attempt in flight. `K` is what the attempt is about (a row id, a surface).
#[derive(Debug)]
pub(crate) struct Tracker<K> {
    next_token: u64,
    pending: Option<Pending<K>>,
}

impl<K: Clone> Tracker<K> {
    pub(crate) const fn new() -> Tracker<K> {
        Tracker {
            next_token: 1,
            pending: None,
        }
    }

    /// Start an attempt. `None` while another one is still in flight (the click is ignored, so
    /// one press cannot queue two host actions). Tokens are never reused. The deadline is
    /// `deadline_ms` from `now`, clamped to one day (a host number never overflows the clock).
    pub(crate) fn begin(&mut self, key: K, now: Instant, deadline_ms: u64) -> Option<u64> {
        if self.pending.is_some() {
            return None;
        }
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1).max(1);
        self.pending = Some(Pending {
            token,
            key,
            deadline: after(now, deadline_ms),
        });
        Some(token)
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn key(&self) -> Option<&K> {
        self.pending.as_ref().map(|pending| &pending.key)
    }

    /// The host reported for `token`, and `now` is the moment it is being resolved. Returns what
    /// the attempt was about, once: [`Resolution::Resolved`] before the deadline,
    /// [`Resolution::Overdue`] at or after it (the attempt is removed either way, so the token
    /// cannot be used again), [`Resolution::Stale`] for any other token (the current attempt, if
    /// there is one, is left alone).
    pub(crate) fn resolve(&mut self, token: u64, now: Instant) -> Resolution<K> {
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.token == token)
        {
            return Resolution::Stale;
        }
        match self.pending.take() {
            Some(pending) if now >= pending.deadline => Resolution::Overdue(pending.key),
            Some(pending) => Resolution::Resolved(pending.key),
            None => Resolution::Stale,
        }
    }

    /// The deadline passed with no report: the attempt failed. Returns its key, once.
    pub(crate) fn expire(&mut self, now: Instant) -> Option<K> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| now >= pending.deadline)
        {
            self.pending.take().map(|pending| pending.key)
        } else {
            None
        }
    }

    /// Forget the attempt (its surface went away). A later report is stale.
    pub(crate) fn cancel(&mut self) {
        self.pending = None;
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tracker() -> Tracker<&'static str> {
        Tracker::new()
    }

    #[test]
    fn a_report_for_the_current_token_resolves_once() {
        let mut t = tracker();
        let now = Instant::now();
        let token = t.begin("row", now, 1000).unwrap();
        assert!(t.is_pending());
        assert_eq!(t.key(), Some(&"row"));
        assert_eq!(t.resolve(token, now), Resolution::Resolved("row"));
        assert!(!t.is_pending());
        assert_eq!(t.resolve(token, now), Resolution::Stale);
    }

    #[test]
    fn a_second_attempt_is_refused_while_one_is_in_flight() {
        let mut t = tracker();
        let now = Instant::now();
        assert!(t.begin("a", now, 1000).is_some());
        assert!(t.begin("b", now, 1000).is_none());
    }

    #[test]
    fn nothing_expires_before_the_deadline_and_the_attempt_fails_at_it() {
        let mut t = tracker();
        let now = Instant::now();
        t.begin("row", now, 500).unwrap();
        assert_eq!(t.deadline(), Some(now + Duration::from_millis(500)));
        assert_eq!(t.expire(now + Duration::from_millis(499)), None);
        assert!(t.is_pending());
        assert_eq!(t.expire(now + Duration::from_millis(500)), Some("row"));
        assert!(!t.is_pending());
        assert_eq!(t.deadline(), None);
    }

    #[test]
    fn a_report_after_the_deadline_is_stale() {
        let mut t = tracker();
        let now = Instant::now();
        let token = t.begin("row", now, 10).unwrap();
        assert_eq!(t.expire(now + Duration::from_millis(11)), Some("row"));
        assert_eq!(t.resolve(token, now), Resolution::Stale);
    }

    #[test]
    fn a_report_for_an_older_attempt_cannot_resolve_a_newer_one() {
        let mut t = tracker();
        let now = Instant::now();
        let first = t.begin("one", now, 10).unwrap();
        t.expire(now + Duration::from_millis(20));
        let second = t.begin("two", now, 1000).unwrap();
        assert_ne!(first, second);
        assert_eq!(t.resolve(first, now), Resolution::Stale);
        assert!(t.is_pending());
        assert_eq!(t.resolve(second, now), Resolution::Resolved("two"));
    }

    #[test]
    fn cancel_makes_a_late_report_stale() {
        let mut t = tracker();
        let now = Instant::now();
        let token = t.begin("row", now, 1000).unwrap();
        t.cancel();
        assert_eq!(t.resolve(token, now), Resolution::Stale);
        assert_eq!(t.expire(now + Duration::from_secs(60)), None);
        // A fresh attempt is possible afterwards.
        assert!(t.begin("row", now, 1000).is_some());
    }

    #[test]
    fn an_idle_tracker_has_no_deadline() {
        let t = tracker();
        assert_eq!(t.deadline(), None);
        assert!(!t.is_pending());
    }
}
