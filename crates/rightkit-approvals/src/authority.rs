//! The authority re-check at execution.
//!
//! An approval is decided at one moment and used at a later one. Between the two the draft can
//! change, the approval can run out, the owner can revoke it, and the key or device that decided it
//! can be revoked or turn out never to have been the owner's. The host calls [`recheck`] (or
//! [`begin`], which also claims) immediately before it runs the effect, and runs the effect only
//! on `Ok`.
//!
//! What it re-verifies, in this order, each refusing on doubt:
//!
//! 1. the record is still approved: not pending, denied or revoked, claimed, used or expired;
//! 2. it was given for this kind, scope, door and filer;
//! 3. the binding is exact: the same arguments digest, destination and revision as the approval
//!    holds. **Any difference voids the approval for good** (it becomes denied, reason
//!    `binding_changed`), so a changed draft cannot be changed back and sent on the old approval;
//! 4. the approval carries a binding of its own (arguments digest and expiry both set), is one
//!    act's grant ([`Grant::Once`]: a session grant approves a family, never one exact effect),
//!    and neither its expiry nor its grant window has passed;
//! 5. the [`Authority`] re-verifies the signature of the decision against the keys enrolled now
//!    (a revoked key fails here) and says who the approver is; and
//! 6. the approver is the owner: an enrolled owner key, or a paired device that is owner-paired
//!    and not a guest and not revoked ([`Approver::is_owner`]).
//!
//! Nothing here reads the clock, a key store or a device registry: the caller passes `now_ms` and
//! its own [`Authority`]. Nothing loosens by setting: there is no flag that skips a step.

use crate::error::{Error, Refusal};
use crate::model::{Grant, Record, Request, State};
use crate::rules;
use crate::store::Store;

/// Who made a decision, as the [`Authority`] resolves it now (not as it was when it decided).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approver {
    /// A key the owner enrolled on their own computer (an approver key or an owner-check key).
    /// Enrolment itself needs the owner's PIN or an already enrolled key, so a key found here is
    /// the owner's.
    OwnerKey { key_id: String, device: String },
    /// A paired device, with what the pairing registry says about it now. The facts come from the
    /// registry, never from the device's own word.
    PairedDevice {
        device_id: String,
        /// The owner admitted this device themselves (the owner check admitted it, or it joined
        /// through an owner-check token). A paired device that is not owner-paired may hold
        /// grants to request things; it never approves.
        owner_paired: bool,
        /// A leased cloud agent. A guest is never an approver, whatever else its record says.
        guest: bool,
        /// Paired and not revoked at this moment.
        active: bool,
    },
}

impl Approver {
    /// True for an owner key, and for a paired device that is active, owner-paired and not a
    /// guest. Every other approver is refused.
    pub fn is_owner(&self) -> bool {
        match self {
            Approver::OwnerKey { .. } => true,
            Approver::PairedDevice {
                owner_paired,
                guest,
                active,
                ..
            } => *active && *owner_paired && !*guest,
        }
    }
}

/// The host's side of the re-check: it holds the evidence of the decision and the key and device
/// registries.
pub trait Authority {
    /// Re-verifies the decision on `record` and says who made it. It must check the decision's
    /// signature (or equivalent proof) over the record's own content against the keys or devices
    /// enrolled **now**, without spending a nonce (the nonce was spent when the decision was
    /// taken), and it must refuse a key or device that is unknown, revoked, development-only or
    /// not proven. A decision made by a call from the app's own window has no proof; an
    /// authority for such an app names its own window as the approver only where that is the
    /// owner's own process. Any doubt is `Err`.
    fn approver(&self, record: &Record) -> Result<Approver, Refusal>;
}

/// A record that passed [`recheck`], and who approved it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rechecked {
    /// The record as read for the check; after [`begin`], as claimed.
    pub record: Record,
    pub approver: Approver,
}

/// Why a decision's approver was refused: not the owner.
fn not_owner() -> Error {
    Error::Proof(Refusal::new(
        "approver_not_owner",
        "the approval was not given by the owner: a paired device approves only if it is owner-paired, \
         and never as a guest",
    ))
}

