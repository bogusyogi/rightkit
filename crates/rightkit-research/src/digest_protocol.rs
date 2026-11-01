//! Payload shapes for research digests (channel store schema v16). A digest is a dated list of findings the
//! caller supplies; nothing here fetches a source. `digest.apply` turns each entry that has not been applied
//! yet into one draft calendar entry (with its brief), and reports which entry caused which calendar entry.
//! Times are UTC instants `YYYY-MM-DDTHH:MM:SSZ`; dates are `YYYY-MM-DD`; a slot's wall time is entered
//! with its IANA zone name, as calendar.put takes it.
use serde::{Deserialize, Serialize};

pub use rightkit_social_publish::protocol::Scope;

/// One digest entry as the caller supplies it. `source` is required (a url or `path:line`); it is an
/// `Option` here so a missing source is reported as `bad_input` naming the entry, not as a parse error.
#[derive(Debug, Clone, Deserialize)]
pub struct DigestEntryInput {
    pub entry_id: String,
    pub topic: String,
    pub finding: String,
    #[serde(default)]
    pub source: Option<String>,
    /// `YYYY-MM-DD`, the day the finding was read.
    pub fetched_on: String,
    /// The platform the finding suggests (for example `instagram`). Free text, stored as given.
    pub platform: String,
    /// The format the finding suggests (for example `reel`). Free text, stored as given.
    pub format: String,
}

/// One stored digest entry. `source` is a url or a `path:line` reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestEntry {
    pub entry_id: String,
    pub topic: String,
    pub finding: String,
    pub source: String,
    pub fetched_on: String,
    pub platform: String,
    pub format: String,
}

/// One posting slot: a wall time in a zone. `fold` (`earlier` or `later`) is needed only for an ambiguous time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestSlot {
    pub timezone: String,
    pub local_time: String,
    #[serde(default)]
    pub fold: Option<String>,
}

/// `digest.put` input. Stores the digest once: the same id with the same content is a no-op, and the same
/// id with different content is `digest_mismatch`. `now` sets `created_at`.
#[derive(Debug, Clone, Deserialize)]
pub struct DigestPutInput {
    pub scope: Scope,
    pub digest_id: String,
    pub digest_date: String,
    pub entries: Vec<DigestEntryInput>,
    pub now: String,
}

/// `digest.put` output. `unchanged` is true when the same digest was already stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestPutOutput {
    pub scope: Scope,
    pub digest_id: String,
    pub digest_date: String,
    pub entry_count: i64,
    pub unchanged: bool,
}

/// `digest.get` input.
#[derive(Debug, Clone, Deserialize)]
pub struct DigestGetInput {
    pub scope: Scope,
    pub digest_id: String,
}

/// One stored digest, as `digest.get` and `digest.list` return it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestRecord {
    pub scope: Scope,
    pub digest_id: String,
    pub digest_date: String,
    pub entries: Vec<DigestEntry>,
    pub created_at: String,
}

/// `digest.list` input.
#[derive(Debug, Clone, Deserialize)]
pub struct DigestListInput {
    pub scope: Scope,
}

/// `digest.list` output, ordered by digest date then digest id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestListOutput {
    pub scope: Scope,
    pub digests: Vec<DigestRecord>,
}

/// `digest.plan` input. Writes nothing. `week_start` is `YYYY-MM-DD`; every slot must fall on a day in the
/// seven days from it. Entries take the free slots in order.
#[derive(Debug, Clone, Deserialize)]
pub struct DigestPlanInput {
    pub scope: Scope,
    pub digest_id: String,
    pub week_start: String,
    pub slots: Vec<DigestSlot>,
}

/// `digest.apply` input: the same fields as `digest.plan`, and `now` for the brief and calendar writes.
#[derive(Debug, Clone, Deserialize)]
pub struct DigestApplyInput {
    pub scope: Scope,
    pub digest_id: String,
    pub week_start: String,
    pub slots: Vec<DigestSlot>,
    pub now: String,
}

/// One digest entry that `digest.apply` would add, with the calendar entry it would create.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestPlanItem {
    pub digest_entry_id: String,
    pub brief_id: String,
    pub entry_id: String,
    /// The index in the request's `slots` that this item takes.
    pub slot_index: i64,
    /// The UTC instant of the slot.
    pub scheduled_at: String,
    pub timezone: String,
    pub local_time: String,
    pub fold: Option<String>,
    pub entry: DigestEntry,
}

/// One applied digest entry: the calendar entry it produced. `diff` and `already_applied` both use this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestApplication {
    pub digest_id: String,
    pub digest_entry_id: String,
    pub brief_id: String,
    pub entry_id: String,
    pub applied_at: String,
}

/// `digest.plan` output. `unplaced` lists the digest entry ids that had no free slot, in digest order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestPlanOutput {
    pub scope: Scope,
    pub digest_id: String,
    pub week_start: String,
    pub items: Vec<DigestPlanItem>,
    pub unplaced: Vec<String>,
    pub already_applied: Vec<DigestApplication>,
}

/// `digest.apply` output. `diff` is what this call added (empty on a replay). `already_applied` is every
/// application of the digest after the call. `unplaced` is as in the plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestApplyOutput {
    pub scope: Scope,
    pub digest_id: String,
    pub week_start: String,
    pub diff: Vec<DigestApplication>,
    pub already_applied: Vec<DigestApplication>,
    pub unplaced: Vec<String>,
}

/// `digest.applied` input.
#[derive(Debug, Clone, Deserialize)]
pub struct DigestAppliedInput {
    pub scope: Scope,
    pub digest_id: String,
}

/// `digest.applied` output: which digest entries produced which calendar entries, ordered by apply time then
/// digest entry id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestAppliedOutput {
    pub scope: Scope,
    pub digest_id: String,
    pub applications: Vec<DigestApplication>,
}

/// `digest.release` input: frees the slot a digest entry reserved, so a later plan may use it. Does not touch the
/// calendar entry.
#[derive(Debug, Clone, Deserialize)]
pub struct DigestReleaseInput {
    pub scope: Scope,
    pub digest_id: String,
    pub digest_entry_id: String,
}

/// `digest.release` output. `released` is false when the entry held no reservation (already released, or never applied).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestReleaseOutput {
    pub scope: Scope,
    pub digest_id: String,
    pub digest_entry_id: String,
    pub released: bool,
}

/// `digest.reserved` input. Without `digest_id`, every reservation in scope is listed.
#[derive(Debug, Clone, Deserialize)]
pub struct DigestReservedInput {
    pub scope: Scope,
    #[serde(default)]
    pub digest_id: Option<String>,
}

/// One reserved slot: the UTC instant `digest.apply` took for a digest entry. A plan skips a reserved instant even
/// after the entry moved off it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestSlotReservation {
    pub digest_id: String,
    pub digest_entry_id: String,
    pub entry_id: String,
    pub scheduled_at: String,
    pub reserved_at: String,
}

/// `digest.reserved` output, ordered by instant then digest entry id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestReservedOutput {
    pub scope: Scope,
    pub reservations: Vec<DigestSlotReservation>,
}
