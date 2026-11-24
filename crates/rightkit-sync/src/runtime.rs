//! The run loop of a desktop sync host (features `cipher` and `drive`): one pass per family each round, the duty lease, the status a
//! host shows, and the pass thread.
//!
//! The host builds the engines (one per [`Family`], each behind its sealing backend) and the lease backend, and owns everything
//! that is its own: the switches, the token, where state lives, what a revoked computer erases. This module owns the shape of a
//! round so every host runs it the same way:
//!
//! 1. With `duty` set, the lease is ticked ([`crate::duty_lease::tick`]) and the result recorded in [`Status`].
//! 2. Each family's engine makes one pass, in the order given; the outcome words, consent requests and the last problem are
//!    recorded in [`Status`].
//! 3. [`Runner::run_round`] returns whether any pass reached the store (`Synced` or `Pending`), which is what a deadman clock
//!    resets on.
//!
//! Nothing here logs a record's content, a key, a token or a code.

use crate::drive::DriveAuth;
use crate::duty_lease::{self, State};
use crate::families::{Family, FamilyMapper};
use crate::{AuthProvider, Backend, ConsentChoice, PassOutcome, SyncEngine, SyncResult};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Seconds between rounds.
pub const PASS_SECS: u64 = 60;
/// Seconds the pass thread waits after it starts before its first round.
pub const START_DELAY_SECS: u64 = 20;

/// What the last rounds did, in words and counts only.
#[derive(Default)]
pub struct Status {
    pub last_pass_ms: i64,
    pub last_error: Option<String>,
    pub families: Vec<(&'static str, String)>,
    pub lease: Option<State>,
    pub lease_at: Option<Instant>,
    /// Family, from-scope and to-scope of each pass paused for the owner's consent.
    pub consent: Vec<(&'static str, String, String)>,
}

impl Status {
    pub const fn new() -> Self {
        Status {
            last_pass_ms: 0,
            last_error: None,
            families: Vec::new(),
            lease: None,
            lease_at: None,
            consent: Vec::new(),
        }
    }
}

/// A [`Status`] a host keeps in a `static`.
pub struct SharedStatus(Mutex<Status>);

impl SharedStatus {
    pub const fn new() -> Self {
        SharedStatus(Mutex::new(Status::new()))
    }

    pub fn lock(&self) -> MutexGuard<'_, Status> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Back to the empty status (sync turned off, or this computer wiped).
    pub fn reset(&self) {
        *self.lock() = Status::default();
    }

    pub fn set_error(&self, code: &str) {
        self.lock().last_error = Some(code.to_string());
    }

    /// Whether this computer may do the noticing loop's work now: true while this computer holds the lease and while the lease is
    /// unknown (a lone computer keeps working through an outage); false only while another computer is known to hold it and
    /// renewed within three renewal periods. A host that has sync switched off answers true without asking.
    pub fn duty_allowed(&self) -> bool {
        let status = self.lock();
        match (&status.lease, status.lease_at) {
            (Some(State::Other { .. }), Some(at)) => {
                at.elapsed() > Duration::from_millis((duty_lease::RENEW_MS * 3) as u64)
            }
            _ => true,
        }
    }
}

impl Default for SharedStatus {
    fn default() -> Self {
        Self::new()
    }
}

/// The word a status shows for a family.
pub fn family_word(family: Family) -> &'static str {
    match family {
        Family::Memory => "memory",
        Family::Profile => "profile",
        Family::Inbox => "inbox",
        Family::Facts => "facts",
        Family::Schedules => "schedules",
        Family::Instructions => "instructions",
        Family::Cursors => "cursors",
    }
}

/// The word a status shows for a pass's outcome.
pub fn outcome_word(outcome: &PassOutcome) -> String {
    match outcome {
        PassOutcome::Synced => "synced".into(),
        PassOutcome::Pending(_) => "pending".into(),
        PassOutcome::Deferred { secs } => format!("waiting {} s", *secs as i64),
        PassOutcome::Unavailable(_) => "unavailable".into(),
        PassOutcome::Disabled => "off".into(),
        PassOutcome::ConsentRequired(_) => "needs your consent".into(),
        PassOutcome::Cancelled => "cancelled".into(),
        PassOutcome::Failed(_) => "failed".into(),
    }
}

/// Why [`Runner::grant_consent`] did not grant.
pub enum ConsentError {
    UnknownFamily,
    Refused(String),
}

/// The engines and the lease backend of one running sync.
pub struct Runner {
    pub engines: Vec<(Family, SyncEngine<FamilyMapper>)>,
    pub lease: Arc<dyn Backend>,
    pub device: String,
}

impl Runner {
    pub fn new(
        engines: Vec<(Family, SyncEngine<FamilyMapper>)>,
        lease: Arc<dyn Backend>,
        device: String,
    ) -> Self {
        Runner {
            engines,
            lease,
            device,
        }
    }

