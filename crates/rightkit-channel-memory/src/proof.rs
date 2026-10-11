//! Proof rules that span a citation, a status change or a write: the citation
//! check of contract §5.3, the status state machine of §3, the revision chain
//! of §6 and the controlled write path of §7.

use serde::{Deserialize, Serialize};

use crate::model::{Author, ChannelMemory, ConflictStatus, DateWindow, Proof, ProofStatus, Scope};
use crate::schema;
use crate::validate::{check_proof_record, parse};
use crate::{Decimal, MemoryError, ReasonCode};

fn refuse<T>(code: ReasonCode, detail: impl Into<String>) -> Result<T, MemoryError> {
    Err(MemoryError::Refused {
        code,
        detail: detail.into(),
    })
}

fn scope_label(scope: &Scope) -> String {
    format!("{}/{}/{}", scope.venture, scope.channel, scope.account)
}

fn status_name(status: ProofStatus) -> &'static str {
    match status {
        ProofStatus::Collected => "collected",
        ProofStatus::Approved => "approved",
        ProofStatus::Withdrawn => "withdrawn",
        ProofStatus::Superseded => "superseded",
    }
}

/// What a claim in copy states, as the gate reads it. Every field is compared
/// exactly; `value` compares as an exact decimal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Citation {
    pub proof_id: String,
    pub proof_revision: u32,
    pub claim_id: String,
    pub value: Decimal,
    pub units: String,
    pub subject: String,
    pub period: DateWindow,
}

impl ChannelMemory {
    /// Refuses a lookup made for another venture, channel or account.
    pub fn ensure_scope(&self, expected: &Scope) -> Result<(), MemoryError> {
        if &self.scope == expected {
            Ok(())
        } else {
            refuse(
                ReasonCode::ForeignScope,
                format!(
                    "memory is {}, lookup asked for {}",
                    scope_label(&self.scope),
                    scope_label(expected)
                ),
            )
        }
    }

    /// Proofs that may be cited as facts: approved ones only.
    pub fn approved_proofs(&self) -> impl Iterator<Item = &Proof> {
        self.proofs
            .iter()
            .filter(|proof| proof.status == ProofStatus::Approved)
    }

    /// Resolves a citation against the bank. Checks run in the order of
    /// contract §5.3 and the first failure returns its reason code. Scope and
    /// the observation check run before the lookup, so a foreign scope or an
    /// observation id never reaches the proof bank (S3 and S-P3-03). `today`
    /// is passed in so validity checks stay deterministic.
    pub fn cite_proof(
        &self,
        scope: &Scope,
        citation: &Citation,
        today: &str,
    ) -> Result<&Proof, MemoryError> {
        self.ensure_scope(scope)?;
        if self
            .observations
            .iter()
            .any(|item| item.id == citation.proof_id)
        {
            return refuse(
                ReasonCode::ObservationAsFact,
                format!("`{}` is an observation, not a proof", citation.proof_id),
            );
        }
        let proof = match self
            .proofs
            .iter()
            .find(|proof| proof.id == citation.proof_id)
        {
            Some(proof) => proof,
            None => {
                return refuse(
                    ReasonCode::UnknownProof,
                    format!("`{}` is not in the proof bank", citation.proof_id),
                )
            }
        };
        let id = proof.id.as_str();
        match proof.status {
            ProofStatus::Approved => {}
            ProofStatus::Collected => {
                return refuse(
                    ReasonCode::ProofNotApproved,
                    format!("`{id}` is collected, not approved"),
                )
            }
            ProofStatus::Withdrawn => {
                return refuse(ReasonCode::ProofWithdrawn, format!("`{id}` was withdrawn"))
            }
            ProofStatus::Superseded => {
                return refuse(
                    ReasonCode::ProofSuperseded,
                    format!("`{id}` was superseded"),
                )
            }
        }
        if citation.proof_revision != proof.revision {
            return refuse(
                ReasonCode::ProofRevisionStale,
                format!(
                    "citation names revision {} of `{id}`, current is {}",
                    citation.proof_revision, proof.revision
                ),
            );
        }
        if today < proof.valid_from.as_str() {
            return refuse(
                ReasonCode::ProofNotYetValid,
                format!("`{id}` is valid from {}", proof.valid_from),
            );
        }
        if let Some(until) = &proof.valid_until {
            if today > until.as_str() {
                return refuse(
                    ReasonCode::ProofExpired,
                    format!("`{id}` was valid until {until}"),
                );
            }
        }
        let claim = match proof
            .permitted_claims
            .iter()
            .find(|claim| claim.claim_id == citation.claim_id)
        {
            Some(claim) => claim,
            None => {
                return refuse(
                    ReasonCode::ClaimNotPermitted,
                    format!("claim `{}` is not permitted for `{id}`", citation.claim_id),
                )
            }
        };
        if citation.value != claim.value {
            return refuse(
                ReasonCode::NumberMismatch,
                format!(
                    "claim states {}, citation states {}",
                    claim.value, citation.value
                ),
            );
        }
        if citation.units != claim.units.as_str() {
            return refuse(
                ReasonCode::UnitMismatch,
                format!(
                    "claim units are `{}`, citation says `{}`",
                    claim.units, citation.units
                ),
            );
        }
        if citation.subject != claim.subject {
            return refuse(
                ReasonCode::SubjectMismatch,
                format!(
                    "claim subject is `{}`, citation says `{}`",
                    claim.subject, citation.subject
                ),
            );
        }
        if citation.period != claim.period {
            return refuse(
                ReasonCode::PeriodMismatch,
                format!(
                    "claim period is {} to {}, citation says {} to {}",
                    claim.period.from, claim.period.to, citation.period.from, citation.period.to
                ),
            );
        }
        if self
            .conflicts
            .iter()
            .any(|item| item.status == ConflictStatus::Open && item.proof_id == proof.id)
        {
            return refuse(
                ReasonCode::ProofConflict,
                format!("`{id}` is in an open conflict"),
            );
        }
        Ok(proof)
    }
}

