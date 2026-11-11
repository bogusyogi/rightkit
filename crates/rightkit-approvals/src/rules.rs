//! The rules, once for every store: file a request, decide it, use it.
//!
//! * [`file()`]: a repeat of a waiting request gets the same id; past the pending cap is
//!   [`Error::Busy`]. Nothing is decided here.
//! * [`decide`]: only a pending, unexpired request is decided; an approval carrying a proof is
//!   accepted only when the proof verifies; a decision carrying the display digest of what the
//!   decider was shown is accepted only when it equals the waiting request's
//!   ([`Error::Shown`]). The move is one compare-and-set.
//! * [`take`]: an approval is used by one act, for the same kind, scope, door, filer and
//!   destination it was given for, with the same arguments and revision, before it expires.
//!   With a proof, a pending request is decided and used in one move (an approval token
//!   presented with the act).
//! * [`session`]: a live session grant for this kind, scope, door and client, refreshed by use.
//! * [`claim`], [`settle`], [`recover`]: for an act whose outcome is only known after it ran
//!   (a send, a delete, a payment), the approval is first claimed by one act, with the same
//!   checks as [`take`], and then settled done or failed. A claim nobody settles (a crash, a
//!   timeout, a handler that vanished) is settled by [`recover`] as the app's reconciler says,
//!   and as `unknown_outcome` when it cannot tell: never as "did not happen", never retried.
//! * [`revoke`], [`revoke_where`]: a pending or approved request is withdrawn before it is
//!   used (revoked by the decider, cancelled by the asker, every grant of an account). A
//!   claimed one is past revoking: its act may be running.
//!
//! A proof that carries a nonce spends it in [`Proof::verify`]; a proof is bound to one record,
//! and a record is decided once, so no replay reaches a second request through these rules.
//! Audit before effect is the [`crate::Audited`] store wrapper, not a rule here.

use crate::error::Error;
use crate::model::{Change, Grant, Origin, Record, Request, State};
use crate::proof::Proof;
use crate::store::Store;

/// Caps on waiting requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Most requests waiting at once.
    pub pending_cap: usize,
    /// Count the cap per client (`origin.client`) instead of over the whole store.
    pub per_client: bool,
}

/// What [`file()`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filed {
    /// A new pending record.
    New(Record),
    /// The same request was already waiting; this is it.
    Existing(Record),
}

impl Filed {
    pub fn record(&self) -> &Record {
        match self {
            Filed::New(r) | Filed::Existing(r) => r,
        }
    }

    pub fn id(&self) -> &str {
        &self.record().request.id
    }

    pub fn is_new(&self) -> bool {
        matches!(self, Filed::New(_))
    }
}

/// Files `request` as pending, or answers a repeat of a waiting one with that one.
pub fn file<S: Store + ?Sized>(
    store: &S,
    request: Request,
    limits: &Limits,
    now_ms: u64,
) -> Result<Filed, Error> {
    if request.id.is_empty() || request.kind.is_empty() {
        return Err(Error::Malformed);
    }
    if let Some(existing) = store.find_pending(&request, now_ms)? {
        return Ok(Filed::Existing(existing));
    }
    let client = limits.per_client.then_some(request.origin.client.as_str());
    if store.count_pending(client, now_ms)? >= limits.pending_cap {
        return Err(Error::Busy);
    }
    let record = Record::pending(request);
    store.insert(&record)?;
    Ok(Filed::New(record))
}

/// A decision on one pending request.
#[derive(Clone, Copy)]
pub struct Decision<'a> {
    pub approve: bool,
    /// The display digest ([`Request::display_digest`]) of what the decider was shown. `None`
    /// only where the deciding surface shows a fresh read of the record and the app checks the
    /// binding again at use.
    pub shown: Option<&'a str>,
    /// The app's wire tag for the display digest.
    pub domain: &'a str,
    /// Who decided, proven; checked for an approval only (a refusal needs no proof).
    pub proof: Option<&'a dyn Proof>,
    /// How long an approval lives.
    pub grant: Grant,
    /// Reason code recorded with the move (for example `user`).
    pub reason: Option<&'a str>,
}

