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

/// Uses the approval `id` for the act `presented` describes (its id is ignored). The record
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
    let Some(mut record) = store.get(id)? else {
        return Err(Error::Unknown);
    };
    if proof.is_none() && matches!(record.state, State::Done | State::Failed) {
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
        State::Done | State::Failed => return Err(Error::Used),
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
    let change = Change::to(State::Done, now_ms);
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