/// One proof's status move between two revisions (§3). Unchanged proofs pass.
/// Withdrawn and superseded are terminal; approved content is frozen.
pub fn check_transition(from: &Proof, to: &Proof) -> Result<(), MemoryError> {
    if from == to {
        return Ok(());
    }
    let id = to.id.as_str();
    if from.id != to.id {
        return refuse(
            ReasonCode::IllegalTransition,
            format!("`{}` cannot become `{}`", from.id, to.id),
        );
    }
    match from.status {
        ProofStatus::Withdrawn => {
            return refuse(
                ReasonCode::ProofWithdrawn,
                format!("`{id}` is withdrawn and terminal"),
            )
        }
        ProofStatus::Superseded => {
            return refuse(
                ReasonCode::ProofSuperseded,
                format!("`{id}` is superseded and terminal"),
            )
        }
        ProofStatus::Approved if to.status == ProofStatus::Approved => {
            return refuse(
                ReasonCode::IllegalTransition,
                format!("`{id}` is approved; its content is frozen and a correction is a new id"),
            )
        }
        _ => {}
    }
    let legal = matches!(
        (from.status, to.status),
        (ProofStatus::Collected, ProofStatus::Collected)
            | (ProofStatus::Collected, ProofStatus::Approved)
            | (ProofStatus::Collected, ProofStatus::Withdrawn)
            | (ProofStatus::Approved, ProofStatus::Withdrawn)
            | (ProofStatus::Approved, ProofStatus::Superseded)
    );
    if !legal {
        return refuse(
            ReasonCode::IllegalTransition,
            format!(
                "`{id}` cannot move from {} to {}",
                status_name(from.status),
                status_name(to.status)
            ),
        );
    }
    if to.revision != from.revision + 1 || to.supersedes_revision != Some(from.revision) {
        return refuse(
            ReasonCode::IllegalTransition,
            format!(
                "`{id}` revision {} must supersede revision {}",
                to.revision, from.revision
            ),
        );
    }
    Ok(())
}

/// The next revision of one memory over its head (§6 A1, A3, A4). Proofs are
/// never removed, every change is a legal transition, and a new proof starts
/// collected.
pub fn check_successor(head: &ChannelMemory, next: &ChannelMemory) -> Result<(), MemoryError> {
    if head.memory_id != next.memory_id || head.scope != next.scope {
        return Err(MemoryError::Semantic {
            rule: format!(
                "`{}` is not a revision of `{}`",
                next.memory_id, head.memory_id
            ),
        });
    }
    if next.revision != head.revision + 1 {
        return refuse(
            ReasonCode::RevisionConflict,
            format!(
                "next revision {} does not follow current revision {}",
                next.revision, head.revision
            ),
        );
    }
    for proof in &head.proofs {
        match next.proofs.iter().find(|item| item.id == proof.id) {
            Some(after) => check_transition(proof, after)?,
            None => {
                return refuse(
                    ReasonCode::IllegalTransition,
                    format!("`{}` was removed; withdraw it instead", proof.id),
                )
            }
        }
    }
    for proof in &next.proofs {
        let is_new = !head.proofs.iter().any(|item| item.id == proof.id);
        if is_new && (proof.status != ProofStatus::Collected || proof.revision != 1) {
            return refuse(
                ReasonCode::IllegalTransition,
                format!(
                    "new proof `{}` must start collected at revision 1",
                    proof.id
                ),
            );
        }
    }
    Ok(())
}

