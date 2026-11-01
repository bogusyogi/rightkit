//! rightkit-spend: a spend ledger on a real SQLite file: an allowance, a reservation within it, an over-allowance reservation
//! refused, a replay recognised, the commit settling the actual cost, and the remaining amount read back after reopening.
use rightkit_spend::rusqlite::Connection;
use rightkit_spend::Ledger;
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("spend")?;
    let db = dir.join("spend.sqlite");
    let mut ledger = Ledger::attach(Connection::open(&db).map_err(|e| e.to_string())?).map_err(|e| format!("attach: {e}"))?;
    ledger.set_allowance("account", "day", 1000, None).map_err(|e| format!("set allowance: {e}"))?;
    let first = ledger.reserve("account", "req-1", "day", "route", "price-v1", 300).map_err(|e| format!("reserve: {e}"))?;
    let replay = ledger.reserve("account", "req-1", "day", "route", "price-v1", 300).map_err(|e| format!("replay: {e}"))?;
    if !replay.replayed || replay.reservation.reservation_id != first.reservation.reservation_id {
        return Err("a repeated request id was not recognised as a replay".into());
    }
    let over = ledger.reserve("account", "req-2", "day", "route", "price-v1", 800).is_err();
    if !over {
        return Err("a reservation beyond the remaining allowance was admitted".into());
    }
    ledger.commit(&first.reservation.reservation_id, 250).map_err(|e| format!("commit: {e}"))?;
    drop(ledger);
    let ledger = Ledger::attach(Connection::open(&db).map_err(|e| e.to_string())?).map_err(|e| format!("reattach: {e}"))?;
    let allowance = ledger.allowance("account", "day").map_err(|e| format!("allowance: {e}"))?.ok_or("the allowance is gone after reopening")?;
    if allowance.remaining_micro() != 750 || allowance.committed_micro != 250 {
        return Err(format!("after committing 250 of 1000: committed {}, remaining {}", allowance.committed_micro, allowance.remaining_micro()));
    }
    Ok(json!({ "committed": allowance.committed_micro, "remaining": allowance.remaining_micro(), "over_refused": over, "replayed": true }))
}
