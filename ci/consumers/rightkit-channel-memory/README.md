# rightkit-channel-memory 0.2.1 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-channel-memory =0.2.1` exactly. The crate has no features, so none are selected; this is the dependency the PostRight engine declares. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed assertion.

The program embeds one `channel-memory.v1` document (revision 2, scope `orthic/legion-notes/main`, four proofs: one approved, two collected, one withdrawn, plus an observation, a lesson and a performance note) and builds every other document it needs from that one in memory.

## What it proves

- The embedded schema (`SCHEMA_SOURCE`) is the `channel-memory:v1` schema.
- `Decimal` compares exact values (`1240.00` equals `1240`, `-0` equals `0.0`), refuses malformed text and a JSON number, and `Decimal::try_parse` reports `E_NUMBER_INVALID`.
- `Unit` accepts currency codes, the built-in codes and open slugs, refuses the reserved `_per_` forms and a 33-character base, and `Unit::try_parse` reports `E_UNIT_INVALID`.
- `ProofStatus` helpers and the wire name of every `ReasonCode`.
- `parse` accepts the document, reads it into the typed model and survives a JSON round trip, and refuses, in stage order: text that is not JSON (`Parse`), another contract (`WrongContract`), a missing, malformed or unreadable version (`MissingVersion`, `InvalidVersion`, `UnsupportedVersion` for major 2 and for minor 1; a patch number never gates), schema defects (`Schema`, including an unknown field, the retired `pending` status, a numeric proof value, a bad unit and a revision 1 that carries `supersedes`), an observation marked authoritative, and semantic defects (`Semantic`: revision chain, duplicate ids, a reversed period, a `validUntil` before `validFrom`, a claim that does not mirror its proof, an unknown observation reference, two approved proofs that disagree with `E_PROOF_CONFLICT_SCOPE`).
- `ensure_scope` refuses another venture, channel or account (`E_FOREIGN_SCOPE`), and `approved_proofs` lists approved proofs only.
- `cite_proof` accepts an exact match (also with the number written without trailing zeros) and returns the first failing reason code in contract order: foreign scope, observation, unknown proof, not approved, withdrawn, superseded, stale revision, not yet valid, expired (the last valid day is inclusive), claim not permitted, number, units, subject and period mismatch, and an open conflict (`has_open_conflict`, `open_conflicts`, `ConflictRecord` helpers).
- `check_transition` (legal moves, frozen approved content, terminal withdrawn and superseded states, the revision step), `check_successor` (revision follows the head, proofs are never removed, a new proof starts collected), `check_stale_arrival` (an older copy cannot restore a withdrawn proof, a different body loses to the head), `check_supersession` and `conflicts_with`.
- `check_write` for each author and intent: a stale base revision (`revision_conflict`), a skill that tries to approve (`E_SKILL_STATUS_FORBIDDEN`), the daemon that tries to approve, an intent that does not match the status change, a skill that changes authoritative memory, and the permitted writes (a skill or the daemon collecting a proof or recording an observation, any author recording a lesson, and the daemon recording a conflict). `WriteRequest` serialises with `author`, `intent` and `baseRevision`, and an observation cannot be proposed as authoritative.
- The record-level entry points `validate_proof` and `check_proof_write`.
- `BannedMove::matches` ignoring case, `review_epoch` and `brief_reentry`.

## What it deliberately does not prove

- Storage, sync and conflict creation, which belong to the daemon, not this crate.
- The fixture files of the source repository (`contracts/fixtures/channel-memory`); the document is embedded instead.
- The optional `modelDefaults` and `identityBindings` fields and non-empty `references`, which the embedded document does not carry.
- The performance of the JSON Schema validators.

State: in-memory strings only. No network, no files, no process spawn, no user data.