/// Decides the pending request `id`. `Ok(None)`: no such request, it is not pending, it
/// expired, or another decider moved it first.
pub fn decide<S: Store + ?Sized>(
    store: &S,
    id: &str,
    decision: &Decision<'_>,
    now_ms: u64,
) -> Result<Option<Record>, Error> {
    let Some(mut record) = store.get(id)? else {
        return Ok(None);
    };
    if record.state != State::Pending || !record.live(now_ms) {
        return Ok(None);
    }
    if decision.approve {
        if let Some(proof) = decision.proof {
            proof.verify(&record).map_err(Error::Proof)?;
        }
    }
    if let Some(shown) = decision.shown {
        if record.request.display_digest(decision.domain) != shown {
            return Err(Error::Shown);
        }
    }
    let mut change = if decision.approve {
        Change::to(State::Approved, now_ms).grant(decision.grant)
    } else {
        Change::to(State::Denied, now_ms)
    };
    change.reason = decision.reason.map(str::to_string);
    if !store.transition(id, State::Pending, &change)? {
        return Ok(None);
    }
    change.apply(&mut record);
    Ok(Some(record))
}

/// Uses the approval `id` for the act `presented` describes (its id is ignored), in one move
/// to [`State::Done`]. For an act whose outcome is known only after it runs, use [`claim`] and
/// [`settle`] instead. The record
/// must have been given for the same kind, scope, door, filer and destination
/// ([`Error::Different`]), the same arguments and revision ([`Error::Changed`]) and, with
/// `exact_summary`, the same summary ([`Error::Shown`]). With `proof`, a pending request is
/// approved and used in one move once the proof verifies; without one it is
/// [`Error::Pending`]. Returns the used record (state [`State::Done`]).
pub fn take<S: Store + ?Sized>(
    store: &S,
    id: &str,
    presented: &Request,
    proof: Option<&dyn Proof>,
    exact_summary: bool,
    now_ms: u64,
) -> Result<Record, Error> {
    spend(
        store,
        id,
        presented,
        proof,
        exact_summary,
        now_ms,
        State::Done,
    )
}

/// Claims the approval `id` for the act `presented` describes: every check of [`take`], but
/// the record moves to [`State::Claimed`], not done. Of any number of claimants exactly one
/// wins; the others get [`Error::Used`]. The winner runs the act and then calls [`settle`].
pub fn claim<S: Store + ?Sized>(
    store: &S,
    id: &str,
    presented: &Request,
    proof: Option<&dyn Proof>,
    exact_summary: bool,
    now_ms: u64,
) -> Result<Record, Error> {
    spend(
        store,
        id,
        presented,
        proof,
        exact_summary,
        now_ms,
        State::Claimed,
    )
}

fn spend<S: Store + ?Sized>(
    store: &S,
    id: &str,
    presented: &Request,
    proof: Option<&dyn Proof>,
    exact_summary: bool,
    now_ms: u64,
    to: State,
) -> Result<Record, Error> {
    let Some(mut record) = store.get(id)? else {
        return Err(Error::Unknown);
    };
    if proof.is_none() && matches!(record.state, State::Done | State::Failed | State::Claimed) {
        return Err(Error::Used);
    }
    let asked = &record.request;
    if asked.kind != presented.kind
        || asked.scope != presented.scope
        || asked.origin.door != presented.origin.door
        || asked.origin.filer != presented.origin.filer
        || asked.binding.destination != presented.binding.destination
    {
        return Err(Error::Different);
    }
    if let Some(proof) = proof {
        proof.verify(&record).map_err(Error::Proof)?;
    }
    asked.binding.check(&presented.binding, now_ms)?;
    if exact_summary && asked.summary != presented.summary {
        return Err(Error::Shown);
    }
    let from = match record.state {
        State::Done | State::Failed | State::Claimed => return Err(Error::Used),
        State::Denied => return Err(Error::Denied),
        State::Expired => return Err(Error::Expired),
        State::Pending if proof.is_none() => {
            return Err(if record.live(now_ms) {
                Error::Pending
            } else {
                Error::Expired
            })
        }
        state => state,
    };
    if !record.live(now_ms) {
        return Err(Error::Expired);
    }
    let change = Change::to(to, now_ms);
    if !store.transition(id, from, &change)? {
        return Err(Error::Used);
    }
    change.apply(&mut record);
    Ok(record)
}

/// A live session grant of `kind` for `scope` given to this door and client, if there is one.
/// Finding it counts as a use (an idle grant is refreshed).
pub fn session<S: Store + ?Sized>(
    store: &S,
    kind: &str,
    scope: &str,
    origin: &Origin,
    now_ms: u64,
) -> Result<Option<Record>, Error> {
    let found = store.approved(now_ms)?.into_iter().find(|r| {
        r.request.kind == kind
            && r.request.scope == scope
            && r.request.origin.door == origin.door
            && r.request.origin.client == origin.client
    });
    if let Some(record) = &found {
        store.touch(&record.request.id, now_ms)?;
    }
    Ok(found)
}

