//! The generic input code now lives in `rightkit-automation`; every path this crate used to
//! define must still resolve to it, with the same behaviour. Compile-and-behaviour pin only:
//! no real app, window or user folder is touched.

use rightkit_qa_driver::admission::{
    AdmissionDecision, CancelToken, EffectGate, EffectKind, EffectRequest, SettleOutcome,
};
use rightkit_qa_driver::events::{ControlEvent, EventSink};
use rightkit_qa_driver::lease::{InputLease, InputOutcome, MemoryLeaseStore};
use rightkit_qa_driver::{json, keys, offscreen};
use std::sync::{Arc, Mutex};

#[test]
fn the_gate_is_the_automation_gate() {
    let log: Arc<Mutex<Vec<ControlEvent>>> = Arc::default();
    let l = log.clone();
    let sink: EventSink = Arc::new(move |e: &ControlEvent| l.lock().unwrap().push(e.clone()));
    let gate =
        EffectGate::new(|_: &EffectRequest| AdmissionDecision::deny("no")).with_events(sink);
    let e = gate.admit(
        EffectRequest::new(EffectKind::Input, "click", "main"),
        &CancelToken::new(),
        |_| Ok(()),
    );
    assert_eq!(e.settlement.outcome, SettleOutcome::Denied);
    assert!(!e.settlement.executed);
    assert_eq!(log.lock().unwrap().len(), 1);
}

#[test]
fn the_lease_is_the_automation_lease() {
    let lease = InputLease::new("reexports");
    let grant = lease.acquire().unwrap();
    let out = lease
        .submit(grant.epoch, grant.next_input_sequence, || Ok(5))
        .unwrap();
    assert!(matches!(out, InputOutcome::Applied { value: 5, .. }));
    let store = MemoryLeaseStore::default();
    assert!(InputLease::open("reexports-durable", Arc::new(store)).is_ok());
}

#[test]
fn small_helpers_keep_their_paths() {
    assert_eq!(json::esc("a\"b"), "a\\\"b");
    assert_eq!(json::str_field(r#"{"k":"v"}"#, "k").as_deref(), Some("v"));
    assert_eq!(json::base64_encode(b"foo"), "Zm9v");
    assert_eq!(keys::chord("cmd+a"), Some((0, keys::FLAG_CMD)));
    assert_eq!(keys::chord("delete"), Some((0x33, 0)), "Delete stays backspace");
    assert_eq!(offscreen::ENV_OFFSCREEN, "RIGHTKIT_OFFSCREEN");
}