/// Re-checks the approval `id` for the act `presented` describes (its summary and id are ignored;
/// its kind, scope, origin door and filer, and its binding's arguments, destination and revision
/// are compared). Changes nothing except that a binding mismatch voids the approval. Call it
/// immediately before running the effect; run the effect only on `Ok`. For one move that also
/// claims the approval, use [`begin`].
///
/// Refusals: [`Error::Unknown`], [`Error::Pending`], [`Error::Denied`] (refused or revoked or
/// voided), [`Error::Used`] (claimed, done or failed), [`Error::Expired`], [`Error::Different`]
/// (kind, scope, door or filer), [`Error::Changed`] (arguments, destination or revision; the
/// approval is voided), [`Error::Malformed`] (no arguments digest, no expiry, or not a one-act
/// grant), and [`Error::Proof`] for the authority's own refusal or `approver_not_owner`.
pub fn recheck<S: Store + ?Sized, A: Authority + ?Sized>(
    store: &S,
    authority: &A,
    id: &str,
    presented: &Request,
    now_ms: u64,
) -> Result<Rechecked, Error> {
    let Some(record) = store.get(id)? else {
        return Err(Error::Unknown);
    };
    match record.state {
        State::Approved => {}
        State::Pending => return Err(Error::Pending),
        State::Denied => return Err(Error::Denied),
        State::Done | State::Failed | State::Claimed => return Err(Error::Used),
        State::Expired => return Err(Error::Expired),
    }
    let asked = &record.request;
    if asked.kind != presented.kind
        || asked.scope != presented.scope
        || asked.origin.door != presented.origin.door
        || asked.origin.filer != presented.origin.filer
    {
        return Err(Error::Different);
    }
    let held = &asked.binding;
    if held.args.is_empty() || held.expires_ms == 0 || presented.binding.args.is_empty() {
        return Err(Error::Malformed);
    }
    if held.args != presented.binding.args
        || held.destination != presented.binding.destination
        || held.revision != presented.binding.revision
    {
        // Void it: a draft that changed is a different effect, and changing it back later must not
        // revive the approval. A store that cannot record the voiding is an error, never a pass.
        rules::revoke(store, id, "binding_changed", now_ms)?;
        return Err(Error::Changed);
    }
    if !matches!(record.grant, Some(Grant::Once { .. })) {
        return Err(Error::Malformed);
    }
    if now_ms > held.expires_ms || !record.live(now_ms) {
        return Err(Error::Expired);
    }
    let approver = authority.approver(&record).map_err(Error::Proof)?;
    if !approver.is_owner() {
        return Err(not_owner());
    }
    Ok(Rechecked { record, approver })
}

/// [`recheck`], then [`rules::claim`] in the same call: the approval is claimed by this act (of
/// any number of callers exactly one wins; the rest get [`Error::Used`]) and a revocation that
/// lands between the check and the claim is refused ([`Error::Denied`]). The caller then runs the
/// effect and calls [`rules::settle`] with what happened (`Outcome::Unknown` when it cannot tell;
/// never retry on unknown). The returned record is the claimed one.
pub fn begin<S: Store + ?Sized, A: Authority + ?Sized>(
    store: &S,
    authority: &A,
    id: &str,
    presented: &Request,
    now_ms: u64,
) -> Result<Rechecked, Error> {
    let checked = recheck(store, authority, id, presented, now_ms)?;
    let record = rules::claim(store, id, presented, None, false, now_ms)?;
    Ok(Rechecked {
        record,
        approver: checked.approver,
    })
}

/// Voids the approval `id` because the thing it was given for changed (a draft edited, a target
/// moved), before anything tries to run it. Same effect as the voiding inside [`recheck`]: the
/// approval becomes denied, reason `binding_changed`. `Ok(None)` when it was not pending or
/// approved any more.
pub fn void_changed<S: Store + ?Sized>(
    store: &S,
    id: &str,
    now_ms: u64,
) -> Result<Option<Record>, Error> {
    rules::revoke(store, id, "binding_changed", now_ms)
}