/// What a claimed act did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It ran: the record becomes [`State::Done`].
    Done,
    /// It did not happen: [`State::Failed`], reason `failed`.
    Failed,
    /// Whether it happened is not known (a crash after the claim, a timeout, a reconciler that
    /// cannot tell): [`State::Failed`], reason `unknown_outcome`. Never read as either.
    Unknown,
}

fn settlement(outcome: Outcome, result: Option<&str>, now_ms: u64) -> Change {
    let mut change = match outcome {
        Outcome::Done => Change::to(State::Done, now_ms),
        Outcome::Failed => Change::to(State::Failed, now_ms).reason("failed"),
        Outcome::Unknown => Change::to(State::Failed, now_ms).reason("unknown_outcome"),
    };
    change.result = result.map(str::to_string);
    change
}

/// Settles the claim on `id` with `outcome` and, when the act names something, its `result`.
/// [`Error::Unclaimed`] when nothing is claimed under `id` (never claimed, or settled already,
/// by the act or by [`recover`]): of two settlements exactly one wins.
pub fn settle<S: Store + ?Sized>(
    store: &S,
    id: &str,
    outcome: Outcome,
    result: Option<&str>,
    now_ms: u64,
) -> Result<Record, Error> {
    let Some(mut record) = store.get(id)? else {
        return Err(Error::Unknown);
    };
    if record.state != State::Claimed {
        return Err(Error::Unclaimed);
    }
    let change = settlement(outcome, result, now_ms);
    if !store.transition(id, State::Claimed, &change)? {
        return Err(Error::Unclaimed);
    }
    change.apply(&mut record);
    Ok(record)
}

/// Settles every claim made at or before `claimed_before_ms` (the claim's time, the record's
/// `decided_ms`) that nobody settled: at start, every claim an earlier run left; later, every
/// claim older than the act's timeout. `reconcile` answers for each record whether its act
/// happened; an app that cannot ask answers [`Outcome::Unknown`]. Returns the settled records.
pub fn recover<S: Store + ?Sized>(
    store: &S,
    claimed_before_ms: u64,
    now_ms: u64,
    mut reconcile: impl FnMut(&Record) -> Outcome,
) -> Result<Vec<Record>, Error> {
    let mut settled = Vec::new();
    for mut record in store.claimed()? {
        if record.decided_ms > claimed_before_ms {
            continue;
        }
        let change = settlement(reconcile(&record), None, now_ms);
        if store.transition(&record.request.id, State::Claimed, &change)? {
            change.apply(&mut record);
            settled.push(record);
        }
    }
    Ok(settled)
}

/// Withdraws the pending or approved request `id` before it is used: it becomes
/// [`State::Denied`] with `reason` (`revoked` by the decider, `cancelled` by the asker; the
/// app checks who may). `Ok(None)`: no such request, or it is no longer pending or approved
/// (decided, claimed, used, closed), or another move came first. Revoking only reduces trust,
/// so it takes no proof.
pub fn revoke<S: Store + ?Sized>(
    store: &S,
    id: &str,
    reason: &str,
    now_ms: u64,
) -> Result<Option<Record>, Error> {
    let Some(mut record) = store.get(id)? else {
        return Ok(None);
    };
    if !matches!(record.state, State::Pending | State::Approved) {
        return Ok(None);
    }
    let change = Change::to(State::Denied, now_ms).reason(reason);
    if !store.transition(id, record.state, &change)? {
        return Ok(None);
    }
    change.apply(&mut record);
    Ok(Some(record))
}

/// [`revoke`] for every live pending or approved request `select` picks (every grant of one
/// account, of one client). Returns the revoked records.
pub fn revoke_where<S: Store + ?Sized>(
    store: &S,
    select: impl Fn(&Record) -> bool,
    reason: &str,
    now_ms: u64,
) -> Result<Vec<Record>, Error> {
    let mut open = store.pending(now_ms)?;
    open.extend(store.approved(now_ms)?);
    let mut revoked = Vec::new();
    for record in open.iter().filter(|r| select(r)) {
        if let Some(record) = revoke(store, &record.request.id, reason, now_ms)? {
            revoked.push(record);
        }
    }
    Ok(revoked)
}
