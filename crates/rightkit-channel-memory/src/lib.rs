//! rightkit-channel-memory: the channel-memory.v1 schema as Rust types, with
//! validation for PostRight P3 and GenRight G7.
//!
//! The schema lives in `rightkit/contracts/` and is registered in `rightkit-contracts`.
//! This crate embeds its own byte-identical copy at compile time.
//!
//! Field sources (line numbers in the plan files as of 2026-10-08):
//!
//! | Field group | Source |
//! |---|---|
//! | `scope` (venture, channel, account) on every lookup | PostRight P3 line 80 |
//! | `voice`, `visualSystem`, `pillars`, `cadence`, memory fields and shared schema | PostRight P3 line 81 |
//! | `proofs` (exact decimal value, status, structured permitted claims, validity window); `observations` kept apart and never authoritative | PostRight P3 line 82, contract §2 and §3 |
//! | Proof bank is personal and true, never invented | PostRight P3 line 83 |
//! | `references` (style notes: type, palette, caption style, layout, pacing, sound) | PostRight P3 line 85 |
//! | `licenceScope` (monetised channels require commercial-safe assets) | PostRight P4 line 98; GenRight G7 line 83 |
//! | `bannedPhrases` and `bannedMoves` with matchable `phrases` (AI-tell gate and banned-move scoring) | PostRight P4 line 99 |
//! | `cadence.postsPerDay` and `cadence.batch` (five a day, overnight batch) | PostRight P4 line 100 (kids-cartoon template) |
//! | `lessons` written into channel memory from analytics | PostRight P6 line 113 |
//! | `guidance.doList` and `dontList` | Derived from PostRight P4 line 99 (banned moves) and P3 line 81 (voice); the plans name no separate do and don't lists, so this is an added field |
//! | `identityBindings` (characters, product references, voice and pronunciation, brand assets, immutable per revision) | GenRight G7 line 88 |
//! | `modelDefaults` | GenRight G7 line 83 |
//! | `provenance.sources` (seeded from Roadmap files and the hook bank) | PostRight P3 line 81 |
//! | Revisions with `supersedes`; memory revisioned on the daemon as authority | PostRight line 33; a withdrawn receipt must fail, PostRight P3 line 82 |
//! | `conflicts` (two device bodies from one base revision, with resolution state) | Contract §6 A5 |
//!
//! Validation runs in this order: contract name and version, the JSON Schema,
//! the typed model, then the semantic rules (revision ordering, unique ids,
//! date order, claim mirroring, conflict bodies). The consumer helpers enforce
//! scope, proof citation, status transitions, the revision chain and the
//! controlled write path.

#![forbid(unsafe_code)]

mod decimal;
mod epoch;
mod error;
mod model;
mod proof;
mod schema;
mod unit;
mod validate;

pub use decimal::Decimal;
pub use epoch::BriefReentry;
pub use error::{MemoryError, ReasonCode};
pub use model::*;
pub use proof::{
    check_proof_write, check_stale_arrival, check_successor, check_supersession, check_transition,
    check_write, conflicts_with, validate_proof, Citation, Intent, WriteRequest,
};
pub use schema::SCHEMA_SOURCE;
pub use unit::Unit;
pub use validate::parse;
