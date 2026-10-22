// Supplemental PK14 compatibility consumer; public shim paths only.
// No desktop input, processes, sockets, files, or optional features.
use rightkit_qa_driver::admission::{
    AdmissionDecision, CancelToken, EffectAdmissionStage, EffectGate, EffectKind, EffectRequest,
    SettleOutcome,
};
use rightkit_qa_driver::events::{ControlEvent, EventSink};
use rightkit_qa_driver::lease::{InputLease, InputOutcome, MemoryLeaseStore};
use rightkit_qa_driver::{json, keys, offscreen};
use std::cell::Cell;
use std::sync::{Arc, Mutex};

fn denied_effect_never_executes() {
    let events: Arc<Mutex<Vec<ControlEvent>>> = Arc::default();
    let captured = events.clone();
    let sink: EventSink = Arc::new(move |event: &ControlEvent| {
        captured.lock().unwrap().push(event.clone());
    });
    let gate = EffectGate::new(|_: &EffectRequest| AdmissionDecision::deny("consumer denial"))
        .with_events(sink);
    let executed = Cell::new(false);
    let effect = gate.admit(
        EffectRequest::new(EffectKind::Input, "click", "consumer"),
        &CancelToken::new(),
        |_| {
            executed.set(true);
            Ok(())
        },
    );

    assert!(!executed.get(), "denied executor must never be invoked");
    assert_eq!(effect.settlement.outcome, SettleOutcome::Denied);
    assert!(!effect.settlement.executed);
    assert!(!effect.settlement.crashed);
    assert_eq!(effect.value, None);
    assert_eq!(effect.settlement.reason.as_deref(), Some("consumer denial"));
    assert_eq!(
        effect.settlement.stages.last(),
        Some(&EffectAdmissionStage::Settle)
    );
    assert!(!effect
        .settlement
        .stages
        .contains(&EffectAdmissionStage::Execute));
    assert_eq!(gate.settlements(), vec![effect.settlement.clone()]);
    assert_eq!(
        *events.lock().unwrap(),
        vec![ControlEvent::EffectDenied {
            id: effect.settlement.id,
            kind: EffectKind::Input,
            method: "click".into(),
            stage: EffectAdmissionStage::Approval,
            reason: "consumer denial".into(),
        }]
    );
}

fn current_lease_sequence_applies() {
    let lease = InputLease::new("registry-consumer");
    let grant = lease.acquire().expect("acquire in-memory lease");
    let calls = Cell::new(0);
    let first = lease
        .submit(grant.epoch, grant.next_input_sequence, || {
            calls.set(calls.get() + 1);
            Ok(5)
        })
        .expect("apply granted sequence");
    assert!(matches!(first, InputOutcome::Applied { value: 5, .. }));
    assert_eq!(calls.get(), 1);
    assert_eq!(first.ack().epoch, grant.epoch);
    assert_eq!(first.ack().sequence, grant.next_input_sequence);

    let next = lease.snapshot().next_input_sequence;
    assert_eq!(next, grant.next_input_sequence + 1);
    let second = lease
        .submit(grant.epoch, next, || {
            calls.set(calls.get() + 1);
            Ok(6)
        })
        .expect("apply next current sequence");
    assert!(matches!(second, InputOutcome::Applied { value: 6, .. }));
    assert_eq!(calls.get(), 2);
    assert_eq!(second.ack().epoch, grant.epoch);
    assert_eq!(second.ack().sequence, next);
    assert_eq!(lease.snapshot().next_input_sequence, next + 1);

    let store = MemoryLeaseStore::default();
    assert!(InputLease::open("registry-memory-store", Arc::new(store)).is_ok());
}

fn helpers_keep_behavior() {
    assert_eq!(json::esc("a\"b"), "a\\\"b");
    assert_eq!(json::str_field(r#"{"k":"v"}"#, "k").as_deref(), Some("v"));
    assert_eq!(json::base64_encode(b"foo"), "Zm9v");
    assert_eq!(keys::chord("cmd+a"), Some((0, keys::FLAG_CMD)));
    assert_eq!(keys::chord("delete"), Some((0x33, 0)));
    assert_eq!(offscreen::ENV_OFFSCREEN, "RIGHTKIT_OFFSCREEN");
}

fn main() {
    denied_effect_never_executes();
    current_lease_sequence_applies();
    helpers_keep_behavior();
    println!("rightkit-qa-driver 0.7.0 registry consumer passed");
}