/// An older revision arriving after the head (§6 A3, A4). It may not restore a
/// withdrawn or superseded proof, and a different body of a proof loses to the
/// head.
pub fn check_stale_arrival(head: &ChannelMemory, stale: &ChannelMemory) -> Result<(), MemoryError> {
    if head.memory_id != stale.memory_id || head.scope != stale.scope {
        return Err(MemoryError::Semantic {
            rule: format!(
                "`{}` is not a revision of `{}`",
                stale.memory_id, head.memory_id
            ),
        });
    }
    if stale.revision >= head.revision {
        return Err(MemoryError::Semantic {
            rule: format!(
                "revision {} is not older than head {}",
                stale.revision, head.revision
            ),
        });
    }
    for old in &stale.proofs {
        let Some(current) = head.proofs.iter().find(|item| item.id == old.id) else {
            continue;
        };
        if current.status == ProofStatus::Withdrawn && old.status != ProofStatus::Withdrawn {
            return refuse(
                ReasonCode::ProofWithdrawn,
                format!(
                    "stale revision {} would restore withdrawn `{}`",
                    stale.revision, old.id
                ),
            );
        }
        if current.status == ProofStatus::Superseded && old.status != ProofStatus::Superseded {
            return refuse(
                ReasonCode::ProofSuperseded,
                format!(
                    "stale revision {} would restore superseded `{}`",
                    stale.revision, old.id
                ),
            );
        }
        if old != current {
            return refuse(
                ReasonCode::RevisionConflict,
                format!(
                    "stale body of `{}` at revision {} loses to revision {}",
                    old.id, old.revision, current.revision
                ),
            );
        }
    }
    Ok(())
}

/// Who proposes a write. Only `adrian` may change a proof's status (§3).
/// Intent names what the write is for and must match its changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    Collect,
    Edit,
    Approve,
    Withdraw,
    Supersede,
    Observe,
    Lesson,
    Conflict,
}

/// A proposed next revision. `base_revision` is the `If-Match` precondition
/// (§6 A2). Observations cannot be proposed as authoritative: `authoritative`
/// is `false` by type, so a `true` fails to deserialise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WriteRequest {
    pub author: Author,
    pub intent: Intent,
    pub base_revision: u32,
    pub proposed: ChannelMemory,
}

/// The parts of a memory a skill or the daemon may not change: everything
/// except observations, lessons, collected proofs and, for the daemon, conflicts.
fn frozen_view(memory: &ChannelMemory, daemon: bool) -> ChannelMemory {
    let mut view = memory.clone();
    view.revision = 0;
    view.supersedes = None;
    view.created_at.clear();
    view.observations.clear();
    view.lessons.clear();
    view.proofs
        .retain(|proof| proof.status != ProofStatus::Collected);
    if daemon {
        view.conflicts.clear();
    }
    view
}

/// The author and intent gate (§7). Shared by `check_write` and
/// `check_proof_write`, so both return the same code for the same write.
fn check_author_intent(author: Author, intent: Intent) -> Result<(), MemoryError> {
    let status_intent = matches!(
        intent,
        Intent::Approve | Intent::Withdraw | Intent::Supersede
    );
    let allowed = match author {
        Author::Adrian => true,
        Author::Skill => matches!(intent, Intent::Collect | Intent::Observe | Intent::Lesson),
        Author::Daemon => matches!(
            intent,
            Intent::Collect | Intent::Observe | Intent::Lesson | Intent::Conflict
        ),
    };
    if allowed {
        return Ok(());
    }
    match author {
        Author::Skill if status_intent => refuse(
            ReasonCode::SkillStatusForbidden,
            format!("a skill may not {intent:?} a proof"),
        ),
        _ => refuse(
            ReasonCode::AuthorForbidden,
            format!("{author:?} may not write intent {intent:?}"),
        ),
    }
}

/// Only Adrian may set a proof to anything but `collected` (§3). `before` is
/// the same proof at the head, or `None` when the record is new.
fn check_status_authority(
    author: Author,
    before: Option<&Proof>,
    after: &Proof,
) -> Result<(), MemoryError> {
    let changed = before.is_none_or(|item| item.status != after.status);
    if author == Author::Adrian || after.status == ProofStatus::Collected || !changed {
        return Ok(());
    }
    match author {
        Author::Skill => refuse(
            ReasonCode::SkillStatusForbidden,
            "a skill may set a proof only to collected",
        ),
        _ => refuse(
            ReasonCode::AuthorForbidden,
            "the daemon may set a proof only to collected",
        ),
    }
}

