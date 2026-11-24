//! The duty lease and its epoch (no feature needed; the record is meant to be written through a sealing backend).
//!
//! One small control record, `duty`, in its own zone ([`ZONE`]) of the owner's store, sealed like every other record. It says who
//! holds duty (the computer where a noticing loop reads the owner's mail and routes alerts), under which **epoch**, how high that
//! computer ranks, and when it last renewed. The rules:
//!
//! * The holder renews every [`RENEW_MS`]. A computer that sees a holder silent for more than [`TAKEOVER_MS`] (three missed
//!   renewals) claims duty at `epoch + 1`.
//! * The always-on computer outranks an open laptop: when its rank is higher than the holder's it claims at once, so it takes duty
//!   back the moment it returns, and the laptop stops at its next renewal.
//! * A claim is not trusted until it is read back after a short wait: the claimant holds only if the record then names it at that
//!   epoch. The store has no atomic compare-and-swap, so two claimants inside one write window can both believe for a moment; the
//!   re-read settles it, and the epoch is re-checked immediately before an effect, which narrows the window to one round trip. It
//!   does not close it. A host whose effects are idempotent (a mail id dedupes, a send waits for the owner's approval of that exact
//!   message) is safe through the window.
//! * Age is read from the store's modification time of the record (`modified_ms`), not from the writer's clock; the local clock
//!   is used only to compare it, so skew between computers shows up as a slower or faster takeover, within the 90 s margin.
//!
//! The record names devices and numbers only.

use crate::{field, Backend, FieldValue, RawRecord, SavePolicy, SyncError};
use std::time::Duration;

pub const RECORD: &str = "duty";
pub const RECORD_TYPE: &str = "RkDuty";
/// The zone of the control record.
pub const ZONE: &str = "rk-duty";
pub const RENEW_MS: i64 = 30_000;
pub const TAKEOVER_MS: i64 = 90_000;
const RECHECK: Duration = Duration::from_secs(5);

/// Where this computer stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// This computer holds duty at `epoch`.
    Holding { epoch: i64 },
    /// Another computer holds it.
    Other { holder: String, epoch: i64 },
}

struct Held {
    holder: String,
    epoch: i64,
    rank: i64,
    renewed_ms: i64,
}

fn number(record: &RawRecord, key: &str) -> Option<i64> {
    match record.get(key)? {
        FieldValue::I64(n) => Some(*n),
        _ => None,
    }
}

fn parse(record: &RawRecord) -> Option<Held> {
    Some(Held {
        holder: record.text("holder")?,
        epoch: number(record, "epoch")?,
        rank: number(record, "rank").unwrap_or(0),
        renewed_ms: record
            .modified_ms
            .or_else(|| record.date_ms(field::UPDATED_AT))?,
    })
}

fn build(device: &str, rank: i64, epoch: i64, now_ms: i64, tag: Option<String>) -> RawRecord {
    let mut record = RawRecord {
        record_type: RECORD_TYPE.to_string(),
        record_name: RECORD.to_string(),
        fields: Default::default(),
        modified_ms: None,
        created_ms: None,
        change_tag: tag,
    };
    record.set(field::ID, FieldValue::Text(RECORD.to_string()));
    record.set(field::CREATED_AT, FieldValue::Date(now_ms));
    record.set(field::UPDATED_AT, FieldValue::Date(now_ms));
    record.set("holder", FieldValue::Text(device.to_string()));
    record.set("epoch", FieldValue::I64(epoch));
    record.set("rank", FieldValue::I64(rank));
    record
}

/// Writes the record only if it is still the version `tag` names (no tag: it must not exist). `false` when someone else wrote first.
fn write(backend: &dyn Backend, record: RawRecord) -> Result<bool, String> {
    let outcomes = backend
        .save_records(&[record], SavePolicy::IfServerUnchanged)
        .map_err(|e| e.to_string())?;
    match outcomes.into_iter().next().map(|o| o.result) {
        Some(Ok(())) => Ok(true),
        Some(Err(SyncError::Conflict)) => Ok(false),
        Some(Err(other)) => Err(other.to_string()),
        None => Err("the store answered nothing".to_string()),
    }
}

fn read(backend: &dyn Backend) -> Result<Option<RawRecord>, String> {
    backend.fetch_record(RECORD).map_err(|e| e.to_string())
}

fn standing(device: &str, record: Option<&RawRecord>) -> Option<State> {
    let held = parse(record?)?;
    Some(if held.holder == device {
        State::Holding { epoch: held.epoch }
    } else {
        State::Other {
            holder: held.holder,
            epoch: held.epoch,
        }
    })
}

/// A claim at `epoch`, read back after a short wait.
fn claim(
    backend: &dyn Backend,
    device: &str,
    rank: i64,
    epoch: i64,
    over: Option<&RawRecord>,
    now_ms: i64,
) -> Result<State, String> {
    let tag = over.and_then(|r| r.change_tag.clone());
    let won = write(backend, build(device, rank, epoch, now_ms, tag))?;
    if won {
        std::thread::sleep(RECHECK);
    }
    let after = read(backend)?;
    Ok(standing(device, after.as_ref()).unwrap_or(State::Other {
        holder: String::new(),
        epoch,
    }))
}

/// One look at the lease, acting on it: renews it when held, claims it when it lapsed or this computer outranks the holder.
/// `rank` is 1 for the always-on computer and 0 for an open laptop.
pub fn tick(backend: &dyn Backend, device: &str, rank: i64, now_ms: i64) -> Result<State, String> {
    backend.prepare().map_err(|e| e.to_string())?;
    let current = read(backend)?;
    let Some(held) = current.as_ref().and_then(parse) else {
        return claim(backend, device, rank, 1, current.as_ref(), now_ms);
    };
    if held.holder == device {
        if now_ms - held.renewed_ms >= RENEW_MS {
            let renewed = build(
                device,
                rank,
                held.epoch,
                now_ms,
                current.as_ref().and_then(|r| r.change_tag.clone()),
            );
            if !write(backend, renewed)? {
                // Someone wrote between the read and the write: look again next tick.
                return standing(device, read(backend)?.as_ref())
                    .ok_or_else(|| "the lease record is unreadable".to_string());
            }
        }
        return Ok(State::Holding { epoch: held.epoch });
    }
    let lapsed = now_ms - held.renewed_ms > TAKEOVER_MS;
    if lapsed || rank > held.rank {
        return claim(
            backend,
            device,
            rank,
            held.epoch + 1,
            current.as_ref(),
            now_ms,
        );
    }
    Ok(State::Other {
        holder: held.holder,
        epoch: held.epoch,
    })
}
