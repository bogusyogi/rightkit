# channel-memory contract (v1)

This is the published contract for `channel-memory.v1`. The schema is `schemas/channel-memory.v1.schema.json` in this crate (`SCHEMA_SOURCE` in code). Contract name `channel-memory`, contract version `1.0.0`. A consumer pins the crate version and reads the schema from the crate; it does not read `contracts-pending/`.

The shared repository copy is `contracts-pending/channel-memory/` until the contracts crate lists `channel-memory` (see Registry status). `tests/schema_copy.rs` fails when the two differ.

## What one document is

One immutable revision of a channel's memory for one `scope` (`venture`, `channel`, `account`). A correction is a new revision whose `supersedes` names the revision it replaces. Parsing runs in this order: contract name and version, the JSON Schema, the typed model, then the semantic rules.

Top-level fields: `contract`, `contractVersion`, `memoryId`, `revision`, `supersedes`, `createdAt`, `scope`, `brandCode`, `voice`, `visualSystem`, `pillars`, `cadence`, `guidance`, `bannedMoves`, `bannedPhrases`, `licenceScope`, `proofs`, `conflicts`, `observations`, `lessons`, `performanceNotes`, `references`, optional `modelDefaults` and `identityBindings`, and `provenance`.

## Fact statuses

A proof is the only authoritative fact. Its `status` is one of four values, listed in code as `ProofStatus::ALL`:

| Status | Meaning | Extra field |
|---|---|---|
| `collected` | Gathered from a source, not approved. The only status a skill may write. | none |
| `approved` | Adrian approved it. The only status a claim may cite. | `approval` (`approvedBy` is `adrian`, `approvedAt`) |
| `withdrawn` | Retired by Adrian. Terminal. | `withdrawal` (`withdrawnBy`, `withdrawnAt`, `reason`) |
| `superseded` | Replaced by a later approved proof. Terminal. | `supersededBy` (id of an approved proof) |

`pending` was the earlier name for `collected` and is refused.

Allowed transitions: collected to collected, collected to approved, collected to withdrawn, approved to withdrawn, approved to superseded. Approved content is frozen. Each transition advances `revision` by one. A withdrawn proof is never re-approved under the same id; a correction takes a new id. A withdrawn or superseded proof carries no permitted claims. Checks: `check_transition`, `check_successor`, `check_stale_arrival`, `check_supersession`.

## Conflict record

A conflict keeps two edits of one proof made on two devices from the same base revision. Fields: `id`, `proofId`, `baseRevision`, `bodies`, `status`, and `chosenDevice`.

- `bodies` holds exactly two entries `{device, proof}`, from two different devices. Each proof is revision `baseRevision + 1`, supersedes `baseRevision`, and carries the conflict's `proofId`. The two proofs differ.
- `status` is `open` or `resolved`. An open conflict has no `chosenDevice`. A resolved conflict names one of its two devices.
- While a conflict on a proof is open, every citation of that proof fails with `E_PROOF_CONFLICT`.
- Two approved proofs for the same subject, period and units with different values are a separate defect, `E_PROOF_CONFLICT_SCOPE`, found by `conflicts_with`.

Conflicts are created by the daemon. This crate validates the record and offers `ConflictRecord::is_open`, `chosen_body`, `body_for`, and `ChannelMemory::open_conflicts` and `has_open_conflict`.

## Citation

`ChannelMemory::cite_proof(scope, citation, today)` checks, in order: scope, observation, existence, status, revision, validity window, claim, value, units, subject, period, open conflict. The first failure is returned as a `ReasonCode` (`as_str` gives the `E_` name). Values compare as exact decimals; units, subject and period compare exactly.

## Writes

`check_write` and `check_proof_write` gate a change by author (`adrian`, `skill`, `daemon`), intent and base revision. A skill may set a proof only to `collected`. Observations are never authoritative; `authoritative` is fixed to `false`.

## Error codes

Codes come from `ReasonCode`. Codes beyond the PostRight contract section 5.4 are `E_AUTHOR_FORBIDDEN`, `E_ILLEGAL_TRANSITION`, `E_NUMBER_INVALID` and `E_UNIT_INVALID`. `revision_conflict` is from daemon-api v1.

## Registry status

- The crate is published on crates.io as `rightkit-channel-memory`. Pin the version listed in `CHANGELOG.md` that adds what you need.
- The contracts crate (`rightkit-contracts`) does not yet list `channel-memory` among its contracts. Moving the schema from `contracts-pending/` into `contracts/` and registering it there is that crate's change and is tracked outside this crate.

## Versioning

The contract version stays `1.0.0` under the `v1` file name and `$id` while no consumer outside this repository depends on a different shape. A breaking schema change bumps the contract to `2.0.0` with a `channel-memory.v2` file, and the crate takes a breaking version bump. Additive Rust API changes bump the crate patch version.