/// The intent a single-record write implies, from its status move. Only the
/// record-level entry point derives it. `check_write` takes it from the request.
fn implied_intent(before: Option<&Proof>, after: &Proof) -> Intent {
    let changed = before.is_none_or(|item| item.status != after.status);
    match (after.status, changed) {
        (ProofStatus::Collected, _) => Intent::Collect,
        (ProofStatus::Approved, true) => Intent::Approve,
        (ProofStatus::Withdrawn, true) => Intent::Withdraw,
        (ProofStatus::Superseded, true) => Intent::Supersede,
        (_, false) => Intent::Edit,
    }
}

/// Checks that a record may be read back as a proof: the JSON Schema for one
/// proof, then the rules that apply to a record in isolation.
fn check_proof_shape(proof: &Proof) -> Result<(), MemoryError> {
    let value = serde_json::to_value(proof).map_err(|error| MemoryError::Parse {
        message: error.to_string(),
    })?;
    schema::check_proof_schema(&value)?;
    check_proof_record(proof)
}

/// The supersession rule, shared by `check_supersession` and the document path
/// (`validate::check_proof`). `successor` is the proof that the superseded
/// record names, or `None` when the document does not hold it. Returns the
/// reason code and a detail. The record path wraps it in `MemoryError::Refused`;
/// the document path keeps `MemoryError::Semantic` and puts the code first.
pub(crate) fn supersession_rule(
    prev: &Proof,
    successor: Option<&Proof>,
) -> Result<(), (ReasonCode, String)> {
    let id = prev.id.as_str();
    if prev.status != ProofStatus::Superseded {
        return Err((
            ReasonCode::IllegalTransition,
            format!("`{id}` is not superseded"),
        ));
    }
    let Some(named) = prev.superseded_by.as_deref() else {
        return Err((
            ReasonCode::IllegalTransition,
            format!("superseded `{id}` names no successor"),
        ));
    };
    let Some(successor) = successor else {
        return Err((
            ReasonCode::UnknownProof,
            format!("successor `{named}` of `{id}` is not in the proof bank"),
        ));
    };
    if successor.id != named {
        return Err((
            ReasonCode::IllegalTransition,
            format!(
                "`{id}` names successor `{named}`, but the successor given is `{}`",
                successor.id
            ),
        ));
    }
    if successor.id == prev.id {
        return Err((
            ReasonCode::IllegalTransition,
            format!("`{id}` cannot supersede itself"),
        ));
    }
    if successor.status != ProofStatus::Approved {
        return Err((
            ReasonCode::ProofNotApproved,
            format!(
                "successor `{}` is {}, not approved",
                successor.id,
                status_name(successor.status)
            ),
        ));
    }
    Ok(())
}

/// Checks that a superseded record and its successor agree (§3): the record is
/// superseded, it names the successor given, and the successor is a different
/// proof that is approved. Refusals: `E_ILLEGAL_TRANSITION` for a record that
/// is not superseded, names no successor, names another id, or names itself;
/// `E_PROOF_NOT_APPROVED` for a successor that is not approved. The document
/// path (`parse`) runs the same rule with the successor looked up in the
/// document, so it also refuses an absent successor with `E_UNKNOWN_PROOF`.
pub fn check_supersession(prev: &Proof, successor: &Proof) -> Result<(), MemoryError> {
    supersession_rule(prev, Some(successor))
        .map_err(|(code, detail)| MemoryError::Refused { code, detail })
}

/// True exactly when two proofs disagree for the same subject, period and
/// units, which is the rule `parse` applies to approved proofs (contract §3,
/// `E_PROOF_CONFLICT_SCOPE`). Both proofs must be `approved`. The rule compares
/// `subject` (exact text), `period` (both dates, exact text), `units` (exact
/// text) and `value` (exact decimal, so `1240.00` equals `1240`). A pair with
/// different values conflicts.
///
/// Not compared: `validFrom`, `validUntil`, `observedAt`, the source and the
/// claims. A proof never conflicts with itself: the same id is not a pair. A
/// withdrawn, superseded or collected proof never conflicts.
pub fn conflicts_with(a: &Proof, b: &Proof) -> bool {
    a.id != b.id
        && a.status == ProofStatus::Approved
        && b.status == ProofStatus::Approved
        && a.subject == b.subject
        && a.period == b.period
        && a.units == b.units
        && a.value != b.value
}

