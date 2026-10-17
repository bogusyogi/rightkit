# Changelog

Publishable (wave 2.8): `publish = false` is removed; the schema is copied into the crate (`schemas/`, checked by `tests/schema_copy.rs`) and embedded from there; the manifest gains `readme`, `keywords`, `categories` and `include`. The schema is pending in
`contracts-pending/channel-memory/` and its contract version stays `1.0.0`
under the `v1` file name and `$id`. Nothing outside this repository reads it,
so the revisions below are made in place; they are recorded here so that the
contract's change history is visible. If the owner wants a published contract
to start from this state, bump the major (`2.0.0`, file `channel-memory.v2`)
at that point.

## 0.2.0 (breaking: minor bump of 0.1.0, which is on crates.io)

The breaking schema and model changes below (#329 structured `PermittedClaim`, #330 `validFrom` and `validUntil`, #331 exact `Decimal`, plus the status rename, `period`, units and conflicts) were first recorded under 0.1.0 in place. They are released as 0.2.0. The crate keeps its own `Scope` type and does not depend on rightkit-spend.

### Published contract (#332)

- `docs/CONTRACT.md` documents `channel-memory.v1` (fields, the four fact statuses, the conflict record, citation order, writes, error codes, versioning and registry status). It ships in the crate (`include` gains `docs/**`) and is linked from the README.
- The crate is on crates.io; this release is the version PostRight pins. The schema is still copied from `contracts-pending/channel-memory/`. Listing `channel-memory` in `rightkit-contracts` and moving the schema to `contracts/` is a change in that crate and is not made here.
- Tests: `tests/contract_doc.rs` fails when the doc stops naming a fact status, a conflict status or a top-level schema field.

### Conflict record API (#327)

- The conflict record type (`ConflictRecord`, `ConflictBody`, `ConflictStatus`, schema `conflicts`) has been in the schema and typed model since 0.1.0; PostRight's contract still listed it as a gap. This change adds the accessors a consumer needs. Additive, no schema change.
- `ConflictStatus::as_str()`, `ConflictRecord::is_open()`, `ConflictRecord::chosen_body()` (the body of the chosen device of a resolved conflict) and `ConflictRecord::body_for(device)`.
- `ChannelMemory::open_conflicts(proof_id)` and `ChannelMemory::has_open_conflict(proof_id)`. `cite_proof` uses the same check, so behaviour is unchanged (`E_PROOF_CONFLICT`).
- Tests: `tests/conflict_record.rs` covers the open and resolved states, both bodies by device, the chosen-device rules, the two-body and two-device rules, and a serde round trip.

### Fact statuses as one list (#333)

- The four fact statuses `collected`, `approved`, `withdrawn` and `superseded` have been in the schema and `ProofStatus` since 0.1.0 (`pending` was renamed `collected`). This change makes them a checkable surface for PostRight's pin. Additive, no schema change.
- `ProofStatus::ALL` (lifecycle order), `ProofStatus::as_str()` (the wire name), `ProofStatus::from_wire(&str)` (`None` for `pending` or any other text) and `ProofStatus::is_terminal()` (withdrawn and superseded).
- Tests: `tests/fact_statuses.rs` pins the schema enum, the serde names, the refusal of `pending`, and the terminal set.

### Review epoch and brief re-entry (#314)

- Additive, no schema change. `ChannelMemory::review_epoch(proof_ids)` returns the epoch of the facts a brief cites (`id@revision:status`, sorted by id, `;`-joined, `id@missing` for an absent proof). `ChannelMemory::brief_reentry(proof_ids, reviewed_epoch)` returns `BriefReentry::Current` or `BriefReentry::NeedsReview { changed, epoch }`, so a brief re-enters review when a cited fact is revised, withdrawn, superseded or removed. A brief stores the epoch when it passes review.

### Proof semantics revision (contract p3-proof-semantics §2, §3, §5.3, §6, §9, §10)

#### Breaking changes to the schema and typed model

- Proof `value` is an exact decimal string matching `^-?(0|[1-9][0-9]*)(\.[0-9]+)?$`.
  The Rust type is `Decimal`: it keeps the text, compares exactly and never
  uses a float. `f64` and the `1e-9` tolerance are removed.
- Proof `status` moves to the top level and takes `collected`, `approved`,
  `withdrawn` or `superseded`. `pending` is renamed `collected`. The former
  `approval.status` object is removed.
- `approval` (`approvedBy` const `adrian`, `approvedAt`) is present only when
  approved. `withdrawal` (`withdrawnBy` const `adrian`, `withdrawnAt`, `reason`)
  is present only when withdrawn. `supersededBy` is present only when
  superseded and must name an approved proof in the same revision.
- `observedFrom` and `observedTo` are replaced by `period` (the inclusive span
  the value describes) and `observedAt` (when the source showed the value).
- New `validFrom` (date) and `validUntil` (date or null): the window in which a
  claim may state the value. Separate from `period`.
- `subject` is a normalised id `subject.<slug>`. `units` is an ISO 4217 code or
  one of `percent`, `count`, `days`, `seconds`. `origin` is removed; the source
  kind now covers it.
- Source kind `connected_account` is renamed `connected_account_export`, matching
  §2.1. Source kinds for proofs are `interview`, `connected_account_export`,
  `roadmap_file`, `adrian_correction`.
- New `supersedesRevision` (integer or null) and `observationRefs` on every
  proof, and `provenance` `{authoredBy, sourceRecordIds}` with `authoredBy` one
  of `adrian`, `skill`, `daemon`.
- `permittedClaims` is a list of structured claim objects `{claimId, text,
  value, units, subject, period}` instead of free strings. Each claim must
  mirror its proof's `value`, `units`, `subject` and `period`. Withdrawn and
  superseded proofs carry no claims.
- `bannedMoves[].phrases` is a new required list of matchable strings.
- New top-level `conflicts` list. Each conflict holds exactly two bodies from
  two devices, both at revision base + 1, and a `status` of `open` or
  `resolved` (with `chosenDevice`).

#### New rules

- Status transitions (`check_transition`): collected to collected, collected to
  approved, collected to withdrawn, approved to withdrawn, approved to
  superseded. Withdrawn and superseded are terminal. Approved content is frozen.
  Each transition advances the proof revision by one.
- Revision chain (`check_successor`, `check_stale_arrival`): a successor
  follows the head revision, proofs are never removed, and a new proof starts
  collected. An older revision cannot restore a withdrawn or superseded proof.
- Citation (`ChannelMemory::cite_proof`, now taking a scope, a `Citation` and
  `today`): checks scope, observation id, existence, status, revision, validity
  window, claim, value, units, subject, period and open conflicts, returning a
  `ReasonCode` for each failure.
- Document rules: claims mirror their proof, claim ids are unique within a
  proof, observation references exist, superseded proofs name an approved
  successor, two approved proofs for one subject, period and units must agree
  (`E_PROOF_CONFLICT_SCOPE`), validity windows and periods run forwards.
- Controlled writes (`check_write`, `WriteRequest`): author (`adrian`, `skill`,
  `daemon`), intent, and `baseRevision` (compare and swap, `revision_conflict`).
  A skill may set a proof only to collected and may not approve, withdraw or
  supersede (`E_SKILL_STATUS_FORBIDDEN`). A skill or the daemon may change only
  observations, lessons, collected proofs and, for the daemon, conflicts
  (`E_AUTHOR_FORBIDDEN`). Observations with `authoritative: true` fail to
  deserialise.
- `BannedMove::matches` checks text against `phrases`, ignoring case.

#### Errors

- `MemoryError::Refused { code, detail }` replaces `UnknownProof`,
  `ProofNotApproved`, `ProofMismatch` and `ForeignScope`. `ReasonCode` holds the
  codes; its `as_str` gives the `E_` names from §5.4 and §7.
- Codes added beyond §5.4: `E_AUTHOR_FORBIDDEN` and `E_ILLEGAL_TRANSITION`.
  They are not in the contract. `revision_conflict` is from daemon-api v1 §6 A2.
  `E_NUMBER_INVALID` and `E_UNIT_INVALID` were added in the typed-construction
  change; they are not in the contract either (see that section below).

#### Record-level entry points (0.2.0)

- `validate_proof(&Proof, &Scope) -> Result<(), MemoryError>` checks one proof
  record in isolation. It checks the scope ids against the schema `scope`
  definition, the proof against the schema `proof` definition, then the record
  rules that `parse` also applies: period and validity order, unique claim ids,
  and claims that mirror their proof. It shape-checks the scope argument only
  and does NOT prove the proof belongs to that scope: a `Proof` carries no scope
  of its own (the memory does). A foreign scope is refused by
  `ChannelMemory::ensure_scope` at document level, or by the caller.
- `check_proof_write(prev: Option<&Proof>, next: &Proof, author: Author) ->
  Result<(), MemoryError>` checks one proposed status change. The intent is
  implied by the status move: a new record or an unchanged collected record is
  `collect`, a move to approved, withdrawn or superseded is that intent, and an
  unchanged approved, withdrawn or superseded record is `edit`. The author gate,
  status authority, transition rules and record rules are the functions
  `check_write` uses, so the two paths return the same reason code for the same
  defect.
- Not covered by either function, because each needs sibling records: a
  superseded proof's successor must be approved in the same document,
  observation references must resolve, approved proofs must agree across the
  document (`E_PROOF_CONFLICT_SCOPE`), the memory scope and base revision (use
  `ChannelMemory::ensure_scope` and `check_write`), and frozen memory content
  outside the proof.
- Internal refactor: the author and intent gate, the status authority rule and
  the record rules are shared helpers. `check_write` and `parse` call them.
  Behaviour is unchanged; the existing tests stay green.

#### Units

- Proof `units` and permitted-claim `units` are the `Unit` type. A unit is an
  ISO 4217 currency code (three uppercase letters), one of `percent`, `count`,
  `days`, `seconds`, or an open slug matching
  `^[a-z][a-z0-9_]{0,31}(_per_(day|week|month|year))?$`.
- The schema `units` pattern is
  `^([A-Z]{3}|percent|count|days|seconds|[a-z][a-z0-9_]{0,31}(_per_(day|week|month|year))?)$`.
- The typed `Unit` and the schema accept exactly the same language. Two tokens
  are reserved: `_per_` and a trailing `_per`. A slug with neither is a plain
  base (1 to 32 characters). Any other slug must end in `_per_<period>`
  (`day`, `week`, `month` or `year`), and the part before that suffix must be
  a plain base of 1 to 32 characters. A suffixed slug can therefore be up to 42
  characters long.
- Accepted: `views`, `viewers_percent`, `upper_percentile`, `per_capita_score`,
  `super_fans`, `shorts_per_week`, `orders_per_month`, `views_per_day`.
  Refused: `x_per_hour` (not a period), `a_per_week_per_day`, `a_per_per_day`,
  `x_per`, `views_per_`, `views_per`, and the earlier invalid cases.
- Schema construct. The regex crate behind the schema validator has no
  lookahead, so the reservation is written as `anyOf` over two `allOf` clauses,
  combined with the slug pattern
  `^[a-z][a-z0-9_]{0,31}(_per_(day|week|month|year))?$`:
  - `allOf(not _per_, not _per$)`: a plain base;
  - `allOf(_per_(day|week|month|year)$, not _per_.*_per_, not _per_per_)`:
    a suffixed slug whose base is plain.
  `_per_per_` is a separate clause because `_per_.*_per_` misses the
  overlapping case `a_per_per_day`.
- The schema and the Rust `Unit` were compared on 1,511,111 generated strings
  (every string up to length six over ten characters, plus random strings built
  from reserved and near-reserved tokens). Mismatches: 0.
- Citation unit checks stay exact string equality.
- Fixtures: valid `unit-views`, `unit-subscribers`, `unit-episodes`,
  `unit-shorts-per-week`, `unit-orders-per-month`; invalid `unit-uppercase-views`,
  `unit-space-in-slug`, `unit-leading-digit`, `unit-empty`,
  `unit-base-33-characters`, `unit-x-per-hour`, `unit-two-rates`,
  `unit-trailing-per-underscore`, `unit-trailing-per`, `unit-a-per-per-day`,
  `unit-x-per`. Valid `unit-per-capita-score`, `unit-super-fans`,
  `unit-upper-percentile` and `unit-viewers-percent`.

#### Typed construction and supersession (0.2.0)

- `Decimal::try_parse(&str) -> Result<Decimal, MemoryError>` and
  `Unit::try_parse(&str) -> Result<Unit, MemoryError>`. Both use the same
  acceptance as serde (`Decimal::parse`, `Unit::parse`), and refuse with
  `MemoryError::Refused`. Serde stays strict and unchanged in behaviour.
- New reason codes `E_NUMBER_INVALID` (`ReasonCode::NumberInvalid`) and
  `E_UNIT_INVALID` (`ReasonCode::UnitInvalid`). They are not in contract §5.4.
  The existing `E_NUMBER_MISMATCH` and `E_UNIT_MISMATCH` mean a citation
  disagrees with a recorded proof, so they do not fit a malformed value.
- `check_supersession(prev: &Proof, successor: &Proof) -> Result<(), MemoryError>`.
  Refusals: `E_ILLEGAL_TRANSITION` when `prev` is not superseded, names no
  successor, names a different id, or names itself; `E_PROOF_NOT_APPROVED` when
  the successor is not approved (any status other than approved, including
  withdrawn). The document path runs the same rule (`supersession_rule`) with
  the successor looked up in the document; an absent successor is
  `E_UNKNOWN_PROOF`. The document path keeps `MemoryError::Semantic` for these
  refusals (the existing fixture test depends on it) and writes the code first
  in the rule text, as for `E_DUPLICATE_CLAIM`.
- Tests: `tests/typed_construction.rs` drives serde and the typed path from one
  table of decimals and units. `tests/record_rules.rs` checks the supersession
  codes and that both paths return the same code for an unapproved successor.

#### Approved-scope conflicts (0.2.0)

- `conflicts_with(a: &Proof, b: &Proof) -> bool` is the public form of the
  document rule that refuses two approved proofs for the same subject, period
  and units with different values. Both must be `approved`. It compares
  `subject` and `units` as exact text, `period` as both dates exactly, and
  `value` as an exact decimal (`1240.00` equals `1240`). It does not compare
  `validFrom`, `validUntil`, `observedAt`, the source or the claims. A proof
  never conflicts with itself, and a withdrawn, superseded or collected proof
  never conflicts.
- `ReasonCode::ProofConflictScope` (`E_PROOF_CONFLICT_SCOPE`). The code is in
  contract §5.4, and the document path already emitted the same literal. The
  document path now calls `conflicts_with` and writes the code from the enum,
  so the text is unchanged.
- Tests: `tests/conflicts.rs` checks one table of pairs (conflicting, same
  value written differently, different subject, period, units, one withdrawn,
  one superseded, one collected) and asserts that `conflicts_with` and the
  document path agree on each row. It also checks that a proof does not
  conflict with itself.

#### Not in this crate

Text-level steps of §5.3 (step 12, number grammar such as `1,240` and `1.24k`)
belong to the PostRight claim-review gate. Parent-scope and daemon storage stay
with the daemon.