    /// One round: the lease (only when `duty`: this computer can hold duty), then a pass for each family. `rank` is this computer's
    /// lease rank (1 for the always-on computer, 0 for an open laptop). Returns whether any pass reached the store.
    pub fn run_round(&mut self, status: &SharedStatus, duty: bool, rank: i64, now_ms: i64) -> bool {
        if duty {
            match duty_lease::tick(self.lease.as_ref(), &self.device, rank, now_ms) {
                Ok(state) => {
                    let mut status = status.lock();
                    status.lease = Some(state);
                    status.lease_at = Some(Instant::now());
                }
                Err(_) => status.set_error("lease_unreachable"),
            }
        }
        let mut families = Vec::new();
        let mut consent = Vec::new();
        let mut last_error = None;
        let mut reached = false;
        for (family, engine) in self.engines.iter_mut() {
            let report = engine.run_pass();
            reached |= matches!(
                report.outcome,
                PassOutcome::Synced | PassOutcome::Pending(_)
            );
            if let PassOutcome::ConsentRequired(request) = &report.outcome {
                consent.push((
                    family_word(*family),
                    request.from.clone(),
                    request.to.clone(),
                ));
            }
            if matches!(
                report.outcome,
                PassOutcome::Failed(_) | PassOutcome::Unavailable(_)
            ) {
                last_error = Some(format!(
                    "{}_{}",
                    family_word(*family),
                    outcome_word(&report.outcome)
                ));
            }
            families.push((family_word(*family), outcome_word(&report.outcome)));
        }
        let mut status = status.lock();
        status.last_pass_ms = now_ms;
        status.families = families;
        status.consent = consent;
        status.last_error = last_error;
        reached
    }

    /// The owner's yes to syncing into a new account or under a new key: merges this computer's items into it.
    pub fn grant_consent(&mut self, family: &str, to: &str) -> Result<(), ConsentError> {
        let (_, engine) = self
            .engines
            .iter_mut()
            .find(|(f, _)| family_word(*f) == family)
            .ok_or(ConsentError::UnknownFamily)?;
        engine
            .grant_consent(to, ConsentChoice::Merge)
            .map_err(|e| ConsentError::Refused(e.to_string()))
    }
}

/// The engine's `AuthProvider` over Drive's sign-in state, with an account scope that binds both a key scope and the account, so a
/// rotated key or another account pauses a family for the owner's consent instead of publishing into it. The host supplies the
/// three questions only it can answer.
pub struct KeyedAuth {
    inner: DriveAuth,
    key_scope: Box<dyn Fn() -> Option<String> + Send + Sync>,
    account: Box<dyn Fn() -> Option<String> + Send + Sync>,
    switch: Box<dyn Fn() -> bool + Send + Sync>,
}

impl KeyedAuth {
    /// `key_scope` is the sync key's scope string, `account` the signed-in account, `switch` the owner's sync switch.
    pub fn new(
        inner: DriveAuth,
        key_scope: impl Fn() -> Option<String> + Send + Sync + 'static,
        account: impl Fn() -> Option<String> + Send + Sync + 'static,
        switch: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        KeyedAuth {
            inner,
            key_scope: Box::new(key_scope),
            account: Box::new(account),
            switch: Box::new(switch),
        }
    }
}

impl AuthProvider for KeyedAuth {
    fn enabled(&self) -> bool {
        self.inner.enabled() && (self.switch)()
    }

    fn availability(&self) -> SyncResult<()> {
        self.inner.availability()
    }

    fn generation(&self) -> u64 {
        self.inner.generation()
    }

    fn account_scope(&self) -> Option<String> {
        let key_scope = (self.key_scope)()?;
        let account = (self.account)()?;
        Some(rightkit_fs::digest::sha256_bytes(
            format!("{key_scope}|{account}").as_bytes(),
        ))
    }

    fn bearer(&self) -> SyncResult<Option<String>> {
        self.inner.bearer()
    }
}

/// Sleeps up to `span`, returning early (within a second) once `stop` is set.
pub fn sleep_for(span: Duration, stop: &AtomicBool) {
    let end = Instant::now() + span;
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Starts the pass thread: `resume` once (finish an interrupted wipe, before any store or engine is opened), a start delay, then
/// `round` every [`PASS_SECS`] until `stop` is set. A panic in `round` calls `on_panic` and the loop goes on. `false` when the
/// thread could not be started.
pub fn spawn_pass_thread(
    stop: &'static AtomicBool,
    resume: fn(),
    round: fn(),
    on_panic: fn(),
) -> bool {
    stop.store(false, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("sync-passes".into())
        .spawn(move || {
            resume();
            sleep_for(Duration::from_secs(START_DELAY_SECS), stop);
            while !stop.load(Ordering::Relaxed) {
                if std::panic::catch_unwind(round).is_err() {
                    on_panic();
                }
                sleep_for(Duration::from_secs(PASS_SECS), stop);
            }
        })
        .is_ok()
}