/// Validates one proof record in isolation, for an app that stores proofs
/// itself. It checks the record against the `proof` definition of the embedded
/// schema, then the record rules that `parse` also applies: date order, unique
/// claim ids and claims that mirror the proof.
///
/// The `scope` argument is shape-checked only: its three ids must match the
/// schema `scope` definition. This function does NOT prove that the proof
/// belongs to that scope, because a `Proof` carries no scope of its own. A
/// foreign scope is refused by `ChannelMemory::ensure_scope` at document level,
/// or by the caller that chose which memory to store the proof in.
///
/// Not covered, because each needs more than one record: a superseded proof's
/// successor must be approved in the same document, observation references
/// must resolve, and approved proofs must agree across the document. Use
/// `parse` for those.
pub fn validate_proof(proof: &Proof, scope: &Scope) -> Result<(), MemoryError> {
    let value = serde_json::to_value(scope).map_err(|error| MemoryError::Parse {
        message: error.to_string(),
    })?;
    schema::check_scope(&value)?;
    check_proof_shape(proof)
}

/// Validates one proposed record write against the record it replaces. `prev`
/// is `None` for a new record. The author and intent come from the status
/// move (`implied_intent`). Checks run in the order of `check_write`: the author
/// gate, status authority, the transition, then the record rules.
///
/// Not covered, because each needs sibling records: the scope of the memory,
/// the base revision (compare and swap), and frozen memory content outside the
/// proof. Use `check_write` for those. The author gate, status authority,
/// transition and record rules use the same functions as `check_write`.
pub fn check_proof_write(
    prev: Option<&Proof>,
    next: &Proof,
    author: Author,
) -> Result<(), MemoryError> {
    let intent = implied_intent(prev, next);
    check_author_intent(author, intent)?;
    check_status_authority(author, prev, next)?;
    match prev {
        Some(before) => check_transition(before, next)?,
        None => {
            if next.status != ProofStatus::Collected
                || next.revision != 1
                || next.supersedes_revision.is_some()
            {
                return refuse(
                    ReasonCode::IllegalTransition,
                    format!("new proof `{}` must start collected at revision 1", next.id),
                );
            }
        }
    }
    check_proof_shape(next)
}

/// Validates one controlled write against the head revision. The checks run in
/// this order, and the first failure returns its reason code: base revision,
/// author and intent, status changes, frozen content, the proposed document,
/// then the transitions.
pub fn check_write(head: &ChannelMemory, request: &WriteRequest) -> Result<(), MemoryError> {
    if request.base_revision != head.revision {
        return refuse(
            ReasonCode::RevisionConflict,
            format!(
                "base revision {} is not current; current revision is {}",
                request.base_revision, head.revision
            ),
        );
    }
    let author = request.author;
    let intent = request.intent;
    check_author_intent(author, intent)?;
    for proof in &request.proposed.proofs {
        let before = head.proofs.iter().find(|item| item.id == proof.id);
        check_status_authority(author, before, proof)?;
    }
    let mut moves: Vec<(ProofStatus, ProofStatus)> = Vec::new();
    for proof in &request.proposed.proofs {
        if let Some(before) = head.proofs.iter().find(|item| item.id == proof.id) {
            if before.status != proof.status {
                moves.push((before.status, proof.status));
            }
        }
    }
    let matches_intent = match intent {
        Intent::Approve => {
            !moves.is_empty() && moves.iter().all(|(_, to)| *to == ProofStatus::Approved)
        }
        Intent::Withdraw => {
            !moves.is_empty() && moves.iter().all(|(_, to)| *to == ProofStatus::Withdrawn)
        }
        Intent::Supersede => {
            moves.iter().any(|(_, to)| *to == ProofStatus::Superseded)
                && moves
                    .iter()
                    .all(|(_, to)| matches!(to, ProofStatus::Superseded | ProofStatus::Approved))
        }
        Intent::Collect | Intent::Edit | Intent::Observe | Intent::Lesson | Intent::Conflict => {
            moves.is_empty()
        }
    };
    if !matches_intent {
        return refuse(
            ReasonCode::IllegalTransition,
            format!("intent {intent:?} does not match the status changes in this write"),
        );
    }
    if author != Author::Adrian
        && frozen_view(head, author == Author::Daemon)
            != frozen_view(&request.proposed, author == Author::Daemon)
    {
        return refuse(
            ReasonCode::AuthorForbidden,
            format!("{author:?} may change only observations, lessons, collected proofs and, for the daemon, conflicts"),
        );
    }
    let text = serde_json::to_string(&request.proposed).map_err(|error| MemoryError::Parse {
        message: error.to_string(),
    })?;
    parse(&text)?;
    check_successor(head, &request.proposed)
}
