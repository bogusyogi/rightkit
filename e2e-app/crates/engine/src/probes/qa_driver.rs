//! rightkit-qa-driver 0.5.0 is a deprecated shim over rightkit-automation (and rightkit-qa under `qa-control`). The
//! probe proves the old paths still resolve to working code, not just that they name something: the EFF-001 effect
//! gate (admission, kill switch, settlement), the PTY-002 input lease with its durable file store surviving a
//! "crash" (drop and reopen), the old `json` and `keys` paths. The `offscreen` and `webdriver` paths run through their
//! pure parts; launching a real off-screen window and driving a browser need an app-level scenario.
use std::sync::Arc;

use rightkit_qa_driver::admission::{
    AdmissionDecision, CancelToken, EffectGate, EffectKind, EffectRequest, SettleOutcome,
};
use rightkit_qa_driver::lease::{FileLeaseStore, InputLease};
use serde_json::{json, Value};

fn ensure(ok: bool, what: impl FnOnce() -> String) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(what())
    }
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    // ---- the old `offscreen` path: placement math for a window outside every display
    use rightkit_qa_driver::offscreen::{display_union, offscreen_frame, Rect};
    let displays = [
        Rect::new(0.0, 0.0, 1920.0, 1080.0),
        Rect::new(1920.0, 0.0, 1280.0, 1024.0),
    ];
    let union = display_union(&displays).ok_or("display_union of two displays is None")?;
    ensure(union == Rect::new(0.0, 0.0, 3200.0, 1080.0), || {
        format!("display union is {union:?}")
    })?;
    let frame = offscreen_frame(&displays, (800.0, 600.0))
        .map_err(|e| format!("offscreen_frame: {e:?}"))?;
    ensure(
        frame.x >= union.right() && displays.iter().all(|d| !frame.intersects(d)),
        || format!("the off-screen frame {frame:?} touches a display"),
    )?;
    ensure(
        offscreen_frame(&displays, (0.0, 600.0)).is_err()
            && offscreen_frame(&[], (1.0, 1.0)).is_err(),
        || "an invalid size or no displays was accepted".into(),
    )?;
    // ---- the old `webdriver` path
    ensure(
        rightkit_qa_driver::webdriver::b64(b"hello") == "aGVsbG8=",
        || "webdriver::b64 is wrong".into(),
    )?;

    // ---- EFF-001 gate: an effect runs only when validation and the hook both admit it.
    let gate = EffectGate::new(|req: &EffectRequest| {
        if req.target.contains("forbidden") {
            AdmissionDecision::deny("target is forbidden")
        } else {
            AdmissionDecision::Allow
        }
    });
    let cancel = CancelToken::new();
    let ran = std::sync::atomic::AtomicU32::new(0);
    let ok = gate.admit(
        EffectRequest::new(EffectKind::Input, "click", "window:main"),
        &cancel,
        |_| {
            ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(42)
        },
    );
    ensure(ok.is_ok() && ok.value == Some(42), || {
        format!("an admitted effect did not run: {:?}", ok.settlement)
    })?;
    let denied = gate.admit(
        EffectRequest::new(EffectKind::Input, "click", "window:forbidden"),
        &cancel,
        |_| {
            ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(1)
        },
    );
    ensure(
        denied.settlement.outcome == SettleOutcome::Denied && !denied.settlement.executed,
        || format!("a denied effect was not Denied: {:?}", denied.settlement),
    )?;
    let empty = gate.admit(
        EffectRequest::new(EffectKind::Input, " ", "window:main"),
        &cancel,
        |_| Ok(2),
    );
    ensure(empty.settlement.outcome == SettleOutcome::Denied, || {
        "an effect with no method was admitted".into()
    })?;
    ensure(ran.load(std::sync::atomic::Ordering::SeqCst) == 1, || {
        "an executor ran for a denied effect".into()
    })?;
    ensure(gate.settlements().len() == 3, || {
        format!(
            "the gate settled {} effects, expected 3",
            gate.settlements().len()
        )
    })?;

    // ---- PTY-002 lease on a durable file store.
    let dir = ctx.dir("qa-driver")?;
    let path = dir.join("lease.json");
    let first = InputLease::open("e2e", Arc::new(FileLeaseStore::new(&path)))
        .map_err(|e| format!("open durable lease: {e}"))?;
    ensure(first.is_durable(), || {
        "a lease on a file store is not durable".into()
    })?;
    let grant = first.acquire().map_err(|e| format!("acquire: {e}"))?;
    let seq = grant.next_input_sequence;
    let applied = first
        .submit(grant.epoch, seq, || {
            Ok::<_, rightkit_qa_driver::lease::InputFailure>("typed")
        })
        .map_err(|e| format!("submit: {e}"))?;
    ensure(format!("{applied:?}").contains("Applied"), || {
        format!("a fresh input was {applied:?}")
    })?;
    let again = first
        .submit(grant.epoch, seq, || {
            Ok::<_, rightkit_qa_driver::lease::InputFailure>("typed twice")
        })
        .map_err(|e| format!("resubmit: {e}"))?;
    ensure(format!("{again:?}").contains("Duplicate"), || {
        format!("a replayed sequence was {again:?}, expected Duplicate")
    })?;
    let skipped = first.submit(grant.epoch, seq + 5, || {
        Ok::<_, rightkit_qa_driver::lease::InputFailure>(())
    });
    ensure(skipped.is_err(), || {
        "an out-of-order sequence was executed".into()
    })?;
    let rival = first
        .acquire()
        .map_err(|e| format!("second acquire: {e}"))?;
    ensure(
        rival.epoch == grant.epoch + 1 && rival.fenced_epoch == Some(grant.epoch),
        || format!("takeover grant was {rival:?}"),
    )?;
    let stale = first.submit(grant.epoch, seq + 1, || {
        Ok::<_, rightkit_qa_driver::lease::InputFailure>(())
    });
    ensure(
        stale.is_err() && format!("{stale:?}").contains("StaleEpoch"),
        || format!("a fenced epoch was not StaleEpoch: {stale:?}"),
    )?;
    ensure(path.exists(), || "the durable store wrote no file".into())?;

    // "Crash": drop without release, reopen from the file. The epoch only moves forward and the holder is detached.
    drop(first);
    let reopened = InputLease::open("e2e", Arc::new(FileLeaseStore::new(&path)))
        .map_err(|e| format!("reopen durable lease: {e}"))?;
    let snap = reopened.snapshot();
    ensure(snap.epoch >= rival.epoch && !snap.attached, || {
        format!("recovered lease state is {snap:?}")
    })?;
    let wrong_name = InputLease::open("someone-else", Arc::new(FileLeaseStore::new(&path)));
    ensure(wrong_name.is_err(), || {
        "a lease record for another lease name was accepted".into()
    })?;
    let regrant = reopened.acquire();
    let epoch_after = match regrant {
        Ok(g) => g.epoch,
        // Unacknowledged or unknown input after a crash must be reconciled explicitly, never silently.
        Err(_) => {
            reopened
                .try_acquire_reconciling(
                    rightkit_qa_driver::lease::UnacknowledgedInputDecision::ReconcileAsLost,
                )
                .map_err(|e| format!("reconcile after crash: {e}"))?
                .epoch
        }
    };
    ensure(epoch_after > rival.epoch, || {
        format!(
            "the epoch after recovery ({epoch_after}) did not move past {}",
            rival.epoch
        )
    })?;

    // ---- the old json and keys paths
    ensure(
        rightkit_qa_driver::json::s("a\"b\n") == "\"a\\\"b\\n\"",
        || "json::s escaping is wrong".into(),
    )?;
    ensure(
        rightkit_qa_driver::keys::chord("cmd+a").is_some()
            && rightkit_qa_driver::keys::chord("cmd+nonsense-key").is_none(),
        || "keys::appkit::chord is wrong".into(),
    )?;

    Ok(
        json!({ "gate_settled": 3, "epoch_after_recovery": epoch_after, "durable_file": path.exists() }),
    )
}
