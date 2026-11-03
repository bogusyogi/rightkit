//! Control lease with epoch fencing and an acked input sequence (CodeRight
//! PTY-002 shape): a stale controller can never inject.
//!
//! * `acquire_lease` grants a lease on one app. Every grant gets a new, strictly
//!   increasing **epoch**; a takeover bumps it, so the previous holder is fenced.
//! * While a lease is active every input must carry a [`Fence`]
//!   `{epoch, seq}` (in `ActOptions::fence` / `DeliveryOptions::fence`). A wrong
//!   epoch is `StaleLease`; nothing executes.
//! * `seq` is the controller's input sequence. The host executes `seq ==
//!   acked + 1`, and only then advances **acked**. A retransmitted `seq <=
//!   acked` is answered `Replayed{acked_seq}` and is *not* executed again; a gap
//!   is `OutOfOrder{expected}`. The controller therefore resends from `acked + 1`
//!   after any reconnect without double-injecting.
//! * A refused input (policy, admission, risk) still consumes its `seq`: it was
//!   processed, the result says it was refused, and the controller moves on.
//! * Unfenced inputs to a leased app are refused (`Blocked`): fail closed.

use crate::model::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct LeaseRequest {
    /// Who is asking (a controller id, a CLI session id). Free text, not an authority.
    pub holder: String,
    /// The lease lapses after this long without renewal (`None` = until released).
    pub ttl: Option<Duration>,
    /// Replace a live lease held by someone else (their next input fails `StaleLease`).
    pub takeover: bool,
}

impl LeaseRequest {
    pub fn new(holder: &str) -> Self {
        LeaseRequest {
            holder: holder.into(),
            ttl: None,
            takeover: false,
        }
    }
    pub fn ttl(mut self, d: Duration) -> Self {
        self.ttl = Some(d);
        self
    }
    pub fn takeover(mut self) -> Self {
        self.takeover = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub pid: i32,
    pub holder: String,
    pub epoch: u64,
}

impl Lease {
    /// Fence for the controller's `seq`-th input (the first input is `seq` 1).
    pub fn fence(&self, seq: u64) -> Fence {
        Fence {
            epoch: self.epoch,
            seq,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fence {
    pub epoch: u64,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseStatus {
    pub pid: i32,
    pub holder: String,
    pub epoch: u64,
    /// Highest input sequence the host has processed. Resend from `acked_seq + 1`.
    pub acked_seq: u64,
    pub expires_in_ms: Option<u64>,
}

struct State {
    holder: String,
    epoch: u64,
    acked: u64,
    in_flight: bool,
    expires: Option<Instant>,
    ttl: Option<Duration>,
}

#[derive(Default)]
pub(crate) struct Leases {
    epoch: AtomicU64,
    map: Mutex<HashMap<i32, State>>,
}

/// What `admit` decided for one input.
pub(crate) enum Admitted {
    Unleased,
    Fenced { epoch: u64, seq: u64 },
}

impl Leases {
    fn live(map: &mut HashMap<i32, State>, pid: i32) -> Option<&mut State> {
        if map
            .get(&pid)
            .is_some_and(|s| s.expires.is_some_and(|e| Instant::now() >= e))
        {
            map.remove(&pid);
        }
        map.get_mut(&pid)
    }

    /// Grant; the second value is the displaced holder, if any.
    pub fn acquire(&self, pid: i32, req: &LeaseRequest) -> Result<(Lease, Option<(String, u64)>)> {
        let mut map = self.map.lock().unwrap();
        let displaced = match Self::live(&mut map, pid) {
            Some(s) if !req.takeover && s.holder != req.holder => {
                return Err(Error::LeaseHeld {
                    holder: s.holder.clone(),
                    epoch: s.epoch,
                });
            }
            Some(s) => Some((s.holder.clone(), s.epoch)),
            None => None,
        };
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        map.insert(
            pid,
            State {
                holder: req.holder.clone(),
                epoch,
                acked: 0,
                in_flight: false,
                expires: req.ttl.map(|t| Instant::now() + t),
                ttl: req.ttl,
            },
        );
        Ok((
            Lease {
                pid,
                holder: req.holder.clone(),
                epoch,
            },
            displaced,
        ))
    }

    pub fn release(&self, pid: i32, epoch: u64) -> bool {
        let mut map = self.map.lock().unwrap();
        if Self::live(&mut map, pid).is_some_and(|s| s.epoch == epoch) {
            map.remove(&pid);
            true
        } else {
            false
        }
    }

    /// Extend a ttl lease. False when the epoch is stale.
    pub fn renew(&self, pid: i32, epoch: u64) -> bool {
        let mut map = self.map.lock().unwrap();
        match Self::live(&mut map, pid) {
            Some(s) if s.epoch == epoch => {
                s.expires = s.ttl.map(|t| Instant::now() + t);
                true
            }
            _ => false,
        }
    }

    pub fn status(&self, pid: i32) -> Option<LeaseStatus> {
        let mut map = self.map.lock().unwrap();
        Self::live(&mut map, pid).map(|s| LeaseStatus {
            pid,
            holder: s.holder.clone(),
            epoch: s.epoch,
            acked_seq: s.acked,
            expires_in_ms: s
                .expires
                .map(|e| e.saturating_duration_since(Instant::now()).as_millis() as u64),
        })
    }

    pub fn all_pids(&self) -> Vec<i32> {
        self.map.lock().unwrap().keys().copied().collect()
    }

    pub fn drop_pid(&self, pid: i32) {
        self.map.lock().unwrap().remove(&pid);
    }

    pub fn admit(&self, pid: i32, fence: Option<Fence>) -> Result<Admitted> {
        let mut map = self.map.lock().unwrap();
        let st = Self::live(&mut map, pid);
        match (st, fence) {
            (None, None) => Ok(Admitted::Unleased),
            (None, Some(f)) => Err(Error::StaleLease {
                presented: f.epoch,
                current: None,
            }),
            (Some(s), None) => Err(Error::Blocked(format!(
                "pid {pid} is leased by {} (epoch {}); present a fence",
                s.holder, s.epoch
            ))),
            (Some(s), Some(f)) => {
                if f.epoch != s.epoch {
                    return Err(Error::StaleLease {
                        presented: f.epoch,
                        current: Some(s.epoch),
                    });
                }
                if f.seq <= s.acked {
                    return Err(Error::Replayed { acked_seq: s.acked });
                }
                if f.seq != s.acked + 1 {
                    return Err(Error::OutOfOrder {
                        expected: s.acked + 1,
                        got: f.seq,
                    });
                }
                if s.in_flight {
                    return Err(Error::Invalid(
                        "another input from this lease is still executing".into(),
                    ));
                }
                s.in_flight = true;
                Ok(Admitted::Fenced {
                    epoch: s.epoch,
                    seq: f.seq,
                })
            }
        }
    }

    /// The input was processed (done or refused): advance the ack. A takeover
    /// during execution leaves the new holder's state untouched.
    pub fn ack(&self, pid: i32, epoch: u64, seq: u64) {
        let mut map = self.map.lock().unwrap();
        if let Some(s) = map.get_mut(&pid) {
            if s.epoch == epoch {
                s.acked = s.acked.max(seq);
                s.in_flight = false;
            }
        }
    }
}
